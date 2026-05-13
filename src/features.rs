use crate::{provider, runtime};

const BRAVE_SEARCH_API_KEY: &str = "BRAVE_SEARCH_API_KEY";
const BRAVE_API_KEY: &str = "BRAVE_API_KEY";
const TAVILY_API_KEY: &str = "TAVILY_API_KEY";
const SEARCH_PROVIDER: &str = "DEEPSEEK_SEARCH_PROVIDER";

pub(crate) fn features_dashboard() -> String {
    let runtime_state = runtime::load(provider::DEFAULT_MODEL).ok();
    features_dashboard_with_runtime(runtime_state.as_ref())
}

pub(crate) fn features_dashboard_with_runtime(state: Option<&runtime::RuntimeState>) -> String {
    format_features_dashboard(FeatureEnv {
        deepseek_api_key: env_is_set(provider::ENV_KEY),
        brave_search_api_key: env_is_set(BRAVE_SEARCH_API_KEY),
        brave_api_key: env_is_set(BRAVE_API_KEY),
        tavily_api_key: env_is_set(TAVILY_API_KEY),
        search_provider: std::env::var(SEARCH_PROVIDER).ok(),
        runtime_search_provider: state.and_then(|state| state.search_provider.clone()),
    })
}

pub(crate) fn toggle_search_provider(model: &str) -> Result<String, String> {
    let state = runtime::load(model)?;
    let active = active_search_provider(
        state.search_provider.as_deref(),
        std::env::var(SEARCH_PROVIDER).ok().as_deref(),
    );
    let next = if active == "tavily" {
        "brave"
    } else {
        "tavily"
    };
    let state = runtime::set_search_provider(model, next)?;
    Ok(features_dashboard_with_runtime(Some(&state)))
}

#[derive(Debug, Clone, Default)]
struct FeatureEnv {
    deepseek_api_key: bool,
    brave_search_api_key: bool,
    brave_api_key: bool,
    tavily_api_key: bool,
    search_provider: Option<String>,
    runtime_search_provider: Option<String>,
}

fn format_features_dashboard(env: FeatureEnv) -> String {
    let provider = search_provider(
        env.runtime_search_provider
            .as_deref()
            .or(env.search_provider.as_deref()),
    );
    let provider_source = provider_source(
        env.runtime_search_provider.as_deref(),
        env.search_provider.as_deref(),
    );
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
        "Features\n\nDeepSeek\n  status        {}\n  env           {}: {}\n  enables       chat, streaming, agent mode\n\nInternet\n  status        {internet_status}\n  provider      {provider}\n  source        {provider_source}\n  toggle        /features toggle\n  env           {BRAVE_SEARCH_API_KEY}: {}\n                {BRAVE_API_KEY}: {}\n                {TAVILY_API_KEY}: {}\n                {SEARCH_PROVIDER}: {}\n  enables       web_search, current-info prefetch\n  notes         Search requires the selected provider key. fetch_url works without a search API key.\n\nFetch URL\n  status        enabled\n  env           none required\n  enables       URL summaries and agent fetch_url\n  limits        HTTP(S) only, restricted IPs blocked\n",
        status_word(env.deepseek_api_key),
        provider::ENV_KEY,
        set_word(env.deepseek_api_key),
        set_word(env.brave_search_api_key),
        set_word(env.brave_api_key),
        set_word(tavily_set),
        provider_env_word(env.search_provider.as_deref()),
    )
}

pub(crate) fn active_search_provider(
    runtime_value: Option<&str>,
    env_value: Option<&str>,
) -> String {
    search_provider(runtime_value.or(env_value))
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

fn provider_source(runtime_value: Option<&str>, env_value: Option<&str>) -> &'static str {
    if runtime_value
        .map(|value| !value.trim().is_empty())
        .unwrap_or(false)
    {
        "runtime"
    } else if env_value
        .map(|value| !value.trim().is_empty())
        .unwrap_or(false)
    {
        "env"
    } else {
        "default"
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
    use crate::test_support::env_lock;

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

    #[test]
    fn dashboard_runtime_provider_overrides_env_provider() {
        let output = format_features_dashboard(FeatureEnv {
            brave_search_api_key: true,
            search_provider: Some("tavily".to_string()),
            runtime_search_provider: Some("brave".to_string()),
            ..FeatureEnv::default()
        });
        assert!(output.contains("provider      brave"));
        assert!(output.contains("source        runtime"));
        assert!(output.contains("toggle        /features toggle"));
        assert!(output.contains("status        enabled"));
    }

    #[test]
    fn active_search_provider_prefers_runtime_then_env_then_brave() {
        assert_eq!(
            active_search_provider(Some("tavily"), Some("brave")),
            "tavily"
        );
        assert_eq!(active_search_provider(None, Some("tavily")), "tavily");
        assert_eq!(active_search_provider(None, None), "brave");
    }

    #[test]
    fn toggle_search_provider_persists_runtime_provider() {
        let _guard = env_lock();
        let home = tempfile::tempdir().unwrap();
        let old_home = std::env::var_os("HOME");
        let old_env = std::env::var("DEEPSEEK_SEARCH_PROVIDER").ok();
        std::env::set_var("HOME", home.path());
        std::env::remove_var("DEEPSEEK_SEARCH_PROVIDER");

        let first = toggle_search_provider("deepseek-v4-flash").unwrap();
        assert!(first.contains("provider      tavily"));
        assert!(first.contains("source        runtime"));

        let second = toggle_search_provider("deepseek-v4-flash").unwrap();
        assert!(second.contains("provider      brave"));
        assert!(second.contains("source        runtime"));

        match old_home {
            Some(home) => std::env::set_var("HOME", home),
            None => std::env::remove_var("HOME"),
        }
        match old_env {
            Some(value) => std::env::set_var("DEEPSEEK_SEARCH_PROVIDER", value),
            None => std::env::remove_var("DEEPSEEK_SEARCH_PROVIDER"),
        }
    }
}
