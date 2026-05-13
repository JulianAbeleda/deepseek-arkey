use std::collections::BTreeMap;
use std::net::{IpAddr, SocketAddr, ToSocketAddrs};
use std::time::Duration;

use reqwest::blocking::Client;
use reqwest::header::{HeaderMap, ACCEPT, ACCEPT_LANGUAGE, CONTENT_TYPE};
use reqwest::Url;
use serde::Deserialize;
use serde_json::json;

use crate::features;
use crate::provider;
use crate::provider::{system_message, Message};
use crate::runtime;
use crate::safety::cap_text;

const BRAVE_SEARCH_ENDPOINT: &str = "https://api.search.brave.com/res/v1/web/search";
const TAVILY_SEARCH_ENDPOINT: &str = "https://api.tavily.com/search";
const DEFAULT_MAX_RESULTS: usize = 5;
const MAX_SEARCH_RESULTS: usize = 10;
const DEFAULT_FETCH_MAX_BYTES: u64 = 1_000_000;
const HARD_FETCH_MAX_BYTES: u64 = 10 * 1024 * 1024;
const DEFAULT_TIMEOUT_MS: u64 = 15_000;
const HARD_TIMEOUT_MS: u64 = 60_000;
const MAX_REDIRECTS: usize = 5;
const SEARCH_CONTEXT_CAP: usize = 8_000;
const FETCH_CONTEXT_CAP: usize = 12_000;
const USER_AGENT_VALUE: &str =
    "deepseek-arkey/0.1 (+https://github.com/JulianAbeleda/deepseek-arkey)";
const BRAVE_API_KEY_SETUP_HELP: &str = r#"Brave Search API key is not set.

For troubleshooting, you can share this message with an AI provider or support
chat, but do not include any real API keys, tokens, or secrets.

Set it for the current shell:
  export BRAVE_SEARCH_API_KEY="your_brave_search_api_key"

For zsh persistence on this machine:
  echo 'export BRAVE_SEARCH_API_KEY="your_brave_search_api_key"' >> ~/.zsh_secrets
  source ~/.zshrc

The ~/.zshrc file sources ~/.zsh_secrets, so keep provider keys there instead
of writing secrets directly into ~/.zshrc.

Then verify:
  deepseek
  /features

BRAVE_API_KEY is also accepted as a legacy alias, but BRAVE_SEARCH_API_KEY is
preferred."#;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SearchProvider {
    Brave,
    Tavily,
}

impl SearchProvider {
    fn parse(value: &str) -> Result<Self, String> {
        match value.trim().to_ascii_lowercase().as_str() {
            "" | "brave" => Ok(Self::Brave),
            "tavily" => Ok(Self::Tavily),
            other => Err(format!(
                "unsupported search provider `{other}`; use brave or tavily"
            )),
        }
    }

    fn active() -> Result<Self, String> {
        let runtime_state = runtime::load(provider::DEFAULT_MODEL)?;
        let env_value = std::env::var("DEEPSEEK_SEARCH_PROVIDER").ok();
        let provider = features::active_search_provider(
            runtime_state.search_provider.as_deref(),
            env_value.as_deref(),
        );
        Self::parse(&provider)
    }

    fn name(self) -> &'static str {
        match self {
            Self::Brave => "brave",
            Self::Tavily => "tavily",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FetchFormat {
    Markdown,
    Text,
    Raw,
}

impl FetchFormat {
    fn parse(value: Option<&str>) -> Result<Self, String> {
        match value
            .unwrap_or("markdown")
            .trim()
            .to_ascii_lowercase()
            .as_str()
        {
            "" | "markdown" | "md" => Ok(Self::Markdown),
            "text" | "txt" | "plain" => Ok(Self::Text),
            "raw" | "html" => Ok(Self::Raw),
            other => Err(format!(
                "unknown fetch format `{other}`; use markdown, text, or raw"
            )),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SearchEntry {
    title: String,
    url: String,
    snippet: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SearchResponse {
    provider: &'static str,
    query: String,
    results: Vec<SearchEntry>,
}

#[derive(Debug, Clone)]
struct FetchOptions {
    format: FetchFormat,
    max_bytes: u64,
    timeout_ms: u64,
}

impl FetchOptions {
    fn new(format: FetchFormat, max_bytes: u64, timeout_ms: u64) -> Self {
        Self {
            format,
            max_bytes: max_bytes.min(HARD_FETCH_MAX_BYTES),
            timeout_ms: timeout_ms.min(HARD_TIMEOUT_MS),
        }
    }
}

pub(crate) fn web_search_tool(arguments: &serde_json::Value) -> String {
    let query = match arguments.get("query").and_then(|value| value.as_str()) {
        Some(query) if !query.trim().is_empty() => query.trim(),
        _ => return "error: missing non-empty `query`".to_string(),
    };
    let max_results = arguments
        .get("max_results")
        .and_then(|value| value.as_u64())
        .map(clamp_search_results)
        .unwrap_or(DEFAULT_MAX_RESULTS);
    match web_search(query, max_results) {
        Ok(response) => format_search_response(&response),
        Err(err) => format!("error: {err}"),
    }
}

pub(crate) fn fetch_url_tool(arguments: &serde_json::Value) -> String {
    let url = match arguments.get("url").and_then(|value| value.as_str()) {
        Some(url) if !url.trim().is_empty() => url.trim(),
        _ => return "error: missing non-empty `url`".to_string(),
    };
    let format = match FetchFormat::parse(arguments.get("format").and_then(|value| value.as_str()))
    {
        Ok(format) => format,
        Err(err) => return format!("error: {err}"),
    };
    let max_bytes = arguments
        .get("max_bytes")
        .and_then(|value| value.as_u64())
        .unwrap_or(DEFAULT_FETCH_MAX_BYTES);
    let timeout_ms = arguments
        .get("timeout_ms")
        .and_then(|value| value.as_u64())
        .unwrap_or(DEFAULT_TIMEOUT_MS);
    match fetch_url(url, FetchOptions::new(format, max_bytes, timeout_ms)) {
        Ok(response) => response,
        Err(err) => format!("error: {err}"),
    }
}

pub(crate) fn web_context_message_for_prompt(prompt: &str) -> Result<Option<Message>, String> {
    if let Some(url) = first_http_url(prompt) {
        let content = fetch_url(
            url,
            FetchOptions::new(FetchFormat::Markdown, 80_000, DEFAULT_TIMEOUT_MS),
        )?;
        let context = format!(
            "Web context fetched for the user's prompt.\nURL: {url}\n\n{}",
            cap_text(&content, FETCH_CONTEXT_CAP)
        );
        return Ok(Some(system_message(context)));
    }
    if prompt_needs_search(prompt) {
        let response = web_search(prompt.trim(), DEFAULT_MAX_RESULTS)?;
        let context = format!(
            "Web search context for the user's prompt. Use these results as evidence and cite URLs when relevant.\n\n{}",
            cap_text(&format_search_response(&response), SEARCH_CONTEXT_CAP)
        );
        return Ok(Some(system_message(context)));
    }
    Ok(None)
}

pub(crate) fn web_context_message_for_prompt_lossy(
    prompt: &str,
    mut warn: impl FnMut(String),
) -> Option<Message> {
    match web_context_message_for_prompt(prompt) {
        Ok(context) => context,
        Err(err) => {
            warn(format!("web context unavailable: {err}"));
            None
        }
    }
}

fn web_search(query: &str, max_results: usize) -> Result<SearchResponse, String> {
    match SearchProvider::active()? {
        SearchProvider::Brave => brave_search(query, max_results),
        SearchProvider::Tavily => tavily_search(query, max_results),
    }
}

fn clamp_search_results(value: u64) -> usize {
    value.clamp(1, MAX_SEARCH_RESULTS as u64) as usize
}

fn brave_search(query: &str, max_results: usize) -> Result<SearchResponse, String> {
    let key = brave_api_key()?;
    let client = http_client(DEFAULT_TIMEOUT_MS)?;
    let mut url = Url::parse(BRAVE_SEARCH_ENDPOINT).map_err(|err| err.to_string())?;
    url.query_pairs_mut()
        .append_pair("q", query)
        .append_pair("count", &max_results.to_string())
        .append_pair("search_lang", "en")
        .append_pair("country", "us");
    let response = client
        .get(url)
        .header(ACCEPT, "application/json")
        .header("X-Subscription-Token", key)
        .send()
        .map_err(|err| format!("Brave search request failed: {err}"))?;
    let status = response.status();
    let body = response
        .text()
        .map_err(|err| format!("failed to read Brave response: {err}"))?;
    if !status.is_success() {
        return Err(format!(
            "Brave search failed: HTTP {} - {}",
            status.as_u16(),
            cap_text(&strip_html_tags(&body), 500)
        ));
    }
    let parsed: BraveResponse = serde_json::from_str(&body)
        .map_err(|err| format!("failed to parse Brave response: {err}"))?;
    Ok(SearchResponse {
        provider: SearchProvider::Brave.name(),
        query: query.to_string(),
        results: parsed
            .web
            .map(|web| web.results)
            .unwrap_or_default()
            .into_iter()
            .take(max_results)
            .map(|item| SearchEntry {
                title: item.title,
                url: item.url,
                snippet: item
                    .description
                    .or(item.extra_snippets.map(|items| items.join(" "))),
            })
            .collect(),
    })
}

fn tavily_search(query: &str, max_results: usize) -> Result<SearchResponse, String> {
    let key = tavily_api_key()?;
    let client = http_client(DEFAULT_TIMEOUT_MS)?;
    let payload = json!({
        "query": query,
        "search_depth": "basic",
        "max_results": max_results,
        "include_answer": false,
        "include_raw_content": false,
    });
    let response = client
        .post(TAVILY_SEARCH_ENDPOINT)
        .header(ACCEPT, "application/json")
        .header(CONTENT_TYPE, "application/json")
        .bearer_auth(key)
        .json(&payload)
        .send()
        .map_err(|err| format!("Tavily search request failed: {err}"))?;
    let status = response.status();
    let body = response
        .text()
        .map_err(|err| format!("failed to read Tavily response: {err}"))?;
    if !status.is_success() {
        return Err(format!(
            "Tavily search failed: HTTP {} - {}",
            status.as_u16(),
            cap_text(&strip_html_tags(&body), 500)
        ));
    }
    let parsed: TavilyResponse = serde_json::from_str(&body)
        .map_err(|err| format!("failed to parse Tavily response: {err}"))?;
    Ok(SearchResponse {
        provider: SearchProvider::Tavily.name(),
        query: query.to_string(),
        results: parsed
            .results
            .into_iter()
            .take(max_results)
            .map(|item| SearchEntry {
                title: item.title,
                url: item.url,
                snippet: Some(item.content).filter(|content| !content.trim().is_empty()),
            })
            .collect(),
    })
}

fn fetch_url(url: &str, options: FetchOptions) -> Result<String, String> {
    let mut current_url = Url::parse(url).map_err(|err| format!("invalid URL: {err}"))?;
    let mut redirects = 0usize;
    loop {
        let pinned = validate_fetch_target(&current_url)?;
        let mut builder = Client::builder()
            .timeout(Duration::from_millis(options.timeout_ms))
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(USER_AGENT_VALUE);
        if let Some((host, addr)) = pinned {
            builder = builder.resolve(&host, addr);
        }
        let client = builder
            .build()
            .map_err(|err| format!("failed to build HTTP client: {err}"))?;
        let response = client
            .get(current_url.clone())
            .header(ACCEPT, "text/html,text/plain,application/json,*/*;q=0.5")
            .header(ACCEPT_LANGUAGE, "en-US,en;q=0.5")
            .send()
            .map_err(|err| format!("request failed: {err}"))?;
        if response.status().is_redirection() && redirects < MAX_REDIRECTS {
            let Some(location) = response
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|value| value.to_str().ok())
            else {
                return response_to_text(response, options);
            };
            current_url = response
                .url()
                .join(location)
                .map_err(|err| format!("invalid redirect location: {err}"))?;
            redirects += 1;
            continue;
        }
        if response.status().is_redirection() {
            return Err(format!(
                "too many redirects; stopped after {MAX_REDIRECTS} redirects"
            ));
        }
        return response_to_text(response, options);
    }
}

fn response_to_text(
    response: reqwest::blocking::Response,
    options: FetchOptions,
) -> Result<String, String> {
    let final_url = response.url().to_string();
    let status = response.status();
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("application/octet-stream")
        .to_string();
    let headers = response_headers(response.headers());
    let bytes = response
        .bytes()
        .map_err(|err| format!("failed to read body: {err}"))?;
    let truncated = bytes.len() as u64 > options.max_bytes;
    let usable = if truncated {
        &bytes[..options.max_bytes as usize]
    } else {
        &bytes[..]
    };
    let body = String::from_utf8_lossy(usable).to_string();
    let content = match options.format {
        FetchFormat::Raw => body,
        FetchFormat::Markdown | FetchFormat::Text => {
            if content_type.to_ascii_lowercase().contains("text/html") || body_has_html(&body) {
                html_to_text(&body)
            } else {
                body
            }
        }
    };
    let value = json!({
        "url": final_url,
        "status": status.as_u16(),
        "headers": headers,
        "content_type": content_type,
        "content": content,
        "truncated": truncated,
    });
    let rendered = serde_json::to_string_pretty(&value)
        .map_err(|err| format!("failed to serialize fetch response: {err}"))?;
    if !status.is_success() {
        return Ok(format!("failed:\n{rendered}"));
    }
    Ok(format!("ok:\n{rendered}"))
}

fn validate_fetch_target(url: &Url) -> Result<Option<(String, SocketAddr)>, String> {
    if !matches!(url.scheme(), "http" | "https") {
        return Err("only http:// and https:// URLs are supported".to_string());
    }
    let host = url
        .host_str()
        .map(str::to_ascii_lowercase)
        .ok_or_else(|| "URL must include a host".to_string())?;
    if host == "localhost" || host == "localhost.localdomain" {
        return Err("requests to localhost are not allowed".to_string());
    }
    let ip_candidate = host
        .strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))
        .unwrap_or(host.as_str());
    if let Ok(ip) = ip_candidate.parse::<IpAddr>() {
        validate_ip(&ip)?;
        return Ok(None);
    }
    let port = url
        .port_or_known_default()
        .ok_or_else(|| "URL must include a port or known scheme".to_string())?;
    let addrs = (host.as_str(), port)
        .to_socket_addrs()
        .map_err(|err| format!("failed to resolve host: {err}"))?
        .collect::<Vec<_>>();
    let Some(first) = addrs.first().copied() else {
        return Err("host resolved to no addresses".to_string());
    };
    for addr in &addrs {
        validate_ip(&addr.ip())?;
    }
    Ok(Some((host, first)))
}

fn validate_ip(ip: &IpAddr) -> Result<(), String> {
    if is_restricted_ip(ip) {
        Err(format!(
            "IP {ip} is a restricted address (private/loopback/link-local)"
        ))
    } else {
        Ok(())
    }
}

fn is_restricted_ip(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_multicast()
                || v4.is_broadcast()
                || v4.is_unspecified()
                || matches!(v4.octets(), [100, 64..=127, ..])
                || *v4 == std::net::Ipv4Addr::new(169, 254, 169, 254)
                || matches!(v4.octets(), [198, 18..=19, ..])
                || v4.octets()[0] >= 240
        }
        IpAddr::V6(v6) => {
            if v6.is_unspecified()
                || matches!(v6.octets(), [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xff, 0xff, ..])
            {
                return true;
            }
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_restricted_ip(&IpAddr::V4(v4));
            }
            v6.is_loopback()
                || v6.is_multicast()
                || matches!(v6.segments(), [0xfc00..=0xfdff, ..])
                || matches!(v6.segments(), [0xfe80..=0xfebf, ..])
        }
    }
}

fn format_search_response(response: &SearchResponse) -> String {
    let mut output = format!(
        "ok:\nprovider: {}\nquery: {}\ncount: {}\n",
        response.provider,
        response.query,
        response.results.len()
    );
    for (index, result) in response.results.iter().enumerate() {
        output.push_str(&format!(
            "\n{}. {}\nURL: {}\n",
            index + 1,
            result.title,
            result.url
        ));
        if let Some(snippet) = &result.snippet {
            output.push_str(&format!("Snippet: {}\n", cap_text(snippet.trim(), 800)));
        }
    }
    output
}

fn brave_api_key() -> Result<String, String> {
    parse_env_key(&["BRAVE_SEARCH_API_KEY", "BRAVE_API_KEY"])
        .ok_or_else(|| BRAVE_API_KEY_SETUP_HELP.to_string())
}

fn tavily_api_key() -> Result<String, String> {
    parse_env_key(&["TAVILY_API_KEY"])
        .ok_or_else(|| "Tavily search requires TAVILY_API_KEY.".to_string())
}

fn parse_env_key(names: &[&str]) -> Option<String> {
    names.iter().find_map(|name| {
        std::env::var(name)
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
    })
}

fn http_client(timeout_ms: u64) -> Result<Client, String> {
    Client::builder()
        .timeout(Duration::from_millis(timeout_ms.min(HARD_TIMEOUT_MS)))
        .user_agent(USER_AGENT_VALUE)
        .build()
        .map_err(|err| format!("failed to build HTTP client: {err}"))
}

fn response_headers(headers: &HeaderMap) -> BTreeMap<String, String> {
    headers
        .iter()
        .filter_map(|(name, value)| {
            value
                .to_str()
                .ok()
                .map(|value| (name.as_str().to_ascii_lowercase(), value.to_string()))
        })
        .collect()
}

fn first_http_url(prompt: &str) -> Option<&str> {
    prompt.split_whitespace().find_map(|token| {
        let token = token.trim_matches(|ch: char| {
            matches!(
                ch,
                '"' | '\'' | '`' | '<' | '>' | ')' | '(' | '[' | ']' | '{' | '}'
            )
        });
        let token = token.trim_end_matches(['.', ',', ';', ':', '!', '?']);
        (token.starts_with("http://") || token.starts_with("https://")).then_some(token)
    })
}

fn prompt_needs_search(prompt: &str) -> bool {
    let normalized = normalize_prompt(prompt);
    if normalized.is_empty() || matches!(normalized.as_str(), "hi" | "hello") {
        return false;
    }
    [
        "latest",
        "recent",
        "news",
        "today",
        "current version",
        "current release",
        "current price",
        "look up",
        "lookup",
        "search web",
        "web search",
        "search the web",
        "find online",
        "on the internet",
    ]
    .iter()
    .any(|phrase| normalized.contains(phrase))
}

fn normalize_prompt(prompt: &str) -> String {
    prompt
        .trim()
        .to_ascii_lowercase()
        .chars()
        .map(|ch| if ch.is_ascii_punctuation() { ' ' } else { ch })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn body_has_html(body: &str) -> bool {
    let lower = body
        .chars()
        .take(500)
        .collect::<String>()
        .to_ascii_lowercase();
    lower.contains("<html") || lower.contains("<body") || lower.contains("<article")
}

fn html_to_text(html: &str) -> String {
    let no_scripts = remove_tag_blocks(html, "script");
    let no_styles = remove_tag_blocks(&no_scripts, "style");
    let no_tags = strip_html_tags(&no_styles);
    collapse_whitespace(&decode_entities(&no_tags))
}

fn remove_tag_blocks(input: &str, tag: &str) -> String {
    let lower = input.to_ascii_lowercase();
    let open = format!("<{tag}");
    let close = format!("</{tag}>");
    let mut output = String::new();
    let mut index = 0usize;
    while let Some(start_offset) = lower[index..].find(&open) {
        let start = index + start_offset;
        output.push_str(&input[index..start]);
        let Some(end_offset) = lower[start..].find(&close) else {
            index = input.len();
            break;
        };
        index = start + end_offset + close.len();
    }
    output.push_str(&input[index..]);
    output
}

fn strip_html_tags(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut in_tag = false;
    for ch in input.chars() {
        match ch {
            '<' => {
                in_tag = true;
                output.push(' ');
            }
            '>' => {
                in_tag = false;
                output.push(' ');
            }
            _ if !in_tag => output.push(ch),
            _ => {}
        }
    }
    collapse_whitespace(&decode_entities(&output))
}

fn collapse_whitespace(input: &str) -> String {
    input.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn decode_entities(input: &str) -> String {
    input
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&nbsp;", " ")
}

#[derive(Debug, Deserialize)]
struct BraveResponse {
    web: Option<BraveWeb>,
}

#[derive(Debug, Deserialize)]
struct BraveWeb {
    results: Vec<BraveResult>,
}

#[derive(Debug, Deserialize)]
struct BraveResult {
    title: String,
    url: String,
    description: Option<String>,
    extra_snippets: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
struct TavilyResponse {
    results: Vec<TavilyResult>,
}

#[derive(Debug, Deserialize)]
struct TavilyResult {
    title: String,
    url: String,
    content: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::env_lock;

    #[test]
    fn detects_url_prompt_context() {
        assert_eq!(
            first_http_url("read https://example.com/docs."),
            Some("https://example.com/docs")
        );
    }

    #[test]
    fn detects_current_info_prompts_but_not_greetings() {
        assert!(prompt_needs_search("what is the latest DeepSeek model?"));
        assert!(prompt_needs_search("look up rust release notes"));
        assert!(prompt_needs_search("search the web for OpenAI news"));
        assert!(!prompt_needs_search("hello"));
        assert!(!prompt_needs_search("what is a config file?"));
    }

    #[test]
    fn parses_search_provider_values() {
        assert_eq!(SearchProvider::parse("").unwrap(), SearchProvider::Brave);
        assert_eq!(
            SearchProvider::parse("BrAvE").unwrap(),
            SearchProvider::Brave
        );
        assert_eq!(
            SearchProvider::parse("tavily").unwrap(),
            SearchProvider::Tavily
        );
        assert!(SearchProvider::parse("duckduckgo").is_err());
    }

    #[test]
    fn search_provider_defaults_to_brave_without_runtime_or_env() {
        let _guard = env_lock();
        let home = tempfile::tempdir().unwrap();
        let old_home = std::env::var_os("HOME");
        std::env::set_var("HOME", home.path());
        std::env::remove_var("DEEPSEEK_SEARCH_PROVIDER");
        assert_eq!(SearchProvider::active().unwrap(), SearchProvider::Brave);
        match old_home {
            Some(home) => std::env::set_var("HOME", home),
            None => std::env::remove_var("HOME"),
        }
    }

    #[test]
    fn brave_key_prefers_search_specific_env() {
        let _guard = env_lock();
        std::env::set_var("BRAVE_SEARCH_API_KEY", " search-key ");
        std::env::set_var("BRAVE_API_KEY", "alias-key");
        assert_eq!(brave_api_key().unwrap(), "search-key");
        std::env::remove_var("BRAVE_SEARCH_API_KEY");
        std::env::remove_var("BRAVE_API_KEY");
    }

    #[test]
    fn brave_key_rejects_missing_and_blank_values() {
        let _guard = env_lock();
        std::env::remove_var("BRAVE_SEARCH_API_KEY");
        std::env::remove_var("BRAVE_API_KEY");
        let missing = brave_api_key().unwrap_err();
        assert!(missing.contains("Brave Search API key is not set"));
        assert!(missing.contains("AI provider or support"));
        assert!(missing.contains("do not include any real API keys"));
        assert!(missing.contains("export BRAVE_SEARCH_API_KEY"));
        assert!(missing.contains("~/.zsh_secrets"));
        assert!(missing.contains("/features"));
        assert!(missing.contains("BRAVE_API_KEY is also accepted"));

        std::env::set_var("BRAVE_SEARCH_API_KEY", " \t\n ");
        let blank = brave_api_key().unwrap_err();
        assert_eq!(blank, missing);
        std::env::remove_var("BRAVE_SEARCH_API_KEY");
    }

    #[test]
    fn web_search_tool_rejects_empty_query_before_network() {
        let result = web_search_tool(&json!({"query":"   "}));
        assert!(result.contains("missing non-empty `query`"));
    }

    #[test]
    fn lossy_prompt_context_warns_and_continues_on_missing_search_key() {
        let _guard = env_lock();
        std::env::set_var("DEEPSEEK_SEARCH_PROVIDER", "brave");
        std::env::remove_var("BRAVE_SEARCH_API_KEY");
        std::env::remove_var("BRAVE_API_KEY");
        let mut warnings = Vec::new();
        let context = web_context_message_for_prompt_lossy(
            "what is the latest Rust stable version?",
            |warning| warnings.push(warning),
        );
        assert!(context.is_none());
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("Brave Search API key is not set"));
        assert!(warnings[0].contains("~/.zsh_secrets"));
        std::env::remove_var("DEEPSEEK_SEARCH_PROVIDER");
    }

    #[test]
    fn clamps_search_result_count() {
        assert_eq!(clamp_search_results(0), 1);
        assert_eq!(clamp_search_results(7), 7);
        assert_eq!(clamp_search_results(50), MAX_SEARCH_RESULTS);
    }

    #[test]
    fn parses_fetch_format_values() {
        assert_eq!(FetchFormat::parse(None).unwrap(), FetchFormat::Markdown);
        assert_eq!(FetchFormat::parse(Some("text")).unwrap(), FetchFormat::Text);
        assert_eq!(FetchFormat::parse(Some("raw")).unwrap(), FetchFormat::Raw);
        assert!(FetchFormat::parse(Some("pdf")).is_err());
    }

    #[test]
    fn fetch_url_tool_rejects_non_http_before_network() {
        let result = fetch_url_tool(&json!({"url":"file:///etc/passwd"}));
        assert!(result.contains("only http:// and https:// URLs are supported"));
    }

    #[test]
    fn clamps_fetch_options() {
        let options = FetchOptions::new(FetchFormat::Text, u64::MAX, u64::MAX);
        assert_eq!(options.max_bytes, HARD_FETCH_MAX_BYTES);
        assert_eq!(options.timeout_ms, HARD_TIMEOUT_MS);
    }

    #[test]
    fn rejects_restricted_ips() {
        assert!(is_restricted_ip(&"127.0.0.1".parse().unwrap()));
        assert!(is_restricted_ip(&"10.0.0.1".parse().unwrap()));
        assert!(is_restricted_ip(&"169.254.169.254".parse().unwrap()));
        assert!(!is_restricted_ip(&"93.184.216.34".parse().unwrap()));
    }

    #[test]
    fn strips_basic_html() {
        let html = "<html><style>x</style><script>bad()</script><body><h1>Hello &amp; welcome</h1><p>Text</p></body></html>";
        let text = html_to_text(html);
        assert!(text.contains("Hello & welcome"));
        assert!(text.contains("Text"));
        assert!(!text.contains("bad"));
    }

    #[test]
    fn parses_brave_response() {
        let raw = r#"{"web":{"results":[{"title":"Title","url":"https://example.com","description":"Snippet"}]}}"#;
        let parsed: BraveResponse = serde_json::from_str(raw).unwrap();
        let item = &parsed.web.unwrap().results[0];
        assert_eq!(item.title, "Title");
        assert_eq!(item.url, "https://example.com");
    }

    #[test]
    fn parses_tavily_response() {
        let raw =
            r#"{"results":[{"title":"Title","url":"https://example.com","content":"Snippet"}]}"#;
        let parsed: TavilyResponse = serde_json::from_str(raw).unwrap();
        assert_eq!(parsed.results[0].content, "Snippet");
    }
}
