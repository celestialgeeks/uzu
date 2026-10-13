//! Search orchestration: bounds, sanitization, optional content enrichment.
//!
//! Providers are untrusted text. A row can carry a `javascript:` href, an
//! unexpected `@` in the authority, or a five-megabyte "snippet", so everything
//! funnels through [`sanitize_result`] before the model reads it. That is also why
//! it takes `Option<String>` fields rather than a typed row: a provider that
//! invents or omits a field can only make a result disappear, never panic.
//!
//! The wire shape is fixed by `types.rs`: both entry points answer
//! [`SearchResponse`], never `Result`. A search that found nothing is `ok: true`
//! with an empty list; only a real failure produces an error code.

use url::Url;

use super::{
    ContentMode,
    providers::provider_search,
    search_client,
    types::{
        ErrorCode, MAX_QUERY_CHARS, MAX_RESULTS_CAP, MAX_SNIPPET_CHARS, MAX_TITLE_CHARS, MAX_URL_CHARS, SearchResponse,
        SearchResult, WebConfig, WebFailure,
    },
};

/// Query used by the Settings "Test" button. Specific enough that no provider
/// can serve it from a cache of popular searches, and generic enough that it
/// cannot surface the user's own pages.
const SEARCH_TEST_QUERY: &str = "mirai web search connectivity check";

/// Normalize one provider row, or drop it entirely by returning `None`.
///
/// Only `http`/`https` URLs that have a host and no embedded credentials may
/// survive, so `javascript:`/`file:`/`data:` links and credential-bearing URLs
/// never reach the model. Title and snippet are length-capped.
pub fn sanitize_result(
    title: Option<String>,
    url: Option<String>,
    snippet: Option<String>,
) -> Option<SearchResult> {
    let raw = url
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty() && value.chars().count() <= MAX_URL_CHARS)?;
    let parsed = Url::parse(&raw).ok()?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return None;
    }
    // WHATWG parsing promotes an empty authority into a host, so `http:///malformed`
    // parses as the host "malformed". A row with nothing between `://` and the path is
    // broken input, not a page: drop it rather than handing the model a link to nowhere.
    if raw.split_once("://").is_some_and(|(_, rest)| rest.starts_with('/')) {
        return None;
    }
    let host = parsed.host_str().filter(|host| !host.is_empty())?;
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return None;
    }
    // An untitled hit is still a hit: the host names it better than a blank does.
    let title = cap(title.unwrap_or_default(), MAX_TITLE_CHARS);
    let title = if title.is_empty() {
        host.to_string()
    } else {
        title
    };
    Some(SearchResult {
        title,
        url: raw,
        snippet: cap(snippet.unwrap_or_default(), MAX_SNIPPET_CHARS),
        content: None,
        content_truncated: None,
        content_error: None,
    })
}

/// The `web_search` tool body. Never returns `Err`: failures come back as
/// `{"ok": false, "error": {...}}`.
pub async fn run_web_search(
    config: &WebConfig,
    query: &str,
) -> SearchResponse {
    execute_search(config, query, config.content_mode).await
}

/// Validate pending settings with one real, canned query (snippet mode only).
/// Used by the Settings > Web Search "Test" button.
///
/// Snippet mode deliberately, so saving a key costs one request instead of one
/// request per result, and the configured result count still applies so the
/// button proves the whole settings pair, not just the credential.
pub async fn run_search_test(config: &WebConfig) -> SearchResponse {
    execute_search(config, SEARCH_TEST_QUERY, ContentMode::Snippet).await
}

/// The shared pipeline: bound the query, refuse an unusable config, query the
/// provider, sanitize, then optionally expand each hit to full page content.
async fn execute_search(
    config: &WebConfig,
    query: &str,
    content_mode: ContentMode,
) -> SearchResponse {
    let Some(query) = prepare_query(query) else {
        return SearchResponse::failure(invalid_query_failure().0);
    };
    if !config.is_ready() {
        return SearchResponse::failure(super::not_ready_failure(config).0);
    }
    let max_results = config.max_results.min(MAX_RESULTS_CAP);

    let raw = match provider_search(config, search_client(), &query, max_results).await {
        Ok(raw) => raw,
        Err(failure) => return SearchResponse::failure(failure.0),
    };

    let results = raw
        .into_iter()
        .filter_map(|row| sanitize_result(row.title, row.url, row.snippet))
        .take(max_results)
        .collect::<Vec<_>>();
    let results = match content_mode {
        ContentMode::Snippet => results,
        ContentMode::Full => with_page_content(config, results).await,
    };
    SearchResponse::success(config.provider.as_str(), results)
}

/// Trim a query and cap it, or reject it when it holds nothing to search for.
///
/// Pure so the empty-query contract is testable without touching a socket.
fn prepare_query(query: &str) -> Option<String> {
    let trimmed = query.trim();
    (!trimmed.is_empty()).then(|| trimmed.chars().take(MAX_QUERY_CHARS).collect())
}

fn invalid_query_failure() -> WebFailure {
    WebFailure::new(ErrorCode::InvalidArguments, "web_search needs a non-empty 'query' string.", false)
}

/// Attach readable page text to every hit, all fetches in flight at once.
///
/// Content enrichment must never sink a search that already worked: `run_fetch_url`
/// answers with data instead of raising, and a failed or empty page only marks its
/// own row, leaving the snippet intact for the model to cite.
async fn with_page_content(
    config: &WebConfig,
    results: Vec<SearchResult>,
) -> Vec<SearchResult> {
    futures::future::join_all(results.into_iter().map(|result| async move {
        let mut result = result;
        // Cloned so no borrow of `result` is held across the await point.
        let url = result.url.clone();
        let fetched = super::fetch::run_fetch_url(config, &url).await;
        let content = fetched.content.unwrap_or_default();
        if fetched.ok && !content.is_empty() {
            result.content = Some(content);
            result.content_truncated = Some(fetched.truncated.unwrap_or(false));
        } else if fetched.ok {
            result.content_error = Some("The page had no readable text.".to_string());
        } else {
            result.content_error =
                fetched.error.map(|error| error.message).or_else(|| Some("Fetching the page failed.".to_string()));
        }
        result
    }))
    .await
}

/// Trim and truncate to at most `max_chars` characters, so the count is what the
/// constants promise and no multi-byte character is ever cut in half.
fn cap(
    value: String,
    max_chars: usize,
) -> String {
    let trimmed = value.trim();
    trimmed.chars().take(max_chars).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::web::types::{ProviderKind, SearchResult, WebError};

    #[test]
    fn only_plain_http_pages_survive_sanitization() {
        let result = sanitize_result(Some(" Example ".into()), Some(" https://example.com/a#b ".into()), None).unwrap();
        assert_eq!(result.title, "Example", "titles are trimmed");
        assert_eq!(result.url, "https://example.com/a#b", "surrounding space is not part of the url");
        assert_eq!(result.snippet, "");
        assert_eq!(result.content, None, "content is only set in full mode");

        for url in [
            "javascript:alert(1)",
            "JavaScript:alert(1)",
            "file:///etc/passwd",
            "data:text/html,<script>alert(1)</script>",
            "ftp://example.com/pub",
            "mailto:someone@example.com",
            "http://user:password@example.com/private",
            "http://user@example.com/private",
            "https://example.com",
        ] {
            let outcome = sanitize_result(Some("t".into()), Some(url.into()), Some("s".into()));
            if url == "https://example.com" {
                assert!(outcome.is_some(), "{url} is a legitimate hit");
            } else {
                assert!(outcome.is_none(), "{url} must never reach the model");
            }
        }
        assert!(sanitize_result(None, None, None).is_none(), "a row with no url is nothing");
        assert!(sanitize_result(None, Some("".into()), None).is_none());
        assert!(sanitize_result(None, Some("    ".into()), None).is_none(), "whitespace is no url");
        assert!(sanitize_result(None, Some("example.com/no-scheme".into()), None).is_none());
        assert!(sanitize_result(None, Some("http:///path-only".into()), None).is_none(), "no host, no hit");
    }

    #[test]
    fn oversized_fields_are_measured_in_characters_not_bytes() {
        let long = "https://example.com/";
        let padded = format!("{long}{}", "a".repeat(MAX_URL_CHARS - long.chars().count()));
        assert_eq!(padded.chars().count(), MAX_URL_CHARS);
        assert!(sanitize_result(None, Some(padded.clone()), None).is_some(), "the limit is inclusive");
        assert!(sanitize_result(None, Some(format!("{padded}a")), None).is_none(), "one over drops the row");

        let wide = "字🦀".repeat(MAX_TITLE_CHARS);
        let result = sanitize_result(Some(wide), Some("https://example.com".into()), None).unwrap();
        assert_eq!(result.title.chars().count(), MAX_TITLE_CHARS, "titles cap at {MAX_TITLE_CHARS} characters");
        let long_snippet = "🦀".repeat(MAX_SNIPPET_CHARS + 90);
        let result = sanitize_result(None, Some("https://example.com".into()), Some(long_snippet)).unwrap();
        assert_eq!(result.snippet.chars().count(), MAX_SNIPPET_CHARS);
    }

    #[test]
    fn untitled_hits_are_named_by_their_host() {
        let titles: [Option<String>; 3] = [None, Some("".into()), Some("   ".into())];
        for title in titles {
            let result = sanitize_result(title, Some("https://Docs.Example.com/a".into()), None).unwrap();
            assert_eq!(result.title, "docs.example.com", "the url crate normalizes the host");
        }
        let result = sanitize_result(None, Some("https://example.com:8443/a".into()), None).unwrap();
        assert_eq!(result.title, "example.com", "the port is not part of the name");
    }

    #[test]
    fn sanitized_rows_serialize_with_the_documented_camel_case_shape() {
        let result = sanitize_result(Some("T".into()), Some("https://example.com".into()), Some("S".into())).unwrap();
        let json = serde_json::to_value(&result).unwrap();
        assert_eq!(
            json,
            serde_json::json!({ "title": "T", "url": "https://example.com", "snippet": "S" }),
            "content fields stay absent until full mode fills them"
        );
    }

    #[tokio::test]
    async fn an_empty_query_is_refused_before_any_request() {
        for query in ["", "   ", "\n\t "] {
            let response = run_web_search(&WebConfig::default(), query).await;
            assert!(!response.ok, "{query:?}");
            assert!(response.results.is_empty());
            let error = response.error.clone().expect("a refusal carries an error");
            assert_eq!(error.code, ErrorCode::InvalidArguments, "{query:?}");
            assert_eq!(error.message, "web_search needs a non-empty 'query' string.");
            assert!(!error.user_action_required, "a blank query is the model's mistake, not the user's");
        }
    }

    #[tokio::test]
    async fn an_unready_provider_is_reported_as_data_not_an_exception() {
        let config = WebConfig {
            provider: ProviderKind::Brave,
            ..WebConfig::default()
        };
        for response in [run_web_search(&config, "mirai").await, run_search_test(&config).await] {
            assert!(!response.ok);
            assert_eq!(response.provider, None, "a failure names no provider");
            let error = response.error.clone().expect("failures carry an error");
            assert_eq!(error.code, ErrorCode::MissingApiKey);
            assert!(error.user_action_required, "the user has to add the key in Settings");
            assert!(error.message.contains("Settings"), "{}", error.message);
            assert_eq!(response.results, Vec::<SearchResult>::new());
        }
    }

    #[test]
    fn the_settings_probe_uses_an_uncached_query_within_the_bound() {
        assert_eq!(prepare_query(SEARCH_TEST_QUERY).as_deref(), Some(SEARCH_TEST_QUERY));
        assert!(
            SEARCH_TEST_QUERY.chars().count() <= MAX_QUERY_CHARS,
            "the canned query must survive the bound it is passed through"
        );
    }

    #[test]
    fn the_query_bound_is_applied_before_the_provider_sees_it() {
        let prepared = prepare_query(&"q".repeat(MAX_QUERY_CHARS + 400)).expect("non-empty");
        assert_eq!(prepared.chars().count(), MAX_QUERY_CHARS);
        assert_eq!(prepare_query(" mirai search ").as_deref(), Some("mirai search"));
        for blank in ["", "   ", "\n\t "] {
            assert_eq!(prepare_query(blank), None, "{blank:?} is nothing to search for");
        }
    }

    #[tokio::test]
    async fn a_provider_failure_is_returned_verbatim_as_error_data() {
        // SearXNG is configured but its instance URL is nonsense, so the request
        // is refused locally: no socket, and the code stays InvalidArguments.
        let config = WebConfig {
            provider: ProviderKind::Searxng,
            base_url: "http://".to_string(),
            ..WebConfig::default()
        };
        let response = run_web_search(&config, "mirai").await;
        assert!(!response.ok);
        let error: &WebError = response.error.as_ref().expect("a mapped failure");
        assert_eq!(error.code, ErrorCode::InvalidArguments);
        assert!(error.user_action_required);
    }
}
