use crate::common::ResponseEvent;
use crate::common::ResponseStream;
use crate::error::ApiError;
use crate::telemetry::SseTelemetry;
use codex_client::ByteStream;
use codex_client::StreamResponse;
use codex_protocol::models::{ContentItem, ResponseItem};
use codex_protocol::protocol::TokenUsage;
use eventsource_stream::Eventsource;
use futures::StreamExt;
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::OnceLock;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::Instant;
use tokio::time::timeout;
use tracing::{debug, trace};

#[derive(Debug, Deserialize)]
pub struct ChatCompletionChunk {
    pub id: String,
    pub choices: Vec<ChatCompletionChoice>,
    #[serde(default)]
    pub usage: Option<ChatCompletionUsage>,
}

#[derive(Debug, Deserialize)]
pub struct ChatCompletionChoice {
    pub delta: ChatCompletionDelta,
    #[serde(default)]
    pub finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ChatCompletionDelta {
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default)]
    pub tool_calls: Option<Vec<ToolCallDelta>>,
    /// Custom field used by DeepSeek-R1 and other reasoning models.
    #[serde(default)]
    pub reasoning_content: Option<String>,
    /// Another common alias for reasoning content.
    #[serde(default)]
    pub reasoning: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ToolCallDelta {
    pub index: usize,
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub function: Option<ToolCallFunctionDelta>,
}

#[derive(Debug, Deserialize)]
pub struct ToolCallFunctionDelta {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub arguments: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ChatCompletionUsage {
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub total_tokens: i64,
}

impl From<ChatCompletionUsage> for TokenUsage {
    fn from(val: ChatCompletionUsage) -> Self {
        TokenUsage {
            input_tokens: val.prompt_tokens,
            cached_input_tokens: 0,
            output_tokens: val.completion_tokens,
            reasoning_output_tokens: 0,
            total_tokens: val.total_tokens,
        }
    }
}

/// Tracks the accumulated state for a single in-progress tool call.
#[derive(Debug, Default)]
struct ToolCallAccumulator {
    id: String,
    name: String,
    arguments: String,
}

pub fn spawn_chat_response_stream(
    stream_response: StreamResponse,
    idle_timeout: Duration,
    telemetry: Option<Arc<dyn SseTelemetry>>,
    turn_state: Option<Arc<OnceLock<String>>>,
) -> ResponseStream {
    if let Some(turn_state) = turn_state.as_ref()
        && let Some(header_value) = stream_response
            .headers
            .get("x-codex-turn-state")
            .and_then(|v| v.to_str().ok())
    {
        let _ = turn_state.set(header_value.to_string());
    }

    let (tx_event, rx_event) = mpsc::channel::<Result<ResponseEvent, ApiError>>(1600);

    tokio::spawn(async move {
        // Send created event
        let _ = tx_event.send(Ok(ResponseEvent::Created {})).await;

        process_chat_sse(stream_response.bytes, tx_event, idle_timeout, telemetry).await;
    });

    ResponseStream { rx_event }
}

pub async fn process_chat_sse(
    stream: ByteStream,
    tx_event: mpsc::Sender<Result<ResponseEvent, ApiError>>,
    idle_timeout: Duration,
    telemetry: Option<Arc<dyn SseTelemetry>>,
) {
    let mut stream = stream.eventsource();
    let mut response_id = String::new();
    let mut last_usage: Option<TokenUsage> = None;
    let mut accumulated_content = String::new();
    let mut item_added_emitted = false;
    let mut tool_calls: HashMap<usize, ToolCallAccumulator> = HashMap::new();

    // Reasoning tracking
    let mut accumulated_reasoning = String::new();
    let mut in_think_block = false;

    loop {
        let start = Instant::now();
        let response = timeout(idle_timeout, stream.next()).await;

        if let Some(t) = telemetry.as_ref() {
            t.on_sse_poll(&response, start.elapsed());
        }

        let sse = match response {
            Ok(Some(Ok(sse))) => sse,
            Ok(Some(Err(e))) => {
                debug!("SSE Error: {e:#}");
                let _ = tx_event.send(Err(ApiError::Stream(e.to_string()))).await;
                return;
            }
            Ok(None) => {
                // Stream ended — finalize
                finalize_stream(
                    &tx_event,
                    &response_id,
                    &accumulated_content,
                    &accumulated_reasoning,
                    &tool_calls,
                    last_usage,
                    item_added_emitted,
                )
                .await;
                return;
            }
            Err(_) => {
                let _ = tx_event
                    .send(Err(ApiError::Stream("idle timeout waiting for SSE".into())))
                    .await;
                return;
            }
        };

        let data = sse.data.trim();
        if data == "[DONE]" {
            finalize_stream(
                &tx_event,
                &response_id,
                &accumulated_content,
                &accumulated_reasoning,
                &tool_calls,
                last_usage,
                item_added_emitted,
            )
            .await;
            return;
        }

        trace!("SSE event: {}", data);

        let chunk: ChatCompletionChunk = match serde_json::from_str(data) {
            Ok(chunk) => chunk,
            Err(e) => {
                debug!("Failed to parse Chat SSE chunk: {e}, data: {data}");
                continue;
            }
        };

        if response_id.is_empty() {
            response_id = chunk.id.clone();
        }

        if let Some(usage) = chunk.usage {
            last_usage = Some(usage.into());
        }

        // Before processing choice deltas, ensure we have an active item if there is ANY delta
        if !item_added_emitted && (!chunk.choices.is_empty()) {
            item_added_emitted = true;
            let item = ResponseItem::Message {
                id: Some(format!("msg_{}", response_id)),
                role: "assistant".to_string(),
                content: vec![ContentItem::OutputText {
                    text: String::new(),
                }],
                end_turn: None,
                phase: None,
            };
            if tx_event
                .send(Ok(ResponseEvent::OutputItemAdded(item)))
                .await
                .is_err()
            {
                return;
            }
        }

        for choice in chunk.choices {
            // Handle explicit reasoning fields (DeepSeek/o1 style)
            let reasoning_delta = choice.delta.reasoning_content.or(choice.delta.reasoning);
            if let Some(rd) = reasoning_delta {
                accumulated_reasoning.push_str(&rd);
                // Use ReasoningSummaryDelta as it's typically mapped to the thinking block UI
                let _ = tx_event
                    .send(Ok(ResponseEvent::ReasoningSummaryDelta {
                        delta: rd,
                        summary_index: 0,
                    }))
                    .await;
            }

            // Handle text content deltas
            if let Some(mut content) = choice.delta.content {
                let mut output_content = String::new();

                // State machine to strip <think> tags in-flight
                while !content.is_empty() {
                    if !in_think_block {
                        if let Some(start_pos) = content.find("<think>") {
                            // Text before <think> is normal content
                            let before = &content[..start_pos];
                            if !before.is_empty() {
                                output_content.push_str(before);
                            }
                            in_think_block = true;
                            content = content[start_pos + 7..].to_string();
                        } else {
                            // No <think> tag found, all content is normal
                            output_content.push_str(&content);
                            content = String::new();
                        }
                    } else {
                        // We are in a think block
                        if let Some(end_pos) = content.find("</think>") {
                            // Text before </think> is reasoning
                            let reasoning = &content[..end_pos];
                            if !reasoning.is_empty() {
                                accumulated_reasoning.push_str(reasoning);
                                let _ = tx_event
                                    .send(Ok(ResponseEvent::ReasoningSummaryDelta {
                                        delta: reasoning.to_string(),
                                        summary_index: 0,
                                    }))
                                    .await;
                            }
                            in_think_block = false;
                            content = content[end_pos + 8..].to_string();
                        } else {
                            // No </think> tag found, all content is reasoning
                            accumulated_reasoning.push_str(&content);
                            let _ = tx_event
                                .send(Ok(ResponseEvent::ReasoningSummaryDelta {
                                    delta: content,
                                    summary_index: 0,
                                }))
                                .await;
                            content = String::new();
                        }
                    }
                }

                if !output_content.is_empty() {
                    accumulated_content.push_str(&output_content);
                    if tx_event
                        .send(Ok(ResponseEvent::OutputTextDelta(output_content)))
                        .await
                        .is_err()
                    {
                        return;
                    }
                }
            }

            // Handle tool call deltas
            if let Some(tc_deltas) = choice.delta.tool_calls {
                for tc_delta in tc_deltas {
                    let acc = tool_calls.entry(tc_delta.index).or_default();
                    if let Some(id) = tc_delta.id {
                        acc.id = id;
                    }
                    if let Some(func) = tc_delta.function {
                        if let Some(name) = func.name {
                            acc.name = name;
                        }
                        if let Some(args) = func.arguments {
                            acc.arguments.push_str(&args);
                        }
                    }
                }
            }
        }
    }
}

/// Finalize the stream by emitting OutputItemDone for text and/or tool calls,
/// then Completed.
async fn finalize_stream(
    tx_event: &mpsc::Sender<Result<ResponseEvent, ApiError>>,
    response_id: &str,
    accumulated_content: &str,
    accumulated_reasoning: &str,
    tool_calls: &HashMap<usize, ToolCallAccumulator>,
    last_usage: Option<TokenUsage>,
    item_added_emitted: bool,
) {
    if response_id.is_empty() {
        let _ = tx_event
            .send(Err(ApiError::Stream("stream closed without chunks".into())))
            .await;
        return;
    }

    // If we had text content, emit the completed message
    if item_added_emitted {
        let msg_item = ResponseItem::Message {
            id: None,
            role: "assistant".to_string(),
            content: vec![ContentItem::OutputText {
                text: accumulated_content.to_string(),
            }],
            end_turn: Some(tool_calls.is_empty()),
            phase: None,
        };
        let _ = tx_event
            .send(Ok(ResponseEvent::OutputItemDone(msg_item)))
            .await;
    }

    // Emit each accumulated tool call as a separate FunctionCall item
    if !tool_calls.is_empty() {
        let mut sorted_indices: Vec<&usize> = tool_calls.keys().collect();
        sorted_indices.sort();

        for idx in sorted_indices {
            let tc = &tool_calls[idx];
            // Emit OutputItemAdded for the function call
            let fc_item = ResponseItem::FunctionCall {
                id: None,
                name: tc.name.clone(),
                arguments: tc.arguments.clone(),
                call_id: tc.id.clone(),
            };
            let _ = tx_event
                .send(Ok(ResponseEvent::OutputItemAdded(fc_item.clone())))
                .await;
            let _ = tx_event
                .send(Ok(ResponseEvent::OutputItemDone(fc_item)))
                .await;
        }
    }

    let _ = tx_event
        .send(Ok(ResponseEvent::Completed {
            response_id: response_id.to_string(),
            token_usage: last_usage,
        }))
        .await;
}
