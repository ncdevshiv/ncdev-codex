use crate::auth::AuthProvider;
use crate::common::ResponseStream;
use crate::common::ResponsesApiRequest;
use crate::endpoint::responses::ResponsesOptions;
use crate::endpoint::session::EndpointSession;
use crate::error::ApiError;
use crate::provider::Provider;
use crate::requests::headers::build_conversation_headers;
use crate::requests::headers::insert_header;
use crate::requests::headers::subagent_header;
use crate::requests::responses::Compression;
use crate::sse::spawn_chat_response_stream;
use crate::telemetry::SseTelemetry;
use codex_client::HttpTransport;
use codex_client::RequestCompression;
use codex_client::RequestTelemetry;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;
use http::HeaderMap;
use http::HeaderValue;
use http::Method;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;
use std::sync::Arc;
use std::sync::OnceLock;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

/// An OpenAI-style tool call returned by the assistant.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatToolCall {
    pub id: String,
    #[serde(rename = "type")]
    pub call_type: String,
    pub function: ChatToolCallFunction,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatToolCallFunction {
    pub name: String,
    pub arguments: String,
}

/// A chat message that may include tool calls (used for assistant messages in
/// the conversation history).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessageWithToolCalls {
    pub role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ChatToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatCompletionRequest {
    pub model: String,
    pub messages: Vec<ChatMessageWithToolCalls>,
    pub stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream_options: Option<StreamOptions>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamOptions {
    pub include_usage: bool,
}

pub struct ChatClient<T: HttpTransport, A: AuthProvider> {
    session: EndpointSession<T, A>,
    sse_telemetry: Option<Arc<dyn SseTelemetry>>,
}

impl<T: HttpTransport, A: AuthProvider> ChatClient<T, A> {
    pub fn new(transport: T, provider: Provider, auth: A) -> Self {
        Self {
            session: EndpointSession::new(transport, provider, auth),
            sse_telemetry: None,
        }
    }

    pub fn with_telemetry(
        self,
        request: Option<Arc<dyn RequestTelemetry>>,
        sse: Option<Arc<dyn SseTelemetry>>,
    ) -> Self {
        Self {
            session: self.session.with_request_telemetry(request),
            sse_telemetry: sse,
        }
    }

    pub async fn stream_request(
        &self,
        request: ResponsesApiRequest,
        options: ResponsesOptions,
    ) -> Result<ResponseStream, ApiError> {
        let ResponsesOptions {
            conversation_id,
            session_source,
            extra_headers,
            compression,
            turn_state,
        } = options;

        let mut messages: Vec<ChatMessageWithToolCalls> = Vec::new();

        if !request.instructions.is_empty() {
            messages.push(ChatMessageWithToolCalls {
                role: "system".to_string(),
                content: Some(request.instructions.clone()),
                tool_calls: None,
                tool_call_id: None,
            });
        }

        for input in &request.input {
            match input {
                ResponseItem::Message { role, content, .. } => {
                    let text_content = content
                        .iter()
                        .filter_map(|c| match c {
                            ContentItem::InputText { text } => Some(text.clone()),
                            ContentItem::OutputText { text } => Some(text.clone()),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join("\n");

                    messages.push(ChatMessageWithToolCalls {
                        role: role.clone(),
                        content: Some(text_content),
                        tool_calls: None,
                        tool_call_id: None,
                    });
                }
                ResponseItem::FunctionCall {
                    name,
                    arguments,
                    call_id,
                    ..
                } => {
                    // Emit the assistant message with tool_calls
                    messages.push(ChatMessageWithToolCalls {
                        role: "assistant".to_string(),
                        content: None,
                        tool_calls: Some(vec![ChatToolCall {
                            id: call_id.clone(),
                            call_type: "function".to_string(),
                            function: ChatToolCallFunction {
                                name: name.clone(),
                                arguments: arguments.clone(),
                            },
                        }]),
                        tool_call_id: None,
                    });
                }
                ResponseItem::FunctionCallOutput {
                    call_id, output, ..
                } => {
                    let output_text = output.text_content().unwrap_or("").to_string();
                    messages.push(ChatMessageWithToolCalls {
                        role: "tool".to_string(),
                        content: Some(output_text),
                        tool_calls: None,
                        tool_call_id: Some(call_id.clone()),
                    });
                }
                _ => {}
            }
        }

        // Convert Responses API tools to Chat API tools format.
        // The Responses API uses: { "type": "function", "name": "...", "description": "...", "parameters": {...} }
        // Chat API uses: { "type": "function", "function": { "name": "...", "description": "...", "parameters": {...} } }
        let chat_tools: Vec<Value> = request
            .tools
            .iter()
            .filter_map(|tool| {
                let obj = tool.as_object()?;
                let tool_type = obj.get("type")?.as_str()?;
                if tool_type == "function" {
                    // Extract fields and wrap in Chat API format
                    let name = obj.get("name")?.clone();
                    let description = obj.get("description").cloned();
                    let parameters = obj.get("parameters").cloned();
                    let strict = obj.get("strict").cloned();

                    let mut func = serde_json::Map::new();
                    func.insert("name".to_string(), name);
                    if let Some(desc) = description {
                        func.insert("description".to_string(), desc);
                    }
                    if let Some(params) = parameters {
                        func.insert("parameters".to_string(), params);
                    }
                    if let Some(strict) = strict {
                        func.insert("strict".to_string(), strict);
                    }

                    let mut chat_tool = serde_json::Map::new();
                    chat_tool.insert("type".to_string(), Value::String("function".to_string()));
                    chat_tool.insert("function".to_string(), Value::Object(func));
                    Some(Value::Object(chat_tool))
                } else {
                    None
                }
            })
            .collect();

        let chat_request = ChatCompletionRequest {
            model: request.model.clone(),
            messages,
            stream: request.stream,
            max_tokens: None,
            tools: chat_tools,
            stream_options: Some(StreamOptions {
                include_usage: true,
            }),
        };

        let body = serde_json::to_value(&chat_request)
            .map_err(|e| ApiError::Stream(format!("failed to encode chat request: {e}")))?;

        let mut headers = extra_headers;
        headers.extend(build_conversation_headers(conversation_id));
        if let Some(subagent) = subagent_header(&session_source) {
            insert_header(&mut headers, "x-openai-subagent", &subagent);
        }

        self.stream(body, headers, compression, turn_state).await
    }

    fn path() -> &'static str {
        "chat/completions"
    }

    pub async fn stream(
        &self,
        body: Value,
        extra_headers: HeaderMap,
        compression: Compression,
        turn_state: Option<Arc<OnceLock<String>>>,
    ) -> Result<ResponseStream, ApiError> {
        let request_compression = match compression {
            Compression::None => RequestCompression::None,
            Compression::Zstd => RequestCompression::Zstd,
        };

        let stream_response = self
            .session
            .stream_with(
                Method::POST,
                Self::path(),
                extra_headers,
                Some(body),
                |req| {
                    req.headers.insert(
                        http::header::ACCEPT,
                        HeaderValue::from_static("text/event-stream"),
                    );
                    req.compression = request_compression;
                },
            )
            .await?;

        Ok(spawn_chat_response_stream(
            stream_response,
            self.session.provider().stream_idle_timeout,
            self.sse_telemetry.clone(),
            turn_state,
        ))
    }
}
