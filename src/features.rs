use crate::provider;

const BRAVE_SEARCH_API_KEY: &str = "BRAVE_SEARCH_API_KEY";
const BRAVE_API_KEY: &str = "BRAVE_API_KEY";
const TAVILY_API_KEY: &str = "TAVILY_API_KEY";
const SEARCH_PROVIDER: &str = "DEEPSEEK_SEARCH_PROVIDER";

pub(crate) fn features_dashboard() -> String {
    format_features_dashboard(FeatureEnv {
        deepseek_api_key: env_is_set(provider::ENV_KEY),
        brave_search_api_key: env_is_set(BRAVE_SEARCH_API_KEY),
        brave_api_key: env_is_set(BRAVE_API_KEY),
        tavily_api_key: env_is_set(TAVILY_API_KEY),
        search_provider: std::env::var(SEARCH_PROVIDER).ok(),
    })
}

#[derive(Debug, Clone, Default)]
struct FeatureEnv {
    deepseek_api_key: bool,
    brave_search_api_key: bool,
    brave_api_key: bool,
    tavily_api_key: bool,
    search_provider: Option<String>,
}

fn format_features_dashboard(env: FeatureEnv) -> String {
    let provider = search_provider(env.search_provider.as_deref());
    let brave_set = env.brave_search_api_key || env.brave_api_key;
    let tavily_set = env.tavily_api_key;
    let selected_search_ready = match provider.as_str() {
        "tavily" => tavily_set,
        provider if provider.starts_with("invalid:") => false,
        _ => brave_set,
    };
    let internet_status = if provider.starts_with("invalid:") {
        "misconfigured"
    } else if selected_search_ready {
        "enabled"
    } else {
        "partial"
    };

    format!(
        "Features\n\nDeepSeek\n  status        {}\n  env           {}: {}\n  enables       chat, streaming, agent mode\n\nInternet\n  status        {internet_status}\n  provider      {provider}\n  env           {BRAVE_SEARCH_API_KEY}: {}\n                {BRAVE_API_KEY}: {}\n                {TAVILY_API_KEY}: {}\n                {SEARCH_PROVIDER}: {}\n  enables       web_search, current-info prefetch\n  notes         Search requires the selected provider key. fetch_url works without a search API key.\n\nFetch URL\n  status        enabled\n  env           none required\n  enables       URL summaries and agent fetch_url\n  limits        HTTP(S) only, restricted IPs blocked\n",
        status_word(env.deepseek_api_key),
        provider::ENV_KEY,
        set_word(env.deepseek_api_key),
        set_word(env.brave_search_api_key),
        set_word(env.brave_api_key),
        set_word(tavily_set),
        provider_env_word(env.search_provider.as_deref()),
    )
}

fn search_provider(value: Option<&str>) -> String {
    match value {
        Some(value) if value.trim().is_empty() => "brave".to_string(),
        Some(value) if value.trim().eq_ignore_ascii_case("brave") => "brave".to_string(),
        Some(value) if value.trim().eq_ignore_ascii_case("tavily") => "tavily".to_string(),
        Some(value) => format!("invalid: {}", value.trim()),
        _ => "brave".to_string(),
    }
}

fn provider_env_word(value: Option<&str>) -> &'static str {
    if value.map(|value| !value.trim().is_empty()).unwrap_or(false) {
        "set"
    } else {
        "default: brave"
    }
}

fn env_is_set(name: &str) -> bool {
    std::env::var(name)
        .ok()
        .map(|value| !value.trim().is_empty())
        .unwrap_or(false)
}

fn status_word(enabled: bool) -> &'static str {
    if enabled {
        "enabled"
    } else {
        "disabled"
    }
}

fn set_word(enabled: bool) -> &'static str {
    if enabled {
        "set"
    } else {
        "missing"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dashboard_reports_missing_keys_without_values() {
        let output = format_features_dashboard(FeatureEnv::default());
        assert!(output.contains("DeepSeek"));
        assert!(output.contains("DEEPSEEK_API_KEY: missing"));
        assert!(output.contains("provider      brave"));
        assert!(output.contains("BRAVE_SEARCH_API_KEY: missing"));
        assert!(output.contains("status        partial"));
    }

    #[test]
    fn dashboard_reports_selected_provider_keys() {
        let output = format_features_dashboard(FeatureEnv {
            deepseek_api_key: true,
            tavily_api_key: true,
            search_provider: Some("tavily".to_string()),
            ..FeatureEnv::default()
        });
        assert!(output.contains("DEEPSEEK_API_KEY: set"));
        assert!(output.contains("provider      tavily"));
        assert!(output.contains("TAVILY_API_KEY: set"));
        assert!(output.contains("status        enabled"));
    }

    #[test]
    fn dashboard_reports_invalid_search_provider() {
        let output = format_features_dashboard(FeatureEnv {
            search_provider: Some("duckduckgo".to_string()),
            brave_search_api_key: true,
            ..FeatureEnv::default()
        });
        assert!(output.contains("provider      invalid: duckduckgo"));
        assert!(output.contains("status        misconfigured"));
    }
}
