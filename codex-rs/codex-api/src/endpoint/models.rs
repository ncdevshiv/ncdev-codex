use crate::auth::AuthProvider;
use crate::endpoint::session::EndpointSession;
use crate::error::ApiError;
use crate::provider::Provider;
use codex_client::HttpTransport;
use codex_client::RequestTelemetry;
use codex_protocol::config_types::ReasoningSummary;
use codex_protocol::openai_models::ConfigShellToolType;
use codex_protocol::openai_models::ModelInfo;
use codex_protocol::openai_models::ModelVisibility;
use codex_protocol::openai_models::ModelsResponse;
use codex_protocol::openai_models::ReasoningEffort;
use codex_protocol::openai_models::ReasoningEffortPreset;
use codex_protocol::openai_models::TruncationPolicyConfig;
use codex_protocol::openai_models::WebSearchToolType;
use http::HeaderMap;
use http::Method;
use http::header::ETAG;
use serde::Deserialize;
use std::sync::Arc;

pub struct ModelsClient<T: HttpTransport, A: AuthProvider> {
    session: EndpointSession<T, A>,
}

impl<T: HttpTransport, A: AuthProvider> ModelsClient<T, A> {
    pub fn new(transport: T, provider: Provider, auth: A) -> Self {
        Self {
            session: EndpointSession::new(transport, provider, auth),
        }
    }

    pub fn with_telemetry(self, request: Option<Arc<dyn RequestTelemetry>>) -> Self {
        Self {
            session: self.session.with_request_telemetry(request),
        }
    }

    fn path() -> &'static str {
        "models"
    }

    fn append_client_version_query(req: &mut codex_client::Request, client_version: &str) {
        let separator = if req.url.contains('?') { '&' } else { '?' };
        req.url = format!("{}{}client_version={client_version}", req.url, separator);
    }

    pub async fn list_models(
        &self,
        client_version: &str,
        extra_headers: HeaderMap,
    ) -> Result<(Vec<ModelInfo>, Option<String>), ApiError> {
        let resp = self
            .session
            .execute_with(Method::GET, Self::path(), extra_headers, None, |req| {
                Self::append_client_version_query(req, client_version);
            })
            .await?;

        let header_etag = resp
            .headers
            .get(ETAG)
            .and_then(|value| value.to_str().ok())
            .map(ToString::to_string);

        let models = decode_models_response(&resp.body)?;

        Ok((models, header_etag))
    }
}

#[derive(Debug, Deserialize)]
struct OpenAiCompatibleModelsResponse {
    data: Vec<OpenAiCompatibleModelInfo>,
}

#[derive(Debug, Deserialize)]
struct OpenAiCompatibleModelInfo {
    id: String,
}

fn decode_models_response(body: &[u8]) -> Result<Vec<ModelInfo>, ApiError> {
    if let Ok(ModelsResponse { models }) = serde_json::from_slice::<ModelsResponse>(body) {
        return Ok(models);
    }

    if let Ok(OpenAiCompatibleModelsResponse { data }) =
        serde_json::from_slice::<OpenAiCompatibleModelsResponse>(body)
    {
        let models = data
            .into_iter()
            .enumerate()
            .filter_map(|(index, model)| {
                let slug = model.id.trim().to_string();
                (!slug.is_empty()).then(|| openai_compatible_model_info(slug, index as i32))
            })
            .collect();
        return Ok(models);
    }

    Err(ApiError::Stream(format!(
        "failed to decode models response; body: {}",
        String::from_utf8_lossy(body)
    )))
}

fn openai_compatible_model_info(slug: String, priority: i32) -> ModelInfo {
    ModelInfo {
        display_name: slug.clone(),
        slug,
        description: None,
        default_reasoning_level: Some(ReasoningEffort::Medium),
        supported_reasoning_levels: vec![
            ReasoningEffortPreset {
                effort: ReasoningEffort::Low,
                description: "Low reasoning effort".to_string(),
            },
            ReasoningEffortPreset {
                effort: ReasoningEffort::Medium,
                description: "Medium reasoning effort".to_string(),
            },
            ReasoningEffortPreset {
                effort: ReasoningEffort::High,
                description: "High reasoning effort".to_string(),
            },
        ],
        shell_type: ConfigShellToolType::ShellCommand,
        visibility: ModelVisibility::List,
        supported_in_api: true,
        priority,
        availability_nux: None,
        upgrade: None,
        base_instructions: String::new(),
        model_messages: None,
        supports_reasoning_summaries: false,
        default_reasoning_summary: ReasoningSummary::Auto,
        support_verbosity: false,
        default_verbosity: None,
        apply_patch_tool_type: None,
        web_search_tool_type: WebSearchToolType::Text,
        truncation_policy: TruncationPolicyConfig::tokens(256_000),
        supports_parallel_tool_calls: false,
        supports_image_detail_original: false,
        context_window: None,
        auto_compact_token_limit: None,
        effective_context_window_percent: 95,
        experimental_supported_tools: Vec::new(),
        input_modalities: codex_protocol::openai_models::default_input_modalities(),
        prefer_websockets: false,
        used_fallback_model_metadata: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::RetryConfig;
    use async_trait::async_trait;
    use codex_client::Request;
    use codex_client::Response;
    use codex_client::StreamResponse;
    use codex_client::TransportError;
    use http::HeaderMap;
    use http::StatusCode;
    use pretty_assertions::assert_eq;
    use serde_json::json;
    use std::sync::Arc;
    use std::sync::Mutex;
    use std::time::Duration;

    #[derive(Clone)]
    struct CapturingTransport {
        last_request: Arc<Mutex<Option<Request>>>,
        body: Arc<Vec<u8>>,
        etag: Option<String>,
    }

    impl Default for CapturingTransport {
        fn default() -> Self {
            Self {
                last_request: Arc::new(Mutex::new(None)),
                body: Arc::new(
                    serde_json::to_vec(&ModelsResponse { models: Vec::new() })
                        .expect("serialize empty models response"),
                ),
                etag: None,
            }
        }
    }

    #[async_trait]
    impl HttpTransport for CapturingTransport {
        async fn execute(&self, req: Request) -> Result<Response, TransportError> {
            *self.last_request.lock().unwrap() = Some(req);
            let mut headers = HeaderMap::new();
            if let Some(etag) = &self.etag {
                headers.insert(ETAG, etag.parse().unwrap());
            }
            Ok(Response {
                status: StatusCode::OK,
                headers,
                body: self.body.as_ref().clone().into(),
            })
        }

        async fn stream(&self, _req: Request) -> Result<StreamResponse, TransportError> {
            Err(TransportError::Build("stream should not run".to_string()))
        }
    }

    #[derive(Clone, Default)]
    struct DummyAuth;

    impl AuthProvider for DummyAuth {
        fn bearer_token(&self) -> Option<String> {
            None
        }
    }

    fn provider(base_url: &str) -> Provider {
        Provider {
            name: "test".to_string(),
            base_url: base_url.to_string(),
            query_params: None,
            headers: HeaderMap::new(),
            retry: RetryConfig {
                max_attempts: 1,
                base_delay: Duration::from_millis(1),
                retry_429: false,
                retry_5xx: true,
                retry_transport: true,
            },
            stream_idle_timeout: Duration::from_secs(1),
        }
    }

    #[tokio::test]
    async fn appends_client_version_query() {
        let response = ModelsResponse { models: Vec::new() };

        let transport = CapturingTransport {
            last_request: Arc::new(Mutex::new(None)),
            body: Arc::new(serde_json::to_vec(&response).expect("serialize models response")),
            etag: None,
        };

        let client = ModelsClient::new(
            transport.clone(),
            provider("https://example.com/api/codex"),
            DummyAuth,
        );

        let (models, _) = client
            .list_models("0.99.0", HeaderMap::new())
            .await
            .expect("request should succeed");

        assert_eq!(models.len(), 0);

        let url = transport
            .last_request
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .url
            .clone();
        assert_eq!(
            url,
            "https://example.com/api/codex/models?client_version=0.99.0"
        );
    }

    #[tokio::test]
    async fn parses_models_response() {
        let response = ModelsResponse {
            models: vec![
                serde_json::from_value(json!({
                    "slug": "gpt-test",
                    "display_name": "gpt-test",
                    "description": "desc",
                    "default_reasoning_level": "medium",
                    "supported_reasoning_levels": [{"effort": "low", "description": "low"}, {"effort": "medium", "description": "medium"}, {"effort": "high", "description": "high"}],
                    "shell_type": "shell_command",
                    "visibility": "list",
                    "minimal_client_version": [0, 99, 0],
                    "supported_in_api": true,
                    "priority": 1,
                    "upgrade": null,
                    "base_instructions": "base instructions",
                    "supports_reasoning_summaries": false,
                    "support_verbosity": false,
                    "default_verbosity": null,
                    "apply_patch_tool_type": null,
                    "truncation_policy": {"mode": "bytes", "limit": 10_000},
                    "supports_parallel_tool_calls": false,
                    "supports_image_detail_original": false,
                    "context_window": 272_000,
                    "experimental_supported_tools": [],
                }))
                .unwrap(),
            ],
        };

        let transport = CapturingTransport {
            last_request: Arc::new(Mutex::new(None)),
            body: Arc::new(serde_json::to_vec(&response).expect("serialize models response")),
            etag: None,
        };

        let client = ModelsClient::new(
            transport,
            provider("https://example.com/api/codex"),
            DummyAuth,
        );

        let (models, _) = client
            .list_models("0.99.0", HeaderMap::new())
            .await
            .expect("request should succeed");

        assert_eq!(models.len(), 1);
        assert_eq!(models[0].slug, "gpt-test");
        assert_eq!(models[0].supported_in_api, true);
        assert_eq!(models[0].priority, 1);
    }

    #[tokio::test]
    async fn list_models_includes_etag() {
        let response = ModelsResponse { models: Vec::new() };

        let transport = CapturingTransport {
            last_request: Arc::new(Mutex::new(None)),
            body: Arc::new(serde_json::to_vec(&response).expect("serialize models response")),
            etag: Some("\"abc\"".to_string()),
        };

        let client = ModelsClient::new(
            transport,
            provider("https://example.com/api/codex"),
            DummyAuth,
        );

        let (models, etag) = client
            .list_models("0.1.0", HeaderMap::new())
            .await
            .expect("request should succeed");

        assert_eq!(models.len(), 0);
        assert_eq!(etag, Some("\"abc\"".to_string()));
    }

    #[tokio::test]
    async fn parses_openai_compatible_models_response() {
        let response = serde_json::json!({
            "object": "list",
            "data": [
                {"id": "openai/gpt-4o-mini"},
                {"id": "openai/gpt-4o"}
            ]
        });

        let transport = CapturingTransport {
            last_request: Arc::new(Mutex::new(None)),
            body: Arc::new(serde_json::to_vec(&response).expect("serialize compatible response")),
            etag: None,
        };

        let client = ModelsClient::new(transport, provider("https://example.com/v1"), DummyAuth);

        let (models, _) = client
            .list_models("0.99.0", HeaderMap::new())
            .await
            .expect("request should succeed");

        assert_eq!(models.len(), 2);
        assert_eq!(models[0].slug, "openai/gpt-4o-mini");
        assert_eq!(models[1].slug, "openai/gpt-4o");
        assert_eq!(
            models[0].default_reasoning_level,
            Some(ReasoningEffort::Medium)
        );
    }
}
