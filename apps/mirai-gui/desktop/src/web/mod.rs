//! Built-in web tools for the chat: `web_search` and `fetch_url`.
//!
//! Design contract, borrowed from oMLX's `omlx/websearch.py`:
//!
//! * Tools never raise. They answer `{"ok": bool, ...}` so the model can read a
//!   failure and relay something useful instead of hallucinating results.
//! * Every number that bounds a request is a constant in [`types`], not a
//!   setting, except result count and content length, which the user tunes.
//! * `fetch_url` is the safety-critical half: it validates the URL, refuses
//!   non-public addresses, and re-validates every redirect hop by hand.
//! * Search results are untrusted text. The tool descriptions say so, because a
//!   retrieved page must never be able to steer the model into a tool call.
//!
//! Both tools run in Rust, not the webview: `tauri.conf.json` sets
//! `connect-src 'self' ipc: http://ipc.localhost`, so the frontend cannot fetch
//! arbitrary HTTPS hosts and must not be able to.

pub mod fetch;
pub mod providers;
pub mod search;
pub mod tools;
pub mod types;

use std::sync::OnceLock;

use reqwest::{Client, redirect::Policy};
use serde_json::Value;
/// The vocabulary every web submodule shares. Kept to the names actually referenced
/// through `super::` so the private module gains no unused re-exports.
pub use types::{ContentMode, ErrorCode, ProviderKind, WebConfig, WebFailure};

use crate::storage::settings_load;

/// One shared HTTP client for search requests. Redirects are followed normally
/// here because search endpoints are fixed, well-known hosts. [`fetch`] builds
/// its own client with redirects disabled so it can validate every hop itself.
pub(crate) fn search_client() -> &'static Client {
    static CLIENT: OnceLock<Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        Client::builder()
            .timeout(crate::web::types::HTTP_TIMEOUT)
            .user_agent(concat!("Mirai/", env!("CARGO_PKG_VERSION")))
            .build()
            .expect("reqwest client")
    })
}

/// A client that will not follow redirects, for [`fetch::run_fetch_url`].
pub(crate) fn fetch_client() -> &'static Client {
    static CLIENT: OnceLock<Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        Client::builder()
            .timeout(crate::web::types::HTTP_TIMEOUT)
            .redirect(Policy::none())
            .user_agent(concat!("Mirai/", env!("CARGO_PKG_VERSION")))
            .build()
            .expect("reqwest client")
    })
}

/// Read the web-search settings, tolerating absent or malformed keys: `settings.json`
/// is an untyped merge, so every field falls back to a usable default.
///
/// Keys (camelCase, all optional):
/// `webSearchProvider`, `webSearchApiKey`, `webSearchSearxngUrl`, `webSearchMaxResults`,
/// `webSearchContentMode`, `webSearchContentMaxChars`, `webSearchContentTruncate`.
pub fn config_from_settings(settings: &Value) -> WebConfig {
    let field = |name: &str| settings.get(name).and_then(Value::as_str).unwrap_or_default();
    let max_results = settings
        .get("webSearchMaxResults")
        .and_then(|value| {
            value
                .as_u64()
                .map(|count| count as usize)
                .or_else(|| value.as_str().and_then(|raw| raw.trim().parse::<usize>().ok()))
        })
        .unwrap_or(types::DEFAULT_MAX_RESULTS);
    let content_max_chars = settings
        .get("webSearchContentMaxChars")
        .and_then(|value| {
            value
                .as_u64()
                .map(|count| count as usize)
                .or_else(|| value.as_str().and_then(|raw| raw.trim().parse::<usize>().ok()))
        })
        .unwrap_or(types::DEFAULT_FETCH_CONTENT_CHARS);

    WebConfig {
        provider: ProviderKind::from_setting(field("webSearchProvider")),
        api_key: field("webSearchApiKey").trim().to_string(),
        base_url: field("webSearchSearxngUrl").trim().trim_end_matches('/').to_string(),
        max_results: max_results.clamp(1, types::MAX_RESULTS_CAP),
        content_mode: ContentMode::from_setting(field("webSearchContentMode")),
        content_max_chars: if content_max_chars > 0 {
            content_max_chars
        } else {
            types::DEFAULT_FETCH_CONTENT_CHARS
        },
        content_truncate: settings
            .get("webSearchContentTruncate")
            .and_then(|value| value.as_bool().or_else(|| value.as_str().map(|raw| raw != "false")))
            .unwrap_or(true),
    }
}

/// Load settings and resolve the config, so callers never have to handle the
/// storage error path twice. A settings read failure degrades to defaults rather
/// than taking the tool down. Kept synchronous to match `settings_load`, which the
/// rest of the app already calls from async context.
pub fn current_config() -> WebConfig {
    settings_load().map(|settings| config_from_settings(&settings)).unwrap_or_default()
}

/// Shared by both tools: a config that cannot work yet is reported as data, not
/// as an exception, so the model can point the user at Settings.
pub(crate) fn not_ready_failure(config: &WebConfig) -> WebFailure {
    WebFailure::new(ErrorCode::MissingApiKey, config.missing_credential_message(), true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blank_settings_resolve_to_the_keyless_duckduckgo_default() {
        let config = config_from_settings(&Value::Null);
        assert_eq!(config.provider, ProviderKind::Duckduckgo);
        assert_eq!(config.max_results, types::DEFAULT_MAX_RESULTS);
        assert_eq!(config.content_max_chars, types::DEFAULT_FETCH_CONTENT_CHARS);
        assert!(config.content_truncate);
        assert!(config.api_key.is_empty() && config.base_url.is_empty());
    }

    #[test]
    fn settings_are_read_leniently_and_clamped_to_the_documented_bounds() {
        let settings = serde_json::json!({
            "webSearchProvider": "searxng",
            "webSearchSearxngUrl": "https://searx.internal/",
            "webSearchMaxResults": 99,
            "webSearchContentMode": "full",
            "webSearchContentMaxChars": "500",
            "webSearchContentTruncate": false,
        });
        let config = config_from_settings(&settings);
        assert_eq!(config.provider, ProviderKind::Searxng);
        assert_eq!(config.base_url, "https://searx.internal");
        assert_eq!(config.max_results, types::MAX_RESULTS_CAP);
        assert_eq!(config.content_mode, ContentMode::Full);
        assert_eq!(config.content_max_chars, 500);
        assert!(!config.content_truncate);
    }

    #[test]
    fn unusable_values_fall_back_instead_of_disabling_the_tool() {
        let settings = serde_json::json!({
            "webSearchMaxResults": "abc",
            "webSearchContentMaxChars": 0,
        });
        let config = config_from_settings(&settings);
        assert_eq!(config.max_results, types::DEFAULT_MAX_RESULTS);
        assert_eq!(config.content_max_chars, types::DEFAULT_FETCH_CONTENT_CHARS);

        let zero_results = config_from_settings(&serde_json::json!({ "webSearchMaxResults": 0 }));
        assert_eq!(zero_results.max_results, 1, "a zero result count would make the tool useless");
    }

    #[test]
    fn a_missing_credential_is_reported_as_actionable_data() {
        let config = WebConfig {
            provider: ProviderKind::Tavily,
            ..WebConfig::default()
        };
        let failure = not_ready_failure(&config);
        assert_eq!(failure.0.code, ErrorCode::MissingApiKey);
        assert!(failure.0.user_action_required);
        assert!(failure.0.message.contains("Settings"));
    }
}
