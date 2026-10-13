//! Shared types and limits for the built-in web tools.
//!
//! The wire contract deliberately mirrors oMLX's `omlx/websearch.py`: every tool
//! entry point returns an `{"ok": bool, ...}` payload instead of raising, so a
//! small local model can read the failure and tell the user what to do about it.
//! Error codes are part of that contract — the tool descriptions in `tools.rs`
//! teach the model how to handle each one, so adding a code without updating the
//! description leaves the model unable to recover from it.

use std::time::Duration;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Default number of results returned when settings leave the field blank.
pub const DEFAULT_MAX_RESULTS: usize = 3;
/// Hard ceiling on results, regardless of what the user configured.
pub const MAX_RESULTS_CAP: usize = 10;
pub const MAX_QUERY_CHARS: usize = 300;
pub const MAX_URL_CHARS: usize = 2048;
pub const MAX_TITLE_CHARS: usize = 160;
pub const MAX_SNIPPET_CHARS: usize = 500;
pub const HTTP_TIMEOUT: Duration = Duration::from_secs(20);
/// Raw response body ceiling. Always applies, even with truncation disabled.
pub const MAX_RESPONSE_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_REDIRECTS: usize = 3;
pub const DEFAULT_FETCH_CONTENT_CHARS: usize = 20_000;

/// Where search results come from. `Default` is keyless so a fresh install
/// searches without asking the user for credentials.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum ProviderKind {
    /// DuckDuckGo's HTML endpoint. No key, no extra service to run.
    #[default]
    Duckduckgo,
    /// A self-hosted SearXNG instance with the JSON format enabled.
    Searxng,
    /// Brave Search API.
    Brave,
    /// Tavily, which returns already-extracted page content.
    Tavily,
}

impl ProviderKind {
    /// Parse a settings value leniently: unknown or blank input falls back to
    /// the keyless default rather than breaking every tool call.
    pub fn from_setting(raw: &str) -> Self {
        match raw.trim().to_ascii_lowercase().as_str() {
            "searxng" => Self::Searxng,
            "brave" => Self::Brave,
            "tavily" => Self::Tavily,
            _ => Self::Duckduckgo,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Duckduckgo => "duckduckgo",
            Self::Searxng => "searxng",
            Self::Brave => "brave",
            Self::Tavily => "tavily",
        }
    }
}

/// How much page text to attach to each search result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum ContentMode {
    /// Snippets only: one request per search.
    #[default]
    Snippet,
    /// Fetch every result page and inline its readable text. Costs one extra
    /// request per result, so it stays opt-in.
    Full,
}

impl ContentMode {
    pub fn from_setting(raw: &str) -> Self {
        match raw.trim().to_ascii_lowercase().as_str() {
            "full" => Self::Full,
            _ => Self::Snippet,
        }
    }
}

/// Everything the tools need from `settings.json`, resolved to usable values.
#[derive(Debug, Clone, PartialEq)]
pub struct WebConfig {
    pub provider: ProviderKind,
    /// Brave subscription token or Tavily API key, whichever the provider needs.
    pub api_key: String,
    /// SearXNG instance base URL, without a trailing slash.
    pub base_url: String,
    pub max_results: usize,
    pub content_mode: ContentMode,
    pub content_max_chars: usize,
    pub content_truncate: bool,
}

impl Default for WebConfig {
    fn default() -> Self {
        Self {
            provider: ProviderKind::default(),
            api_key: String::new(),
            base_url: String::new(),
            max_results: DEFAULT_MAX_RESULTS,
            content_mode: ContentMode::default(),
            content_max_chars: DEFAULT_FETCH_CONTENT_CHARS,
            content_truncate: true,
        }
    }
}

impl WebConfig {
    /// A provider is usable once its credential exists. Mirrors oMLX's
    /// `webSearchReady()`: the caller should not offer the tool at all when the
    /// configured provider is missing its key.
    pub fn is_ready(&self) -> bool {
        match self.provider {
            ProviderKind::Duckduckgo => true,
            ProviderKind::Searxng => !self.base_url.trim().is_empty(),
            ProviderKind::Brave | ProviderKind::Tavily => !self.api_key.trim().is_empty(),
        }
    }

    pub fn missing_credential_message(&self) -> String {
        match self.provider {
            ProviderKind::Searxng => {
                "No SearXNG instance URL is configured. Add one in Settings, Web Search.".to_string()
            },
            _ => format!("No {} API key is configured. Add one in Settings, Web Search.", self.provider.as_str()),
        }
    }
}

/// A stable error the model can act on.
///
/// The serde names here are the wire contract quoted in the tool descriptions;
/// `#[uzu_tool_function]` also advertises this type as the tool's return
/// schema, so every response type in this module derives `JsonSchema`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    MissingApiKey,
    InvalidAuthentication,
    InsufficientFunds,
    PlanAccess,
    RateLimited,
    ProviderUnavailable,
    InvalidResponse,
    RequestFailed,
    InvalidArguments,
    UnexpectedFailure,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct WebError {
    pub code: ErrorCode,
    pub message: String,
    /// True when no retry will help until the user changes something.
    pub user_action_required: bool,
}

impl WebError {
    pub fn new(
        code: ErrorCode,
        message: impl Into<String>,
        user_action_required: bool,
    ) -> Self {
        Self {
            code,
            message: message.into(),
            user_action_required,
        }
    }
}

/// Raised by providers and fetchers; converted into a [`WebError`] at the tool boundary.
#[derive(Debug, Clone, PartialEq)]
pub struct WebFailure(pub WebError);

impl WebFailure {
    pub fn new(
        code: ErrorCode,
        message: impl Into<String>,
        user_action_required: bool,
    ) -> Self {
        Self(WebError::new(code, message, user_action_required))
    }

    /// Map an upstream HTTP status onto the code set, exactly as oMLX does, so
    /// billing and plan errors read differently from transient ones.
    pub fn from_status(
        status: u16,
        source: &str,
    ) -> Self {
        let (code, action, detail) = match status {
            401 => (
                ErrorCode::InvalidAuthentication,
                true,
                "rejected the configured credential (HTTP 401). Check the key in Settings, Web Search.".to_string(),
            ),
            402 => {
                (ErrorCode::InsufficientFunds, true, "reported the account is out of credits (HTTP 402).".to_string())
            },
            403 => (ErrorCode::PlanAccess, true, "denied access for the current plan (HTTP 403).".to_string()),
            404 => (
                ErrorCode::InvalidResponse,
                true,
                "returned HTTP 404; the endpoint or instance URL looks wrong.".to_string(),
            ),
            429 => (ErrorCode::RateLimited, false, "rate limited the request (HTTP 429).".to_string()),
            _ if status >= 500 => (ErrorCode::ProviderUnavailable, false, format!("is unavailable (HTTP {status}).")),
            _ => (ErrorCode::RequestFailed, false, "the request failed.".to_string()),
        };
        Self::new(code, format!("{source} {detail}"), action)
    }
}

impl From<WebError> for WebFailure {
    fn from(value: WebError) -> Self {
        Self(value)
    }
}

/// One sanitized search hit. `content` is only present in full-content mode.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SearchResult {
    pub title: String,
    pub url: String,
    pub snippet: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_truncated: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_error: Option<String>,
}

/// A raw row straight off a provider, before sanitization. Anything the
/// providers hand back funnels through [`crate::web::search::sanitize_result`].
#[derive(Debug, Clone, PartialEq)]
pub struct RawResult {
    pub title: Option<String>,
    pub url: Option<String>,
    pub snippet: Option<String>,
}

/// `{"ok": true, "provider", "results"}` or `{"ok": false, "error"}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SearchResponse {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub results: Vec<SearchResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<WebError>,
}

impl SearchResponse {
    pub fn success(
        provider: &str,
        results: Vec<SearchResult>,
    ) -> Self {
        Self {
            ok: true,
            provider: Some(provider.to_string()),
            results,
            error: None,
        }
    }

    pub fn failure(error: WebError) -> Self {
        Self {
            ok: false,
            provider: None,
            results: Vec::new(),
            error: Some(error),
        }
    }
}

/// `{"ok": true, "url", "content", "truncated"}` or `{"ok": false, "error"}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct FetchResponse {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub truncated: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<WebError>,
}

impl FetchResponse {
    pub fn success(
        url: String,
        content: String,
        truncated: bool,
    ) -> Self {
        Self {
            ok: true,
            url: Some(url),
            content: Some(content),
            truncated: Some(truncated),
            error: None,
        }
    }

    pub fn failure(error: WebError) -> Self {
        Self {
            ok: false,
            url: None,
            content: None,
            truncated: None,
            error: Some(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_provider_settings_fall_back_to_the_keyless_default() {
        assert_eq!(ProviderKind::from_setting(""), ProviderKind::Duckduckgo);
        assert_eq!(ProviderKind::from_setting(" BRAVE "), ProviderKind::Brave);
        assert_eq!(ProviderKind::from_setting("SeArXnG"), ProviderKind::Searxng);
        assert_eq!(ProviderKind::from_setting("nonsense"), ProviderKind::Duckduckgo);
    }

    #[test]
    fn readiness_only_blocks_providers_missing_their_credential() {
        let mut config = WebConfig::default();
        assert!(config.is_ready(), "duckduckgo must work with a blank install");

        config.provider = ProviderKind::Brave;
        assert!(!config.is_ready());
        config.api_key = "  ".to_string();
        assert!(!config.is_ready(), "whitespace is not a key");
        config.api_key = "token".to_string();
        assert!(config.is_ready());

        config.provider = ProviderKind::Searxng;
        config.api_key.clear();
        assert!(!config.is_ready());
        config.base_url = "https://searx.example".to_string();
        assert!(config.is_ready());
    }

    #[test]
    fn billing_and_plan_errors_ask_for_user_action_while_transient_ones_do_not() {
        for (status, code, action) in [
            (401, ErrorCode::InvalidAuthentication, true),
            (402, ErrorCode::InsufficientFunds, true),
            (403, ErrorCode::PlanAccess, true),
            (429, ErrorCode::RateLimited, false),
            (503, ErrorCode::ProviderUnavailable, false),
            (418, ErrorCode::RequestFailed, false),
        ] {
            let failure = WebFailure::from_status(status, "Brave Search");
            assert_eq!(failure.0.code, code, "status {status}");
            assert_eq!(failure.0.user_action_required, action, "status {status}");
            assert!(failure.0.message.starts_with("Brave Search "));
        }
    }

    #[test]
    fn payloads_match_the_documented_wire_shape() {
        let empty = SearchResponse::success("duckduckgo", vec![]);
        assert_eq!(serde_json::to_value(&empty).unwrap(), serde_json::json!({ "ok": true, "provider": "duckduckgo" }));

        let failed = SearchResponse::failure(WebError::new(ErrorCode::MissingApiKey, "no key", true));
        assert_eq!(
            serde_json::to_value(&failed).unwrap(),
            serde_json::json!({
                "ok": false,
                "error": { "code": "missing_api_key", "message": "no key", "userActionRequired": true }
            })
        );
    }

    #[test]
    fn content_mode_only_enables_full_page_fetching_for_the_explicit_value() {
        assert_eq!(ContentMode::from_setting(""), ContentMode::Snippet);
        assert_eq!(ContentMode::from_setting("FULL"), ContentMode::Full);
        assert_eq!(ContentMode::from_setting("anything else"), ContentMode::Snippet);
    }
}
