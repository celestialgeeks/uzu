use tauri::{AppHandle, Manager};
use uzu::{
    engine::Engine,
    session::{
        chat::{ChatInstance, ChatSession},
        tool::uzu_tool_function,
    },
    types::session::chat::ChatConfig,
};

use super::{
    ChatState, ResidentSession,
    chart::show_chart,
    naming::ChatNaming,
    session_events::{LoadingStatus, emit_session_loading, emit_session_state},
};
use crate::{
    engine::engine,
    error::{AppError, AppResult},
    models::{ReasoningSupport, find_chat_model, reasoning_support},
};

pub(super) fn arm_idle_timer(
    app: &AppHandle,
    state: &ChatState,
    repo_id: &str,
) {
    let (enabled, idle_ms) = {
        let cfg = state.idle_config.lock().expect("idle_config mutex poisoned");
        (cfg.enabled, cfg.idle_ms)
    };
    state.cancel_idle_timer();
    if !enabled {
        return;
    }
    let app = app.clone();
    let repo_id = repo_id.to_string();
    let handle = tauri::async_runtime::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(idle_ms)).await;
        let state = app.state::<ChatState>();
        state.evict(&app, &repo_id).await;
    });
    *state.idle_timer.lock().expect("idle_timer mutex poisoned") = Some(handle);
}

pub(super) async fn ensure_session(
    app: &AppHandle,
    state: &ChatState,
    repo_id: &str,
    model_chat_naming_enabled: Option<bool>,
    date_time_tool_enabled: Option<bool>,
    chart_tool_enabled: Option<bool>,
    web_search_tool_enabled: Option<bool>,
) -> AppResult<(ChatSession, ReasoningSupport, Option<ChatNaming>)> {
    let model = find_chat_model(repo_id).await?;
    let model_size = model.properties.as_ref().map(|properties| properties.size);
    let settings = crate::storage::settings_load()?;
    let model_chat_naming_enabled =
        chat_naming_enabled(resolve_tool_enabled(model_chat_naming_enabled, model_size), &settings);
    let date_time_tool_enabled = resolve_tool_enabled(date_time_tool_enabled, model_size);
    let chart_tool_enabled = resolve_tool_enabled(chart_tool_enabled, model_size);
    // The web tools reach outside the machine, so two more gates apply on top of the
    // per-model choice: the global Settings switch, and whether the configured provider
    // can actually run. A provider with no key yet must not offer a tool that always fails.
    let web_search_tool_enabled = global_tool_switch(&settings, "webSearchEnabled")
        && crate::web::config_from_settings(&settings).is_ready()
        && resolve_tool_enabled(web_search_tool_enabled, model_size);
    let engine = engine().await?;
    if model.is_downloadable()
        && !matches!(engine.ready_download_state(&model).await?.phase, uzu::storage::DownloadPhase::Downloaded {})
    {
        return Err(AppError::msg("Model is not downloaded"));
    }
    let mut guard = state.session.lock().await;
    if let Some(resident) = guard.as_ref()
        && resident.identifier == model.identifier
    {
        if resident.model_chat_naming_enabled == model_chat_naming_enabled
            && resident.date_time_tool_enabled == date_time_tool_enabled
            && resident.chart_tool_enabled == chart_tool_enabled
            && resident.web_search_tool_enabled == web_search_tool_enabled
        {
            return Ok((resident.session.clone(), resident.support.clone(), resident.naming.clone()));
        }
        // Tools survive reset(). Drop the old KV state before creating the
        // replacement session, keeping the loaded model weights in the instance.
        let resident = guard.take().expect("resident session");
        drop(resident.session);
        state.forget_history();
        let (session, naming) = match create_session(
            &engine,
            &resident.instance,
            model_chat_naming_enabled,
            date_time_tool_enabled,
            chart_tool_enabled,
            web_search_tool_enabled,
        )
        .await
        {
            Ok(created) => created,
            Err(error) => {
                emit_session_state(app, false, repo_id, false);
                return Err(error);
            },
        };
        let support = resident.support.clone();
        *guard = Some(ResidentSession {
            session: session.clone(),
            naming: naming.clone(),
            model_chat_naming_enabled,
            date_time_tool_enabled,
            chart_tool_enabled,
            web_search_tool_enabled,
            ..resident
        });
        return Ok((session, support, naming));
    }
    let support = reasoning_support(&model);
    *guard = None;
    state.forget_history();
    // Loading events only on the real load path: emitting them on session reuse
    // would flash the spinner and the resident-model chip.
    emit_session_loading(app, LoadingStatus::Start, repo_id, None);
    let config = ChatConfig::create();
    let loaded = async {
        let instance = engine.chat_instance(model.clone(), config).await?;
        let (session, naming) = create_session(
            &engine,
            &instance,
            model_chat_naming_enabled,
            date_time_tool_enabled,
            chart_tool_enabled,
            web_search_tool_enabled,
        )
        .await?;
        Ok::<_, AppError>((instance, session, naming))
    }
    .await;
    let (instance, session, naming) = match loaded {
        Ok(loaded) => loaded,
        Err(error) => {
            emit_session_loading(app, LoadingStatus::Error, repo_id, Some(error.to_string()));
            return Err(error);
        },
    };
    *guard = Some(ResidentSession {
        repo_id: repo_id.to_string(),
        identifier: model.identifier.clone(),
        session: session.clone(),
        instance,
        model_chat_naming_enabled,
        date_time_tool_enabled,
        chart_tool_enabled,
        web_search_tool_enabled,
        naming: naming.clone(),
        support: support.clone(),
    });
    emit_session_state(app, true, repo_id, false);
    Ok((session, support, naming))
}

fn resolve_tool_enabled(
    requested: Option<bool>,
    model_size: Option<i64>,
) -> bool {
    requested.unwrap_or_else(|| model_size.is_none_or(|size| size >= 2_000_000_000))
}

fn chat_naming_enabled(
    requested: bool,
    settings: &serde_json::Value,
) -> bool {
    requested && settings.get("modelChatNamingEnabled").and_then(serde_json::Value::as_bool) != Some(false)
}

/// A global Settings switch that defaults to enabled: only an explicit `false` disables it,
/// so a settings file written before the feature existed keeps today's behaviour.
fn global_tool_switch(
    settings: &serde_json::Value,
    key: &str,
) -> bool {
    settings.get(key).and_then(serde_json::Value::as_bool) != Some(false)
}

async fn create_session(
    engine: &Engine,
    instance: &ChatInstance,
    model_chat_naming_enabled: bool,
    date_time_tool_enabled: bool,
    chart_tool_enabled: bool,
    web_search_tool_enabled: bool,
) -> AppResult<(ChatSession, Option<ChatNaming>)> {
    let mut session = engine.chat_with_instance(instance).await?;
    let supports_tools = session.supports_tool_calls().await;
    if date_time_tool_enabled && supports_tools {
        session.add_tool(get_current_date_time).await?;
    }
    if chart_tool_enabled && supports_tools {
        session.add_tool(show_chart).await?;
    }
    if web_search_tool_enabled && supports_tools {
        // Registered as a pair: a model offered `web_search` but not `fetch_url` answers
        // from snippets alone, which is the half of the feature that actually grounds it.
        for tool in crate::web::tools::descriptors() {
            session.add_tool(tool).await?;
        }
    }
    let naming = if model_chat_naming_enabled && supports_tools {
        let naming = ChatNaming::default();
        session.add_tool(naming.tool()).await?;
        Some(naming)
    } else {
        None
    };
    Ok((session, naming))
}

/// Returns the current date and time in UTC and the device's local time, with explicit UTC offsets.
#[uzu_tool_function]
fn get_current_date_time() -> String {
    let now = chrono::Local::now();
    format!(
        "Current UTC date and time: {}.\nDevice local date and time: {}.",
        now.with_timezone(&chrono::Utc).format("%A, %-d %B %Y at %H:%M:%S (UTC%:z)"),
        now.format("%A, %-d %B %Y at %H:%M:%S (UTC%:z)"),
    )
}

#[cfg(test)]
mod tests {
    use uzu::session::tool::func_def::ToolDescriptor;

    use super::*;

    #[test]
    fn tool_defaults_follow_model_size_without_overriding_explicit_choices() {
        for (size, default_enabled) in
            [(Some(1_999_999_999), false), (Some(2_000_000_000), true), (Some(27_000_000_000), true), (None, true)]
        {
            assert_eq!(resolve_tool_enabled(None, size), default_enabled);
            assert!(resolve_tool_enabled(Some(true), size));
            assert!(!resolve_tool_enabled(Some(false), size));
        }
    }

    #[test]
    fn global_naming_switch_overrides_request_without_overriding_per_model_disable() {
        let mut settings = serde_json::json!({
            "modelChatNamingEnabled": false,
            "modelParams": {"test/model": {"modelChatNamingEnabled": true}},
        });
        let explicit_small_model_opt_in = resolve_tool_enabled(Some(true), Some(1_200_000_000));
        assert!(!chat_naming_enabled(explicit_small_model_opt_in, &settings));

        settings["modelChatNamingEnabled"] = true.into();
        assert!(chat_naming_enabled(explicit_small_model_opt_in, &settings));
        assert!(!chat_naming_enabled(false, &settings));
        assert!(chat_naming_enabled(true, &serde_json::json!({})));
    }

    #[tokio::test]
    async fn date_time_tool_returns_the_same_current_instant_in_utc_and_local_time() {
        let tool: ToolDescriptor = get_current_date_time.into();
        assert_eq!(tool.name, "get_current_date_time");
        let before = chrono::Local::now();
        let result = tool.execute(serde_json::json!({}).into()).await.unwrap();
        let after = chrono::Local::now();
        let value: serde_json::Value = result.try_into().unwrap();
        let (utc, local) = value.as_str().unwrap().split_once('\n').unwrap();
        let utc =
            chrono::DateTime::parse_from_str(utc, "Current UTC date and time: %A, %-d %B %Y at %H:%M:%S (UTC%:z).")
                .unwrap();
        let local =
            chrono::DateTime::parse_from_str(local, "Device local date and time: %A, %-d %B %Y at %H:%M:%S (UTC%:z).")
                .unwrap();
        assert_eq!(utc, local);
        assert!(utc.timestamp() >= before.timestamp() && utc.timestamp() <= after.timestamp());
        assert_eq!(utc.offset().local_minus_utc(), 0);
        assert_eq!(local.offset(), local.with_timezone(&chrono::Local).offset());
    }

    #[test]
    fn web_search_requires_the_global_switch_a_usable_provider_and_the_model_floor() {
        let blank = serde_json::json!({});
        let switched_off = serde_json::json!({ "webSearchEnabled": false });
        let provider_without_key = serde_json::json!({ "webSearchProvider": "brave" });
        let provider_with_key = serde_json::json!({ "webSearchProvider": "brave", "webSearchApiKey": "token" });

        // A fresh install is keyless DuckDuckGo, so search works before any configuration.
        assert!(global_tool_switch(&blank, "webSearchEnabled"));
        assert!(crate::web::config_from_settings(&blank).is_ready());
        // An explicit global off switch is honoured, and a provider with no credential yet
        // must not be offered at all: the model would only learn by failing mid-answer.
        assert!(!global_tool_switch(&switched_off, "webSearchEnabled"));
        assert!(!crate::web::config_from_settings(&provider_without_key).is_ready());
        assert!(crate::web::config_from_settings(&provider_with_key).is_ready());
        // Only an explicit false disables it, matching the naming switch's leniency.
        assert!(global_tool_switch(&serde_json::json!({ "webSearchEnabled": "yes" }), "webSearchEnabled"));
        assert!(!resolve_tool_enabled(None, Some(1_000_000_000)), "sub-2B models stay tool-free");
    }
}
