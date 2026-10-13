//! Search backends: one request and one row parser per [`ProviderKind`].
//!
//! Providers are transport adapters and nothing more. Each arm returns [`RawResult`]
//! rows as the upstream described them, or a [`WebFailure`] the model can act on.
//! Bounds, URL validation, and the result ceiling live in `search.rs`, because a
//! provider that trusts its own API must still not be able to hand the model a
//! `javascript:` href.
//!
//! Two reqwest 0.13 details that are easy to get wrong, hence written down:
//!
//! * `RequestBuilder::query` and `RequestBuilder::form` sit behind reqwest's
//!   `query`/`form` features, which this crate does not enable (only `json`), so
//!   query strings and form bodies are assembled here with the `url` crate.
//! * A non-2xx response is not an error from `send()`, so every arm checks
//!   `Response::status` itself before touching the body.

use reqwest::{Client, header};
use scraper::{Html, Selector};
use url::{Url, form_urlencoded};

use super::types::{ErrorCode, HTTP_TIMEOUT, ProviderKind, RawResult, WebConfig, WebFailure};

/// Provider labels used inside failure messages. The model quotes them back to
/// the user, so they read as product names, not identifiers.
const DUCKDUCKGO_SOURCE: &str = "DuckDuckGo";
const SEARXNG_SOURCE: &str = "SearXNG";
const BRAVE_SOURCE: &str = "Brave Search";
const TAVILY_SOURCE: &str = "Tavily";

const DUCKDUCKGO_HOST: &str = "duckduckgo.com";
/// Wrapper links can appear on any DuckDuckGo subdomain.
const DUCKDUCKGO_HOST_SUFFIX: &str = ".duckduckgo.com";
const DUCKDUCKGO_HTML_ENDPOINT: &str = "https://html.duckduckgo.com/html/";
const BRAVE_ENDPOINT: &str = "https://api.search.brave.com/res/v1/web/search";
const TAVILY_ENDPOINT: &str = "https://api.tavily.com/search";

/// `html.duckduckgo.com` answers HTML, and answers 403 to requests that look
/// like a JSON client, so the content negotiation has to be explicit.
const HTML_ACCEPT: &str = "text/html,application/xhtml+xml";
const JSON_ACCEPT: &str = "application/json";

/// Row wrappers of a DuckDuckGo result, most specific first. The markup has been
/// renamed several times, so a page is parsed with each candidate until one of
/// them actually yields rows.
const DUCKDUCKGO_ROW_SELECTORS: [&str; 3] = ["div.result.results_links", "div.result", "div.result__body"];

/// Run one provider query and return raw, unsanitized rows.
///
/// Implementations must: trim/clamp nothing here (the caller sanitizes), map
/// every transport failure through `WebFailure::from_status` or an explicit
/// `WebFailure::new`, and treat a zero-hit query as success with an empty vec.
/// A provider that returned something we cannot read is an error instead, so the
/// caller can tell "nothing matched" apart from "the markup changed".
pub async fn provider_search(
    config: &WebConfig,
    client: &Client,
    query: &str,
    max_results: usize,
) -> Result<Vec<RawResult>, WebFailure> {
    // `search.rs` already refuses to call an unready provider; this is the
    // defensive copy so a direct caller cannot send a keyless credentialed
    // request and get a confusing 401 back. Checked first, because a config
    // problem is worth reporting even when the caller asked for zero rows.
    if let Some(failure) = missing_credential_failure(config) {
        return Err(failure);
    }
    if max_results == 0 {
        return Ok(Vec::new());
    }

    match config.provider {
        ProviderKind::Duckduckgo => duckduckgo(client, query, max_results).await,
        ProviderKind::Searxng => searxng(client, config, query, max_results).await,
        ProviderKind::Brave => brave(client, config, query, max_results).await,
        ProviderKind::Tavily => tavily(client, config, query, max_results).await,
    }
}

/// The credential a provider is missing, if any.
///
/// Pure so the dispatch guard is testable without a socket.
fn missing_credential_failure(config: &WebConfig) -> Option<WebFailure> {
    (!config.is_ready()).then(|| super::not_ready_failure(config))
}

/// Percent-encode one form field.
///
/// `url::form_urlencoded::byte_serialize` hands back an iterator of encoded
/// chunks, not something `Display`, so the pieces must be collected into the
/// final string. reqwest's own `.form()`/`.query()` builders sit behind the
/// `form`/`query` features this app does not enable, which is why encoding
/// happens here instead.
fn form_field(value: &str) -> String {
    form_urlencoded::byte_serialize(value.as_bytes()).collect()
}

// --------------------------------------------------------------------------------
// DuckDuckGo (keyless HTML scraping)
// --------------------------------------------------------------------------------

/// Scrape DuckDuckGo's no-JS HTML endpoint.
///
/// The form POST is tried first because it is the endpoint DuckDuckGo documents
/// for programmatic HTML, and the plain GET is kept as a fallback for deployments
/// that answer anything else than 2xx to a body.
async fn duckduckgo(
    client: &Client,
    query: &str,
    max_results: usize,
) -> Result<Vec<RawResult>, WebFailure> {
    let body = format!("q={}&kl=wt-wt", form_field(query));
    let posted = client
        .post(DUCKDUCKGO_HTML_ENDPOINT)
        .header(header::ACCEPT, HTML_ACCEPT)
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .timeout(HTTP_TIMEOUT)
        .body(body)
        .send()
        .await;

    let html = match response_text(posted, DUCKDUCKGO_SOURCE).await {
        Ok(html) => html,
        // Rate limits and transport failures fail identically for the GET form,
        // so only a rejected request is worth a second round trip.
        Err(failure) if matches!(failure.0.code, ErrorCode::RateLimited | ErrorCode::ProviderUnavailable) => {
            return Err(failure);
        },
        Err(_) => {
            crate::logger::warn("web:search:duckduckgo:post-fallback", Some(serde_json::json!({ "via": "get" })));
            let url = append_query(DUCKDUCKGO_HTML_ENDPOINT, DUCKDUCKGO_SOURCE, &[("q", query), ("kl", "wt-wt")])?;
            response_text(get_html(client, &url).await, DUCKDUCKGO_SOURCE).await?
        },
    };

    parse_duckduckgo_html(&html, max_results)
}

async fn get_html(
    client: &Client,
    url: &str,
) -> Result<reqwest::Response, reqwest::Error> {
    client.get(url).header(header::ACCEPT, HTML_ACCEPT).timeout(HTTP_TIMEOUT).send().await
}

/// Pull rows out of a DuckDuckGo HTML page.
///
/// Pure so the scraping contract is covered by a fixture instead of a socket.
/// Rows come back in document order, capped at `max_results`; an honest
/// "nothing matched" page is an empty `Vec`, while a page we cannot read at all
/// is `InvalidResponse` (oMLX treats these differently and so do we).
fn parse_duckduckgo_html(
    html: &str,
    max_results: usize,
) -> Result<Vec<RawResult>, WebFailure> {
    if max_results == 0 {
        return Ok(Vec::new());
    }
    let document = Html::parse_document(html);
    let Some(parts) = DuckDuckGoParts::new() else {
        return Err(WebFailure::new(
            ErrorCode::UnexpectedFailure,
            "the built-in DuckDuckGo selectors failed to compile",
            false,
        ));
    };

    for selector in DUCKDUCKGO_ROW_SELECTORS.iter().filter_map(|source| Selector::parse(source).ok()) {
        let rows = document
            .select(&selector)
            .filter(|row| !is_sponsored(row))
            .filter_map(|row| duckduckgo_row(&row, &parts))
            .take(max_results)
            .collect::<Vec<_>>();
        if !rows.is_empty() {
            return Ok(rows);
        }
    }

    // Layout fallback: some builds drop the wrapper divs but keep the anchors,
    // and an anchor alone is still a citable URL even without a snippet.
    let loose = document
        .select(&parts.link)
        .filter_map(|link| loose_duckduckgo_row(&link))
        .take(max_results)
        .collect::<Vec<_>>();
    if !loose.is_empty() {
        return Ok(loose);
    }

    if reports_no_results(&document) {
        return Ok(Vec::new());
    }
    Err(WebFailure::new(
        ErrorCode::InvalidResponse,
        format!("{DUCKDUCKGO_SOURCE} returned a page with no parsable results; it may have changed its markup."),
        false,
    ))
}

/// The three pieces of one DuckDuckGo row, parsed once per page.
struct DuckDuckGoParts {
    link: Selector,
    heading: Selector,
    snippet: Selector,
}

impl DuckDuckGoParts {
    fn new() -> Option<Self> {
        Some(Self {
            link: Selector::parse("a.result__a").ok()?,
            heading: Selector::parse("h2.result__title").ok()?,
            snippet: Selector::parse(".result__snippet").ok()?,
        })
    }
}

fn duckduckgo_row(
    row: &HtmlRow<'_>,
    parts: &DuckDuckGoParts,
) -> Option<RawResult> {
    let link = row.select(&parts.link).next()?;
    // A row without a resolvable destination is not a result, so it is dropped
    // here rather than consuming one of the `max_results` slots.
    let url = link.attr("href").or_else(|| link.attr("data-url")).and_then(decode_duckduckgo_href)?;
    // Titles live in the anchor text; the heading is the fallback for markup that
    // wraps the anchor in extra nodes, and an empty one lets `sanitize_result`
    // substitute the host.
    let title = element_text(&link)
        .or_else(|| row.select(&parts.heading).next().and_then(|heading| element_text(&heading)))
        .or_else(|| link.attr("title").and_then(non_empty_text));
    let snippet = row.select(&parts.snippet).next().and_then(|node| element_text(&node));
    Some(RawResult {
        title,
        url: Some(url),
        snippet,
    })
}

fn loose_duckduckgo_row(link: &HtmlRow<'_>) -> Option<RawResult> {
    let url = link.attr("href").or_else(|| link.attr("data-url")).and_then(decode_duckduckgo_href)?;
    Some(RawResult {
        title: element_text(link),
        url: Some(url),
        snippet: None,
    })
}

/// Decode DuckDuckGo's outbound-link wrapper back into the real destination.
///
/// Every result link is rewritten as `https://duckduckgo.com/l/?uddg=<encoded
/// target>&rut=<hash>`, so without this step every URL the model cites is a
/// redirect through DuckDuckGo instead of the page itself.
///
/// Hrefs that are not DuckDuckGo redirects are returned as an absolute URL
/// unchanged, including unsafe schemes: this function unwraps the redirect, it
/// does not vet the target. [`crate::web::search::sanitize_result`] is the gate.
fn decode_duckduckgo_href(href: &str) -> Option<String> {
    let href = href.trim();
    if href.is_empty() {
        return None;
    }
    let absolute = if let Some(rest) = href.strip_prefix("//") {
        format!("https://{rest}")
    } else if let Some(rest) = href.strip_prefix('/') {
        format!("https://{DUCKDUCKGO_HOST}/{rest}")
    } else {
        href.to_string()
    };

    let Ok(url) = Url::parse(&absolute) else {
        return Some(absolute);
    };
    if !is_duckduckgo_redirect(&url) {
        return Some(absolute);
    }
    let target = url.query_pairs().find_map(|(name, value)| (name == "uddg").then_some(value.into_owned()))?;
    let target = target.trim();
    if target.is_empty() {
        return None;
    }
    Some(match target.strip_prefix("//") {
        Some(rest) => format!("https://{rest}"),
        None => target.to_string(),
    })
}

fn is_duckduckgo_redirect(url: &Url) -> bool {
    url.path().trim_end_matches('/') == "/l"
        && url.host_str().is_some_and(|host| host == DUCKDUCKGO_HOST || host.ends_with(DUCKDUCKGO_HOST_SUFFIX))
}

/// DuckDuckGo marks sponsored rows with a class on the wrapper. They are ads, not
/// answers, and the model must not present them as search results.
fn is_sponsored(row: &HtmlRow<'_>) -> bool {
    row.attr("class").is_some_and(|classes| classes.contains("result--ad") || classes.contains("result--discuss"))
}

/// True when DuckDuckGo said, in its own markup, that nothing matched.
fn reports_no_results(document: &Html) -> bool {
    let marked =
        Selector::parse(".no-results").ok().is_some_and(|selector| document.select(&selector).next().is_some());
    if marked {
        return true;
    }
    document.root_element().text().collect::<String>().to_lowercase().contains("no results found")
}

// --------------------------------------------------------------------------------
// SearXNG (self-hosted, JSON format must be enabled on the instance)
// --------------------------------------------------------------------------------

async fn searxng(
    client: &Client,
    config: &WebConfig,
    query: &str,
    max_results: usize,
) -> Result<Vec<RawResult>, WebFailure> {
    let url = searxng_search_url(&config.base_url, query)?;
    let response = client.get(url).header(header::ACCEPT, JSON_ACCEPT).timeout(HTTP_TIMEOUT).send().await;
    let payload = response_json(response, SEARXNG_SOURCE).await?;
    Ok(json_rows(&payload, &["results"], &["content", "snippet"], max_results))
}

/// `{base_url}/search?q=<query>&format=json`, tolerating a base URL that already
/// carries a path prefix or a trailing `/search`.
///
/// Pure so instance URL handling is testable offline.
fn searxng_search_url(
    base_url: &str,
    query: &str,
) -> Result<String, WebFailure> {
    let mut url = http_endpoint(SEARXNG_SOURCE, base_url)?;
    let path = url.path().trim_end_matches('/');
    let path = if path.ends_with("/search") {
        path.to_string()
    } else {
        format!("{path}/search")
    };
    url.set_path(&path);
    append_query(url.as_str(), SEARXNG_SOURCE, &[("q", query), ("format", "json")])
}

// --------------------------------------------------------------------------------
// Brave Search API
// --------------------------------------------------------------------------------

async fn brave(
    client: &Client,
    config: &WebConfig,
    query: &str,
    max_results: usize,
) -> Result<Vec<RawResult>, WebFailure> {
    // Brave caps `count` at 20; the caller's ceiling is far below that anyway.
    let count = max_results.min(20).to_string();
    let url = append_query(BRAVE_ENDPOINT, BRAVE_SOURCE, &[("q", query), ("count", count.as_str())])?;
    let response = client
        .get(url)
        .header(header::ACCEPT, JSON_ACCEPT)
        .header("X-Subscription-Token", config.api_key.trim())
        .timeout(HTTP_TIMEOUT)
        .send()
        .await;
    let payload = response_json(response, BRAVE_SOURCE).await?;
    Ok(json_rows(&payload, &["web", "results"], &["description", "snippet"], max_results))
}

// --------------------------------------------------------------------------------
// Tavily
// --------------------------------------------------------------------------------

/// Query Tavily.
///
/// Tavily moved authentication to an `Authorization: Bearer` header only, but the
/// body `api_key` field is still honoured by older deployments, so this sends the
/// bearer header and keeps `api_key` in the JSON body for compatibility.
async fn tavily(
    client: &Client,
    config: &WebConfig,
    query: &str,
    max_results: usize,
) -> Result<Vec<RawResult>, WebFailure> {
    let api_key = config.api_key.trim();
    let payload = serde_json::json!({
        "api_key": api_key,
        "query": query,
        "max_results": max_results,
        "include_raw_content": false,
    });
    let response = client
        .post(TAVILY_ENDPOINT)
        .header(header::ACCEPT, JSON_ACCEPT)
        .bearer_auth(api_key)
        .json(&payload)
        .timeout(HTTP_TIMEOUT)
        .send()
        .await;
    let payload = response_json(response, TAVILY_SOURCE).await?;
    Ok(json_rows(&payload, &["results"], &["content"], max_results))
}

// --------------------------------------------------------------------------------
// Shared transport and row helpers
// --------------------------------------------------------------------------------

/// Turn a `send()` result into text, mapping status and transport errors onto the
/// provider's [`WebFailure`] so the model sees one shape for every backend.
async fn response_text(
    response: Result<reqwest::Response, reqwest::Error>,
    source: &str,
) -> Result<String, WebFailure> {
    let response = map_send_result(response, source)?;
    let status = response.status();
    if !status.is_success() {
        return Err(WebFailure::from_status(status.as_u16(), source));
    }
    response.text().await.map_err(|error| map_request_error(&error, source))
}

async fn response_json(
    response: Result<reqwest::Response, reqwest::Error>,
    source: &str,
) -> Result<serde_json::Value, WebFailure> {
    let response = map_send_result(response, source)?;
    let status = response.status();
    if !status.is_success() {
        return Err(WebFailure::from_status(status.as_u16(), source));
    }
    response.json::<serde_json::Value>().await.map_err(|error| map_request_error(&error, source))
}

fn map_send_result(
    response: Result<reqwest::Response, reqwest::Error>,
    source: &str,
) -> Result<reqwest::Response, WebFailure> {
    response.map_err(|error| map_request_error(&error, source))
}

/// Classify a reqwest error. Anything that stopped us reaching the provider is
/// `ProviderUnavailable` (retrying later may work); a body we cannot read is
/// `InvalidResponse`; a status the caller would have seen is mapped like one.
fn map_request_error(
    error: &reqwest::Error,
    source: &str,
) -> WebFailure {
    if let Some(status) = error.status() {
        return WebFailure::from_status(status.as_u16(), source);
    }
    if error.is_decode() {
        return WebFailure::new(
            ErrorCode::InvalidResponse,
            format!("{source} returned a body that could not be read."),
            false,
        );
    }
    if error.is_timeout() {
        return WebFailure::new(
            ErrorCode::ProviderUnavailable,
            format!("{source} did not answer within {} seconds.", HTTP_TIMEOUT.as_secs()),
            false,
        );
    }
    if error.is_connect() || error.is_dns() || error.is_request() || error.is_body() {
        return WebFailure::new(
            ErrorCode::ProviderUnavailable,
            format!("{source} could not be reached ({error})."),
            false,
        );
    }
    WebFailure::new(ErrorCode::RequestFailed, format!("{source} request failed ({error})."), false)
}

/// Rows from a `{ "<path>": [ { "title", "url", "<snippet key>" } ] }` payload.
///
/// A missing container or a non-array is treated as zero results, not as a
/// malformed response: every provider here answers with an empty object, or
/// leaves the key out entirely, when nothing matched.
fn json_rows(
    payload: &serde_json::Value,
    path: &[&str],
    snippet_keys: &[&str],
    max_results: usize,
) -> Vec<RawResult> {
    let Some(items) = json_container(payload, path) else {
        return Vec::new();
    };
    items
        .iter()
        .take(max_results)
        .map(|item| RawResult {
            title: string_field(item, "title"),
            url: string_field(item, "url"),
            snippet: snippet_keys.iter().copied().find_map(|key| string_field(item, key)),
        })
        .collect()
}

/// Walk a dotted path through a JSON payload and require an array at the end.
fn json_container<'a>(
    payload: &'a serde_json::Value,
    path: &[&str],
) -> Option<&'a [serde_json::Value]> {
    let mut node = payload;
    for key in path {
        node = node.get(*key)?;
    }
    node.as_array().map(|items| items.as_slice())
}

fn string_field(
    item: &serde_json::Value,
    key: &str,
) -> Option<String> {
    item.get(key)
        .and_then(serde_json::Value::as_str)
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .map(|value| value.to_string())
}

/// Percent-encode `pairs` onto `endpoint` as a query string.
///
/// Hand-written because `RequestBuilder::query` needs reqwest's `query` feature.
fn append_query(
    endpoint: &str,
    source: &str,
    pairs: &[(&str, &str)],
) -> Result<String, WebFailure> {
    let mut url = http_endpoint(source, endpoint)?;
    let encoded = pairs
        .iter()
        .map(|(key, value)| format!("{}={}", form_field(key), form_field(value)))
        .collect::<Vec<_>>()
        .join("&");
    let query = match url.query() {
        Some(existing) if !existing.is_empty() => format!("{existing}&{encoded}"),
        _ => encoded,
    };
    url.set_query(Some(&query));
    Ok(url.into())
}

/// Parse a user- or build-time supplied endpoint, allowing only plain http(s).
fn http_endpoint(
    source: &str,
    candidate: &str,
) -> Result<Url, WebFailure> {
    let parsed = Url::parse(candidate.trim()).map_err(|_| invalid_endpoint(source, candidate))?;
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none_or(|host| host.is_empty()) {
        return Err(invalid_endpoint(source, candidate));
    }
    Ok(parsed)
}

fn invalid_endpoint(
    source: &str,
    endpoint: &str,
) -> WebFailure {
    WebFailure::new(
        ErrorCode::InvalidArguments,
        format!("{source} endpoint `{endpoint}` is not a valid http(s) URL. Check it in Settings, Web Search."),
        true,
    )
}

/// Visible text of an element, with markup whitespace collapsed away, or `None`
/// when the element says nothing.
fn element_text(element: &HtmlRow<'_>) -> Option<String> {
    let collapsed = element.text().collect::<Vec<&str>>().join(" ").split_whitespace().collect::<Vec<&str>>().join(" ");
    non_empty_text(&collapsed)
}

fn non_empty_text(value: &str) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

/// The element type `scraper` hands back for a selected node.
type HtmlRow<'a> = scraper::ElementRef<'a>;

#[cfg(test)]
mod tests {
    use super::*;

    /// A trimmed-down copy of the real `html.duckduckgo.com` row markup: two
    /// organic results whose links are `uddg` redirects, plus a sponsored row
    /// that must never be returned.
    const DUCKDUCKGO_FIXTURE: &str = r#"<html><body>
      <div class="result results_links results_links_deep web-result">
        <div class="links_main links_deep result__body">
          <h2 class="result__title">
            <a rel="nofollow" class="result__a"
               href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.com%2Fdocs%2Frust&rut=3f2b1c">
              The Rust Programming &amp; Language
            </a>
          </h2>
          <a class="result__snippet" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.com%2Fdocs%2Frust&rut=3f2b1c">
            A language empowering everyone to build
            <b>reliable</b>   and efficient software.
          </a>
        </div>
      </div>
      <div class="result results_links results_links_deep web-result result--ad">
        <div class="links_main links_deep result__body">
          <h2 class="result__title">
            <a class="result__a" href="https://sponsor.example/buy-now">Buy a course</a>
          </h2>
          <a class="result__snippet">Sponsored</a>
        </div>
      </div>
      <div class="result results_links results_links_deep web-result">
        <div class="links_main links_deep result__body">
          <h2 class="result__title">
            <a class="result__a" href="https://without-redirect.dev/page?a=1">Direct result</a>
          </h2>
          <a class="result__snippet">   Already   spaced   out   </a>
        </div>
      </div>
    </body></html>"#;

    const DUCKDUCKGO_NO_RESULTS_FIXTURE: &str = r#"<html><body>
      <div class="header__results"><h2 class="no-results">No  results found</h2></div>
    </body></html>"#;

    /// The same rows without any of the wrapper divs DuckDuckGo normally emits.
    const DUCKDUCKGO_FLAT_FIXTURE: &str = concat!(
        "<html><body>",
        "<a class=\"result__a\" href=\"//duckduckgo.com/l/?uddg=https%3A%2F%2Fflat.example%2F\">Flat</a>",
        "</body></html>",
    );

    fn config(
        provider: ProviderKind,
        api_key: &str,
        base_url: &str,
    ) -> WebConfig {
        WebConfig {
            provider,
            api_key: api_key.to_string(),
            base_url: base_url.to_string(),
            ..WebConfig::default()
        }
    }

    #[test]
    fn uddg_redirects_are_decoded_to_the_real_destination() {
        let encoded = "//duckduckgo.com/l/?uddg=https%3A%2F%2Fmirai.dev%2Fsearch%3Fq%3Dtauri&rut=deadbeef";
        assert_eq!(
            decode_duckduckgo_href(encoded).as_deref(),
            Some("https://mirai.dev/search?q=tauri"),
            "the model must never be handed a DuckDuckGo wrapper link"
        );
        assert_eq!(
            decode_duckduckgo_href("https://duckduckgo.com/l/?uddg=https%3A%2F%2Fmirai.dev%2F").as_deref(),
            Some("https://mirai.dev/")
        );
        // Path-relative and protocol-relative wrapper forms appear in the wild.
        assert_eq!(
            decode_duckduckgo_href("/l/?uddg=https%3A%2F%2Fmirai.dev%2Fa").as_deref(),
            Some("https://mirai.dev/a")
        );
        assert_eq!(
            decode_duckduckgo_href("//duckduckgo.com/l/?uddg=%2F%2Fmirai.dev%2Fb&rut=1").as_deref(),
            Some("https://mirai.dev/b")
        );
    }

    #[test]
    fn non_redirect_hrefs_pass_through_and_unsafe_schemes_are_left_for_sanitize() {
        assert_eq!(decode_duckduckgo_href("https://example.com/x?y=1").as_deref(), Some("https://example.com/x?y=1"));
        // This function unwraps the redirect; vetting the target is sanitize's job.
        assert_eq!(decode_duckduckgo_href("javascript:alert(1)").as_deref(), Some("javascript:alert(1)"));
        assert_eq!(decode_duckduckgo_href(""), None);
        assert!(decode_duckduckgo_href("   ").is_none());
        // A wrapper with no `uddg` argument has no destination to report.
        assert_eq!(decode_duckduckgo_href("//duckduckgo.com/l/?rut=abc123"), None);
        assert_eq!(decode_duckduckgo_href("//duckduckgo.com/l/?uddg=%20"), None);
    }

    #[test]
    fn duckduckgo_rows_are_parsed_with_titles_snippets_and_real_urls() {
        let rows = parse_duckduckgo_html(DUCKDUCKGO_FIXTURE, 10).expect("fixture parses");
        assert_eq!(rows.len(), 2, "the sponsored row must be dropped");
        assert_eq!(rows[0].url.as_deref(), Some("https://example.com/docs/rust"));
        assert_eq!(rows[0].title.as_deref(), Some("The Rust Programming & Language"));
        assert_eq!(
            rows[0].snippet.as_deref(),
            Some("A language empowering everyone to build reliable and efficient software."),
            "markup whitespace and inline tags must collapse into one line"
        );
        assert_eq!(rows[1].url.as_deref(), Some("https://without-redirect.dev/page?a=1"));
        assert_eq!(rows[1].title.as_deref(), Some("Direct result"));
        assert_eq!(rows[1].snippet.as_deref(), Some("Already spaced out"));
    }

    #[test]
    fn duckduckgo_parsing_respects_max_results() {
        assert_eq!(parse_duckduckgo_html(DUCKDUCKGO_FIXTURE, 1).expect("one row").len(), 1);
        assert!(parse_duckduckgo_html(DUCKDUCKGO_FIXTURE, 0).expect("none").is_empty());
    }

    #[test]
    fn empty_pages_are_success_and_unparsable_pages_are_invalid_response() {
        let empty = parse_duckduckgo_html(DUCKDUCKGO_NO_RESULTS_FIXTURE, 3).expect("no results is not an error");
        assert!(empty.is_empty());
        // Also recognized from the visible copy when the class marker is absent.
        let prose = parse_duckduckgo_html("<html><body><p>No results found for that.</p></body></html>", 3)
            .expect("no results is not an error");
        assert!(prose.is_empty());

        // A blocked or rewritten page must raise, not look like zero hits.
        let anomaly = r#"<html><body><div class="anomaly">Are you a robot?</div></body></html>"#;
        let failure =
            parse_duckduckgo_html(anomaly, 3).expect_err("a page we cannot read is a failure, not an empty answer");
        assert_eq!(failure.0.code, ErrorCode::InvalidResponse);
    }

    #[test]
    fn bare_anchors_still_yield_urls_when_the_wrappers_are_missing() {
        let rows = parse_duckduckgo_html(DUCKDUCKGO_FLAT_FIXTURE, 3).expect("flat layout parses");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].url.as_deref(), Some("https://flat.example/"));
        assert_eq!(rows[0].title.as_deref(), Some("Flat"));
    }

    #[test]
    fn searxng_endpoints_are_built_from_the_instance_base_url() {
        let url = searxng_search_url("https://searx.internal", "rust book").expect("plain base url");
        assert!(url.starts_with("https://searx.internal/search?"), "unexpected {url}");
        assert!(url.contains("q=rust+book"), "the query must be form encoded: {url}");
        assert!(url.contains("format=json"), "SearXNG only answers JSON when asked: {url}");

        let prefixed = searxng_search_url("https://searx.internal/searx/", "x").expect("prefixed base url");
        assert!(prefixed.starts_with("https://searx.internal/searx/search?"), "unexpected {prefixed}");

        let already = searxng_search_url("https://searx.internal/search", "x").expect("/search base url");
        assert!(already.starts_with("https://searx.internal/search?"), "must not double up: {already}");
    }

    #[test]
    fn unusable_instance_urls_are_argument_errors_with_a_settings_hint() {
        for base_url in ["searx.internal", "file:///etc/passwd", "https://", "not a url"] {
            let failure = searxng_search_url(base_url, "x").expect_err("{base_url} must be rejected");
            assert_eq!(failure.0.code, ErrorCode::InvalidArguments, "{base_url}");
            assert!(failure.0.user_action_required, "{base_url}");
            assert!(failure.0.message.contains("Settings"), "{base_url}");
        }
    }

    #[test]
    fn json_rows_read_each_provider_shape_and_stop_at_the_ceiling() {
        let brave = serde_json::json!({
            "web": { "results": [
                { "title": "One", "url": "https://a.example", "description": " First " },
                { "title": "Two", "url": "https://b.example", "description": "Second" },
            ] },
        });
        let rows = json_rows(&brave, &["web", "results"], &["description"], 10);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].snippet.as_deref(), Some("First"), "fields are trimmed");
        assert_eq!(json_rows(&brave, &["web", "results"], &["description"], 1).len(), 1);

        let tavily = serde_json::json!({ "results": [
            { "title": "T", "url": "https://t.example", "content": "body" }
        ] });
        assert_eq!(json_rows(&tavily, &["results"], &["content"], 3)[0].snippet.as_deref(), Some("body"));

        // An absent or malformed container means zero hits, never a parse error.
        assert!(json_rows(&serde_json::json!({}), &["results"], &["content"], 3).is_empty());
        assert!(json_rows(&serde_json::json!({ "results": "oops" }), &["results"], &["content"], 3).is_empty());
        assert!(json_rows(&serde_json::json!(null), &["web", "results"], &["description"], 3).is_empty());
        // Empty strings are not snippets.
        let blank = serde_json::json!({ "results": [{ "title": "", "url": "https://c.example", "content": "" }] });
        let rows = json_rows(&blank, &["results"], &["content"], 3);
        assert_eq!((rows[0].title.as_ref(), rows[0].snippet.as_ref()), (None, None));
    }

    #[test]
    fn missing_credentials_are_reported_before_any_request() {
        assert!(missing_credential_failure(&config(ProviderKind::Duckduckgo, "", "")).is_none());
        assert!(missing_credential_failure(&config(ProviderKind::Searxng, "", "https://searx.internal")).is_none());

        for provider in [ProviderKind::Brave, ProviderKind::Tavily] {
            let failure = missing_credential_failure(&config(provider, "   ", "")).expect("blank key is no key");
            assert_eq!(failure.0.code, ErrorCode::MissingApiKey);
            assert!(failure.0.user_action_required);
            assert!(failure.0.message.contains("Settings"));
        }
        let failure = missing_credential_failure(&config(ProviderKind::Searxng, "", "  ")).expect("blank instance url");
        assert_eq!(failure.0.code, ErrorCode::MissingApiKey);
        assert!(missing_credential_failure(&config(ProviderKind::Brave, "token", "")).is_none());
    }

    #[tokio::test]
    async fn provider_dispatch_refuses_uncredentialed_providers_without_network() {
        let client = Client::new();
        for provider in [ProviderKind::Brave, ProviderKind::Tavily, ProviderKind::Searxng] {
            let config = config(provider, "", "");
            let failure = provider_search(&config, &client, "anything", config.max_results)
                .await
                .expect_err("a provider without its credential must not send a request");
            assert_eq!(failure.0.code, ErrorCode::MissingApiKey, "{}", provider.as_str());
            assert!(failure.0.user_action_required, "{}", provider.as_str());
        }
    }

    #[tokio::test]
    async fn a_zero_result_ceiling_short_circuits_the_request() {
        // DuckDuckGo is ready with no credential, so this reaches the ceiling
        // guard instead of the one above it.
        let config = config(ProviderKind::Duckduckgo, "", "");
        let rows = provider_search(&config, &Client::new(), "anything", 0)
            .await
            .expect("zero results is an empty answer, not a failure");
        assert!(rows.is_empty());
    }

    #[test]
    fn provider_ids_round_trip_through_the_settings_parser() {
        for kind in [ProviderKind::Duckduckgo, ProviderKind::Searxng, ProviderKind::Brave, ProviderKind::Tavily] {
            assert_eq!(ProviderKind::from_setting(kind.as_str()), kind);
            assert_eq!(ProviderKind::from_setting(&kind.as_str().to_uppercase()), kind);
        }
    }

    // --------------------------------------------------------------------------------
    // Live provider checks (opt-in: `cargo test -- --ignored --nocapture`)
    // --------------------------------------------------------------------------------
    //
    // A fixture proves the parser agrees with itself, not that the endpoint still
    // answers in that shape. DuckDuckGo's HTML classes have changed before. These are
    // `#[ignore]`d so CI stays offline, and are meant to be run by hand when a provider
    // is suspected of drifting.

    /// Keyless default provider, end to end: request, parse, sanitize.
    #[tokio::test]
    #[ignore = "makes a live network request"]
    async fn live_duckduckgo_answers_with_readable_results() {
        report_live("duckduckgo", &WebConfig::default()).await;
    }

    /// Keyed providers run only when their credential is in the environment, so this
    /// stays green on a machine that has none.
    #[tokio::test]
    #[ignore = "makes a live network request"]
    async fn live_keyed_providers_answer_when_configured() {
        for (kind, variable) in [
            (ProviderKind::Brave, "BRAVE_API_KEY"),
            (ProviderKind::Tavily, "TAVILY_API_KEY"),
            (ProviderKind::Searxng, "SEARXNG_URL"),
        ] {
            let value = std::env::var(variable).unwrap_or_default();
            if value.trim().is_empty() {
                println!("skipping {}: set {variable} to check it", kind.as_str());
                continue;
            }
            let config = WebConfig {
                provider: kind,
                api_key: if kind == ProviderKind::Searxng {
                    String::new()
                } else {
                    value.clone()
                },
                base_url: if kind == ProviderKind::Searxng {
                    value
                } else {
                    String::new()
                },
                ..WebConfig::default()
            };
            report_live(kind.as_str(), &config).await;
        }
    }

    /// Full-content mode is the expensive path: search, then fetch every result page.
    /// One live run proves the enrichment wiring and that a failed page never sinks
    /// the search around it.
    #[tokio::test]
    #[ignore = "makes several live network requests"]
    async fn live_full_content_mode_inlines_page_text() {
        let config = WebConfig {
            content_mode: crate::web::types::ContentMode::Full,
            max_results: 2,
            content_max_chars: 1_200,
            ..WebConfig::default()
        };
        let response = crate::web::search::run_web_search(&config, "rust 1.99 release notes").await;
        assert!(response.ok, "full-content search failed: {:?}", response.error.map(|error| error.message));
        for result in &response.results {
            println!(
                "{}: content={} chars, error={:?}",
                result.url,
                result.content.as_deref().unwrap_or("").chars().count(),
                result.content_error
            );
        }
        assert_eq!(response.results.len(), 2, "the configured result count must be honoured");
        assert!(
            response
                .results
                .iter()
                .any(|result| result.content.as_deref().is_some_and(|text| text.chars().count() > 40)),
            "no page text was inlined: {:?}",
            response.results.iter().map(|result| (&result.url, &result.content_error)).collect::<Vec<_>>()
        );
        assert!(
            response
                .results
                .iter()
                .all(|result| result.content.as_deref().is_none_or(|text| text.chars().count() <= 1_200)),
            "the content ceiling was not applied"
        );
    }

    async fn report_live(
        name: &str,
        config: &WebConfig,
    ) {
        let response = crate::web::search::run_web_search(config, "latest stable rust release").await;
        if !response.ok {
            let error = response.error.unwrap_or_else(|| {
                crate::web::types::WebError::new(
                    ErrorCode::UnexpectedFailure,
                    "no error field on a failed response",
                    false,
                )
            });
            panic!("{name} failed: {:?} — {}", error.code, error.message);
        }
        println!("{name}: provider={:?} results={}", response.provider, response.results.len());
        for result in &response.results {
            println!("  {} | {}", result.title, result.url);
            println!("    {}", result.snippet.chars().take(120).collect::<String>());
        }
        assert!(!response.results.is_empty(), "{name} returned ok with zero results for a query that must match");
        for result in &response.results {
            assert!(
                result.url.starts_with("http://") || result.url.starts_with("https://"),
                "{name} leaked {}",
                result.url
            );
            assert!(
                !result.url.contains("duckduckgo.com/l?") && !result.url.contains("uddg="),
                "wrapper link survived: {}",
                result.url
            );
            assert!(!result.title.trim().is_empty(), "{name} returned an untitled, unhosted row");
        }
    }
}
