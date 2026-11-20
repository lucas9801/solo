use collections::HashMap;
use http_client::CustomHeaders;
use http_client::http::{HeaderName, HeaderValue};

pub mod anthropic;
pub mod anthropic_compatible;
pub mod api_compatible;
pub mod bedrock;
pub mod copilot_chat;
pub mod deepseek;
pub mod google;
pub mod llama_cpp;
pub mod lmstudio;
pub mod mistral;
pub mod ollama;
pub mod open_ai;
pub mod open_ai_compatible;
pub mod open_router;
pub mod openai_subscribed;
pub mod opencode;

pub mod vercel_ai_gateway;
pub mod x_ai;
pub mod x_ai_subscribed;

const COMMON_RESERVED_HEADER_NAMES: &[&str] = &["Authorization", "Content-Type", "Accept"];

/// Validate the user-supplied custom-headers map once at settings load time,
/// dropping reserved or malformed entries (each with a `log::warn!`) and
/// returning a typed `CustomHeaders` ready to be appended to outgoing requests.
pub(crate) fn resolve_custom_headers(
    provider_name: &str,
    settings: &HashMap<String, String>,
    reserved_header_names: &[&str],
) -> CustomHeaders {
    let headers = settings
        .iter()
        .filter_map(|(name, value)| {
            if COMMON_RESERVED_HEADER_NAMES
                .iter()
                .chain(reserved_header_names)
                .any(|reserved| reserved.eq_ignore_ascii_case(name))
            {
                log::warn!(
                    "ignoring custom {provider_name} header `{name}`: managed by Zed and cannot be overridden"
                );
                return None;
            }
            let header_name = match name.parse::<HeaderName>() {
                Ok(header_name) => header_name,
                Err(err) => {
                    log::warn!("ignoring custom {provider_name} header `{name}`: invalid header name ({err})");
                    return None;
                }
            };
            let header_value = match HeaderValue::from_str(value) {
                Ok(header_value) => header_value,
                Err(err) => {
                    log::warn!(
                        "ignoring custom {provider_name} header `{name}`: invalid header value ({err})"
                    );
                    return None;
                }
            };
            Some((header_name, header_value))
        })
        .collect();
    CustomHeaders::new(headers)
}

/// SOLO: Local servers (llama.cpp, LM Studio) used to be probed on their
/// default localhost ports at every start, logging a connection error for
/// everyone who doesn't run one (Solo's default: no AI model). They now look
/// for models on their own only when the user's settings name the provider: a
/// `language_models` section for it, or an agent model picked from it. Without
/// that, discovery waits for something that needs the models to authenticate
/// the provider (the agent, the settings page, commit messages), as Ollama does.
pub(crate) fn user_settings_name_provider(
    provider_id: &str,
    has_section: impl FnOnce(&settings::AllLanguageModelSettingsContent) -> bool,
    cx: &gpui::App,
) -> bool {
    let Some(user_settings) = cx.global::<settings::SettingsStore>().raw_user_settings() else {
        return false;
    };
    let content = &user_settings.content;
    if content.language_models.as_ref().is_some_and(has_section) {
        return true;
    }
    content.agent.as_ref().is_some_and(|agent| {
        agent
            .default_model
            .iter()
            .chain(agent.subagent_model.iter())
            .chain(agent.inline_assistant_model.iter())
            .chain(agent.commit_message_model.iter())
            .chain(agent.thread_summary_model.iter())
            .chain(agent.compaction_model.iter())
            .chain(agent.inline_alternatives.iter().flatten())
            .chain(agent.favorite_models.iter())
            .any(|selection| selection.provider.0 == provider_id)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::{StreamExt, stream};
    use language_model::stream_in_background;

    #[gpui::test]
    async fn test_stream_in_background(cx: &mut gpui::TestAppContext) {
        let events = stream::iter([1, 2, 3])
            .flat_map(|event| stream::iter([event, event * 10]))
            .boxed();
        let mapped_events = stream_in_background(events, cx.background_executor.clone())
            .collect::<Vec<_>>()
            .await;

        assert_eq!(mapped_events, [1, 10, 2, 20, 3, 30]);
    }

    fn map(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect()
    }

    fn names(headers: &CustomHeaders) -> Vec<String> {
        let mut names: Vec<String> = headers
            .iter()
            .map(|(name, _)| name.as_str().to_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn drops_common_and_provider_reserved_headers() {
        let settings = map(&[
            ("Authorization", "Bearer leak"),
            ("Content-Type", "text/plain"),
            ("Accept", "text/plain"),
            ("X-Api-Key", "leak"),
            ("X-Allowed", "yes"),
        ]);
        let merged = resolve_custom_headers("Test", &settings, &["X-Api-Key"]);
        assert_eq!(names(&merged), vec!["x-allowed".to_string()]);
    }

    #[test]
    fn reserved_header_match_is_case_insensitive() {
        let settings = map(&[
            ("authorization", "Bearer leak"),
            ("CONTENT-TYPE", "text/plain"),
            ("x-api-key", "leak"),
            ("X-Allowed", "yes"),
        ]);
        let merged = resolve_custom_headers("Test", &settings, &["X-Api-Key"]);
        assert_eq!(names(&merged), vec!["x-allowed".to_string()]);
    }

    #[test]
    fn headers_with_reserved_prefix_are_kept() {
        let settings = map(&[("Authorization-Forwarded", "ok"), ("X-Api-Key-Trace", "ok")]);
        let merged = resolve_custom_headers("Test", &settings, &["X-Api-Key"]);
        assert_eq!(
            names(&merged),
            vec![
                "authorization-forwarded".to_string(),
                "x-api-key-trace".to_string(),
            ]
        );
    }

    #[test]
    fn drops_invalid_header_name_and_value() {
        let settings = map(&[
            ("Bad Name", "ok"),
            ("X-Bad-Value", "line1\nline2"),
            ("X-Allowed", "yes"),
        ]);
        let merged = resolve_custom_headers("Test", &settings, &[]);
        assert_eq!(names(&merged), vec!["x-allowed".to_string()]);
    }
}
