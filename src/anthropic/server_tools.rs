//! 服务端工具（web_search）与普通客户端工具混用支持
//!
//! [`super::websearch`] 处理「tools 仅含单个 web_search」的快路径（代理直接执行并伪造
//! 整个 assistant 回合）。本模块处理 tools 中 web_search 与普通客户端工具共存的场景，
//! 对齐官方 API 语义：
//!
//! 1. 把 web_search 以合成 schema 作为普通 Kiro 工具注册给模型；
//! 2. 模型发起该工具调用时由代理拦截，走 Kiro MCP 执行；
//! 3. 结果以 toolResult 回填到会话中再次调用 Kiro，循环直到模型给出最终回答
//!    或调用真正的客户端工具（透传给客户端，stop_reason=tool_use）；
//! 4. 对外输出官方格式的 server_tool_use / web_search_tool_result 内容块。
//!
//! 注：Kiro 后端 MCP 只提供 web_search，不提供 web_fetch（实测 Tool not found），
//! 故此处仅支持 web_search。

use std::collections::{HashMap, HashSet};
use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use axum::{
    body::Body,
    http::{StatusCode, header},
    response::{IntoResponse, Json, Response},
};
use bytes::Bytes;
use futures::{Stream, StreamExt, stream};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::kiro::model::events::Event;
use crate::kiro::model::requests::conversation::{
    AssistantMessage, ConversationState, CurrentMessage, HistoryAssistantMessage,
    HistoryUserMessage, Message as KiroMessage, UserInputMessage, UserInputMessageContext,
    UserMessage,
};
use crate::kiro::model::requests::kiro::KiroRequest;
use crate::kiro::model::requests::tool::{
    InputSchema, Tool as KiroTool, ToolResult, ToolSpecification, ToolUseEntry,
};
use crate::kiro::parser::decoder::EventStreamDecoder;
use crate::kiro::provider::KiroProvider;
use crate::token;

use super::cache_sim::CacheSplit;
use super::converter::{ConversionError, convert_request, get_context_window_size};
use super::handlers::{PING_INTERVAL_SECS, create_ping_sse, map_provider_error};
use super::stream::{SseEvent, extract_thinking_from_complete_text};
use super::types::{ErrorResponse, MessagesRequest, Tool};
use super::websearch;

/// server tool 名称
const WEB_SEARCH: &str = "web_search";

/// 拦截循环轮数硬上限（防止模型反复搜索不收敛）
const MAX_ROUNDS: usize = 12;
/// 单个 server tool 默认最大调用次数（客户端未指定 max_uses 时）
const DEFAULT_MAX_USES: i32 = 8;
/// 文本块 SSE 增量分块大小（字节）
const TEXT_DELTA_CHUNK_BYTES: usize = 2000;

/// 按字节安全分块推送 text_delta（避免大正文一次性 collect 成 Vec<char>）
fn push_text_deltas(events: &mut Vec<SseEvent>, index: i32, text: &str, chunk_bytes: usize) {
    let len = text.len();
    let mut start = 0;
    while start < len {
        let mut end = (start + chunk_bytes).min(len);
        while end < len && !text.is_char_boundary(end) {
            end += 1;
        }
        let chunk = &text[start..end];
        events.push(SseEvent::new(
            "content_block_delta",
            json!({
                "type": "content_block_delta",
                "index": index,
                "delta": { "type": "text_delta", "text": chunk }
            }),
        ));
        start = end;
    }
}

/// 判断是否为 Anthropic 服务端工具定义（带版本化 type 的 web_search）
///
/// 用 name + type 双重判断：客户端完全可以自定义一个叫 web_search 的普通工具
/// （无 type、有 input_schema），不能误拦截。
pub fn is_server_tool(tool: &Tool) -> bool {
    let Some(ref tool_type) = tool.tool_type else {
        return false;
    };
    tool.name == WEB_SEARCH && tool_type.starts_with("web_search")
}

/// 检查请求是否为「web_search 与其他工具混用」场景
///
/// tools 仅含单个 web_search 的纯场景由 websearch 快路径处理，
/// 这里接收其余组合（含 web_search 且工具数 > 1）。
pub fn has_mixed_server_tools(req: &MessagesRequest) -> bool {
    req.tools
        .as_ref()
        .is_some_and(|tools| tools.len() > 1 && tools.iter().any(is_server_tool))
}

/// 为 server tool 合成 Kiro 工具定义（模型据此发起调用）
///
/// Anthropic server tool 定义没有 input_schema（由官方服务端注入），
/// 直接透传会让模型拿到一个无参数、无描述的工具。
pub(crate) fn kiro_tool_spec(tool: &Tool) -> Option<KiroTool> {
    if !is_server_tool(tool) {
        return None;
    }
    let (description, schema) = match tool.name.as_str() {
        WEB_SEARCH => (
            "Search the web for up-to-date information. Returns a list of results with title, URL and snippet. Cite sources by URL when using the results.",
            json!({
                "type": "object",
                "properties": {
                    "query": {"type": "string", "description": "The search query"}
                },
                "required": ["query"],
                "additionalProperties": false
            }),
        ),
        _ => return None,
    };
    Some(KiroTool {
        tool_specification: ToolSpecification {
            name: tool.name.clone(),
            description: description.to_string(),
            input_schema: InputSchema::from_json(schema),
        },
    })
}

/// 收集各 server tool 的剩余可用次数
fn collect_max_uses(req: &MessagesRequest) -> HashMap<String, i32> {
    let mut limits = HashMap::new();
    if let Some(tools) = &req.tools {
        for t in tools.iter().filter(|t| is_server_tool(t)) {
            limits.insert(
                t.name.clone(),
                t.max_uses.unwrap_or(DEFAULT_MAX_USES).max(0),
            );
        }
    }
    limits
}

/// 单轮模型输出中解析出的工具调用
struct ParsedToolUse {
    /// Kiro 侧的 toolUseId（回填 toolResult 时使用）
    kiro_id: String,
    /// 还原后的原始工具名
    name: String,
    input: Value,
}

/// 单轮 Kiro 调用的解析结果
struct ParsedRound {
    text: String,
    tool_uses: Vec<ParsedToolUse>,
    context_input_tokens: Option<i32>,
    context_window_exceeded: bool,
    length_exceeded: bool,
}

/// 调用一次 Kiro 并完整解析响应事件流（与非流式 handler 同口径）
async fn call_kiro_round(
    provider: &KiroProvider,
    conversation_state: &ConversationState,
    model: &str,
    tool_name_map: &HashMap<String, String>,
) -> anyhow::Result<ParsedRound> {
    let kiro_request = KiroRequest {
        conversation_state: conversation_state.clone(),
        profile_arn: None,
    };
    let request_body = serde_json::to_string(&kiro_request)?;
    let response = provider.call_api(&request_body).await?;
    let body_bytes = response.bytes().await?;

    let mut decoder = EventStreamDecoder::new();
    if let Err(e) = decoder.feed(&body_bytes) {
        tracing::warn!("缓冲区溢出: {}", e);
    }

    let mut round = ParsedRound {
        text: String::new(),
        tool_uses: Vec::new(),
        context_input_tokens: None,
        context_window_exceeded: false,
        length_exceeded: false,
    };
    let mut tool_json_buffers: HashMap<String, String> = HashMap::new();

    for result in decoder.decode_iter() {
        match result {
            Ok(frame) => {
                if let Ok(event) = Event::from_frame(frame) {
                    match event {
                        Event::AssistantResponse(resp) => round.text.push_str(&resp.content),
                        Event::ToolUse(tool_use) => {
                            let buffer = tool_json_buffers
                                .entry(tool_use.tool_use_id.clone())
                                .or_default();
                            buffer.push_str(&tool_use.input);
                            if tool_use.stop {
                                let input: Value = if buffer.is_empty() {
                                    json!({})
                                } else {
                                    serde_json::from_str(buffer).unwrap_or_else(|e| {
                                        tracing::warn!(
                                            "工具输入 JSON 解析失败: {}, tool_use_id: {}",
                                            e,
                                            tool_use.tool_use_id
                                        );
                                        json!({})
                                    })
                                };
                                let name = tool_name_map
                                    .get(&tool_use.name)
                                    .cloned()
                                    .unwrap_or_else(|| tool_use.name.clone());
                                round.tool_uses.push(ParsedToolUse {
                                    kiro_id: tool_use.tool_use_id.clone(),
                                    name,
                                    input,
                                });
                            }
                        }
                        Event::ContextUsage(cu) => {
                            let window = get_context_window_size(model);
                            round.context_input_tokens = Some(
                                (cu.context_usage_percentage * (window as f64) / 100.0) as i32,
                            );
                            if cu.context_usage_percentage >= 100.0 {
                                round.context_window_exceeded = true;
                            }
                        }
                        Event::Exception { exception_type, .. } => {
                            if exception_type == "ContentLengthExceededException" {
                                round.length_exceeded = true;
                            }
                        }
                        _ => {}
                    }
                }
            }
            Err(e) => tracing::warn!("解码事件失败: {}", e),
        }
    }

    Ok(round)
}

/// 内容块累积器 + 可选 SSE 推送
///
/// 非流式模式只累积 blocks；流式模式同时把每个块转成标准 SSE 事件序列推给客户端。
struct BlockSink {
    tx: Option<tokio::sync::mpsc::UnboundedSender<Vec<SseEvent>>>,
    blocks: Vec<Value>,
    next_index: i32,
}

impl BlockSink {
    fn new(tx: Option<tokio::sync::mpsc::UnboundedSender<Vec<SseEvent>>>) -> Self {
        Self {
            tx,
            blocks: Vec::new(),
            next_index: 0,
        }
    }

    fn send(&self, events: Vec<SseEvent>) {
        if let Some(tx) = &self.tx {
            // 客户端断开时发送失败，循环仍会跑完本轮后由上层结束
            let _ = tx.send(events);
        }
    }

    /// 推送一个完整内容块。文本类块按增量事件推送，
    /// 客户端 tool_use 块按官方约定用 input_json_delta 传输参数，
    /// server_tool_use / *_tool_result 块整块随 content_block_start 一次性发送。
    fn push_block(&mut self, block: Value) {
        let index = self.next_index;
        self.next_index += 1;

        if self.tx.is_some() {
            let mut events = Vec::new();
            let block_type = block.get("type").and_then(|v| v.as_str()).unwrap_or("");
            match block_type {
                "text" => {
                    let text = block.get("text").and_then(|v| v.as_str()).unwrap_or("");
                    events.push(SseEvent::new(
                        "content_block_start",
                        json!({
                            "type": "content_block_start",
                            "index": index,
                            "content_block": { "type": "text", "text": "" }
                        }),
                    ));
                    push_text_deltas(&mut events, index, text, TEXT_DELTA_CHUNK_BYTES);
                }
                "thinking" => {
                    let thinking = block.get("thinking").and_then(|v| v.as_str()).unwrap_or("");
                    events.push(SseEvent::new(
                        "content_block_start",
                        json!({
                            "type": "content_block_start",
                            "index": index,
                            "content_block": { "type": "thinking", "thinking": "" }
                        }),
                    ));
                    events.push(SseEvent::new(
                        "content_block_delta",
                        json!({
                            "type": "content_block_delta",
                            "index": index,
                            "delta": { "type": "thinking_delta", "thinking": thinking }
                        }),
                    ));
                }
                "tool_use" => {
                    let input_json = block
                        .get("input")
                        .map(|v| serde_json::to_string(v).unwrap_or_else(|_| "{}".to_string()))
                        .unwrap_or_else(|| "{}".to_string());
                    events.push(SseEvent::new(
                        "content_block_start",
                        json!({
                            "type": "content_block_start",
                            "index": index,
                            "content_block": {
                                "type": "tool_use",
                                "id": block.get("id"),
                                "name": block.get("name"),
                                "input": {}
                            }
                        }),
                    ));
                    events.push(SseEvent::new(
                        "content_block_delta",
                        json!({
                            "type": "content_block_delta",
                            "index": index,
                            "delta": { "type": "input_json_delta", "partial_json": input_json }
                        }),
                    ));
                }
                _ => {
                    events.push(SseEvent::new(
                        "content_block_start",
                        json!({
                            "type": "content_block_start",
                            "index": index,
                            "content_block": block
                        }),
                    ));
                }
            }
            events.push(SseEvent::new(
                "content_block_stop",
                json!({ "type": "content_block_stop", "index": index }),
            ));
            self.send(events);
        }

        self.blocks.push(block);
    }
}

/// server tool 执行产物
struct ServerToolOutcome {
    /// 对外的 server_tool_use 块 id（srvtoolu_*）
    client_tool_use_id: String,
    /// 对外结果块（web_search_tool_result）
    result_block: Value,
    /// 回填给 Kiro 模型的 toolResult
    kiro_result: ToolResult,
}

/// 执行 web_search（走 Kiro MCP），失败降级为空结果
async fn execute_web_search(provider: &KiroProvider, call: &ParsedToolUse) -> ServerToolOutcome {
    let query = call
        .input
        .get("query")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let (client_id, mcp_request) = websearch::create_mcp_request(&query);

    let search_results = if query.is_empty() {
        tracing::warn!("web_search 调用缺少 query 参数");
        None
    } else {
        match websearch::call_mcp_api(provider, &mcp_request).await {
            Ok(resp) => websearch::parse_search_results(&resp),
            Err(e) => {
                tracing::warn!("web_search MCP 调用失败: {}", e);
                None
            }
        }
    };

    // 对外块与纯 websearch 快路径同形（无 tool_use_id）
    let result_block = json!({
        "type": "web_search_tool_result",
        "content": websearch::search_results_to_result_content(&search_results)
    });

    // 回填给模型：精简 JSON（title/url/snippet），便于引用来源
    let kiro_text = match &search_results {
        Some(results) if !results.results.is_empty() => {
            let simplified: Vec<Value> = results
                .results
                .iter()
                .map(|r| {
                    json!({
                        "title": r.title,
                        "url": r.url,
                        "snippet": r.snippet.clone().unwrap_or_default()
                    })
                })
                .collect();
            serde_json::to_string(&simplified).unwrap_or_else(|_| "[]".to_string())
        }
        _ => "No results found.".to_string(),
    };

    ServerToolOutcome {
        client_tool_use_id: client_id,
        result_block,
        kiro_result: ToolResult::success(&call.kiro_id, kiro_text),
    }
}

/// 超出 max_uses 时的错误结果（对外 max_uses_exceeded，对模型提示不要再调用）
fn max_uses_exceeded_outcome(call: &ParsedToolUse) -> ServerToolOutcome {
    let client_id = format!(
        "srvtoolu_{}",
        Uuid::new_v4().to_string().replace('-', "")[..32].to_string()
    );
    let result_block = json!({
        "type": "web_search_tool_result",
        "content": {
            "type": "web_search_tool_result_error",
            "error_code": "max_uses_exceeded"
        }
    });
    let kiro_result = ToolResult::error(
        &call.kiro_id,
        format!(
            "{} usage limit exceeded; do not call this tool again. Answer with what you have.",
            call.name
        ),
    );
    ServerToolOutcome {
        client_tool_use_id: client_id,
        result_block,
        kiro_result,
    }
}

/// 把本轮 assistant 输出与 server tool 结果写回会话，构造下一轮请求状态
fn advance_conversation(
    state: &mut ConversationState,
    round_text: &str,
    server_entries: Vec<ToolUseEntry>,
    tool_results: Vec<ToolResult>,
) {
    // 1. 当前消息转入历史（历史项保留 toolResults、不携带 tools 定义，与 converter 口径一致）
    let prev_input = std::mem::take(&mut state.current_message).user_input_message;
    let model_id = prev_input.model_id.clone();
    let tools_def = prev_input.user_input_message_context.tools.clone();
    let history_user = UserMessage {
        content: prev_input.content,
        model_id: prev_input.model_id,
        origin: prev_input.origin,
        images: prev_input.images,
        user_input_message_context: UserInputMessageContext::new()
            .with_tool_results(prev_input.user_input_message_context.tool_results),
    };
    state.history.push(KiroMessage::User(HistoryUserMessage {
        user_input_message: history_user,
    }));

    // 2. assistant 回合（Kiro 要求 content 非空，只有工具调用时用占位符）
    let content = if round_text.is_empty() {
        " ".to_string()
    } else {
        round_text.to_string()
    };
    state
        .history
        .push(KiroMessage::Assistant(HistoryAssistantMessage {
            assistant_response_message: AssistantMessage::new(content)
                .with_tool_uses(server_entries),
        }));

    // 3. 新的当前消息：空文本 + toolResults + 原 tools 定义
    let context = UserInputMessageContext::new()
        .with_tools(tools_def)
        .with_tool_results(tool_results);
    let user_input = UserInputMessage::new("", model_id)
        .with_context(context)
        .with_origin("AI_EDITOR");
    state.current_message = CurrentMessage::new(user_input);
}

/// 循环最终结果
struct LoopResult {
    stop_reason: String,
    final_input_tokens: Option<i32>,
    web_search_requests: u32,
}

/// 拦截循环主体：调 Kiro → 拦截 server tool → 执行 → 回填 → 续跑
async fn run_loop(
    provider: &KiroProvider,
    mut conversation_state: ConversationState,
    tool_name_map: HashMap<String, String>,
    model: &str,
    extract_thinking: bool,
    mut remaining_uses: HashMap<String, i32>,
    sink: &mut BlockSink,
) -> anyhow::Result<LoopResult> {
    let mut result = LoopResult {
        stop_reason: "end_turn".to_string(),
        final_input_tokens: None,
        web_search_requests: 0,
    };
    let server_tool_names: HashSet<String> = remaining_uses.keys().cloned().collect();

    for round in 0..MAX_ROUNDS {
        let parsed = call_kiro_round(provider, &conversation_state, model, &tool_name_map).await?;
        if parsed.context_input_tokens.is_some() {
            result.final_input_tokens = parsed.context_input_tokens;
        }

        // 本轮文本（可选 thinking 提取）
        if extract_thinking {
            let (thinking, remaining) = extract_thinking_from_complete_text(&parsed.text);
            if let Some(t) = thinking {
                sink.push_block(json!({ "type": "thinking", "thinking": t }));
            }
            if !remaining.is_empty() {
                sink.push_block(json!({ "type": "text", "text": remaining }));
            }
        } else if !parsed.text.is_empty() {
            sink.push_block(json!({ "type": "text", "text": parsed.text }));
        }

        if parsed.context_window_exceeded {
            result.stop_reason = "model_context_window_exceeded".to_string();
        } else if parsed.length_exceeded {
            result.stop_reason = "max_tokens".to_string();
        }

        let (server_calls, client_calls): (Vec<_>, Vec<_>) = parsed
            .tool_uses
            .into_iter()
            .partition(|tu| server_tool_names.contains(&tu.name));

        // 执行 server tool 调用并输出对应块
        let mut kiro_results = Vec::new();
        let mut server_entries = Vec::new();
        for call in &server_calls {
            let uses = remaining_uses
                .get_mut(&call.name)
                .expect("server tool name 必在 remaining_uses 中");
            let outcome = if *uses <= 0 {
                max_uses_exceeded_outcome(call)
            } else {
                // server_tool_names 只含 web_search（is_server_tool 保证），此处必为 web_search
                *uses -= 1;
                result.web_search_requests += 1;
                execute_web_search(provider, call).await
            };

            sink.push_block(json!({
                "type": "server_tool_use",
                "id": outcome.client_tool_use_id,
                "name": call.name,
                "input": call.input
            }));
            sink.push_block(outcome.result_block);

            server_entries
                .push(ToolUseEntry::new(&call.kiro_id, &call.name).with_input(call.input.clone()));
            kiro_results.push(outcome.kiro_result);
        }

        // 客户端工具调用：透传给客户端并终止循环（由客户端执行后带 tool_result 回来）
        if !client_calls.is_empty() {
            if !server_calls.is_empty() {
                tracing::warn!(
                    "同一轮同时出现 server tool 与客户端工具调用，server tool 结果无法回填模型"
                );
            }
            for call in &client_calls {
                sink.push_block(json!({
                    "type": "tool_use",
                    "id": call.kiro_id,
                    "name": call.name,
                    "input": call.input
                }));
            }
            if result.stop_reason == "end_turn" {
                result.stop_reason = "tool_use".to_string();
            }
            return Ok(result);
        }

        // 没有任何工具调用：模型完成回答
        if server_calls.is_empty() {
            return Ok(result);
        }

        // 上下文满 / 输出超长时不再续跑
        if result.stop_reason != "end_turn" {
            return Ok(result);
        }

        advance_conversation(
            &mut conversation_state,
            &parsed.text,
            server_entries,
            kiro_results,
        );
        tracing::info!(round = round + 1, "server tool 已执行，回填结果续跑模型");
    }

    tracing::warn!("server tool 拦截循环达到轮数上限 {}，强制结束", MAX_ROUNDS);
    Ok(result)
}

/// 构建最终 usage 中的 server_tool_use 统计
fn server_tool_usage(result: &LoopResult) -> Option<Value> {
    let mut usage = serde_json::Map::new();
    if result.web_search_requests > 0 {
        usage.insert(
            "web_search_requests".to_string(),
            json!(result.web_search_requests),
        );
    }
    (!usage.is_empty()).then(|| Value::Object(usage))
}

/// 处理 server tool 混用请求（流式与非流式）
pub async fn handle_mixed_request(
    provider: Arc<KiroProvider>,
    payload: &MessagesRequest,
    extract_thinking_config: bool,
    input_tokens: i32,
    cache_split: Option<CacheSplit>,
) -> Response {
    let conversion = match convert_request(payload) {
        Ok(c) => c,
        Err(e) => {
            let (error_type, message) = match &e {
                ConversionError::UnsupportedModel(m) => {
                    ("invalid_request_error", format!("模型不支持: {}", m))
                }
                ConversionError::EmptyMessages => {
                    ("invalid_request_error", "消息列表为空".to_string())
                }
            };
            tracing::warn!("请求转换失败: {}", e);
            return (
                StatusCode::BAD_REQUEST,
                Json(ErrorResponse::new(error_type, message)),
            )
                .into_response();
        }
    };

    let thinking_enabled = payload
        .thinking
        .as_ref()
        .map(|t| t.is_enabled())
        .unwrap_or(false);
    let model = payload.model.clone();
    let max_uses = collect_max_uses(payload);
    let message_id = format!(
        "msg_{}",
        Uuid::new_v4().to_string().replace('-', "")[..24].to_string()
    );

    if payload.stream {
        // 流式：后台跑拦截循环，通道推送 SSE，间隔 ping 保活。
        // thinking 提取口径与流式主路径一致（thinking_enabled 即提取）。
        let (ms_input, ms_creation, ms_read) = match &cache_split {
            Some(split) => (
                split.input_tokens,
                split.cache_creation_input_tokens,
                split.cache_read_input_tokens,
            ),
            None => (input_tokens, 0, 0),
        };

        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<Vec<SseEvent>>();
        tokio::spawn(async move {
            let mut sink = BlockSink::new(Some(tx));
            sink.send(vec![SseEvent::new(
                "message_start",
                json!({
                    "type": "message_start",
                    "message": {
                        "id": message_id,
                        "type": "message",
                        "role": "assistant",
                        "model": model,
                        "content": [],
                        "stop_reason": null,
                        "usage": {
                            "input_tokens": ms_input,
                            "output_tokens": 0,
                            "cache_creation_input_tokens": ms_creation,
                            "cache_read_input_tokens": ms_read
                        }
                    }
                }),
            )]);

            match run_loop(
                &provider,
                conversion.conversation_state,
                conversion.tool_name_map,
                &model,
                thinking_enabled,
                max_uses,
                &mut sink,
            )
            .await
            {
                Ok(outcome) => {
                    let output_tokens = token::estimate_output_tokens(&sink.blocks);
                    let mut usage = json!({ "output_tokens": output_tokens });
                    if let Some(st) = server_tool_usage(&outcome) {
                        usage["server_tool_use"] = st;
                    }
                    sink.send(vec![
                        SseEvent::new(
                            "message_delta",
                            json!({
                                "type": "message_delta",
                                "delta": { "stop_reason": outcome.stop_reason },
                                "usage": usage
                            }),
                        ),
                        SseEvent::new("message_stop", json!({ "type": "message_stop" })),
                    ]);
                }
                Err(e) => {
                    tracing::error!("server tool 混用请求失败: {}", e);
                    sink.send(vec![SseEvent::new(
                        "error",
                        json!({
                            "type": "error",
                            "error": {
                                "type": "api_error",
                                "message": format!("上游 API 调用失败: {}", e)
                            }
                        }),
                    )]);
                }
            }
        });

        Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "text/event-stream")
            .header(header::CACHE_CONTROL, "no-cache")
            .header(header::CONNECTION, "keep-alive")
            .body(Body::from_stream(channel_sse_stream(rx)))
            .unwrap()
    } else {
        // 非流式：内联跑循环后一次性返回。
        // thinking 提取口径与非流式主路径一致（config 且 thinking_enabled）。
        let extract = extract_thinking_config && thinking_enabled;
        let mut sink = BlockSink::new(None);
        let outcome = run_loop(
            &provider,
            conversion.conversation_state,
            conversion.tool_name_map,
            &model,
            extract,
            max_uses,
            &mut sink,
        )
        .await;

        match outcome {
            Ok(outcome) => {
                let output_tokens = token::estimate_output_tokens(&sink.blocks);
                let final_input_tokens = outcome.final_input_tokens.unwrap_or(input_tokens);
                let mut usage = match &cache_split {
                    Some(split) => json!({
                        "input_tokens": split.input_tokens,
                        "output_tokens": output_tokens,
                        "cache_creation_input_tokens": split.cache_creation_input_tokens,
                        "cache_read_input_tokens": split.cache_read_input_tokens
                    }),
                    None => json!({
                        "input_tokens": final_input_tokens,
                        "output_tokens": output_tokens
                    }),
                };
                if let Some(st) = server_tool_usage(&outcome) {
                    usage["server_tool_use"] = st;
                }
                let body = json!({
                    "id": message_id,
                    "type": "message",
                    "role": "assistant",
                    "content": sink.blocks,
                    "model": model,
                    "stop_reason": outcome.stop_reason,
                    "stop_sequence": null,
                    "usage": usage
                });
                (StatusCode::OK, Json(body)).into_response()
            }
            Err(e) => {
                // 已有部分内容（如已完成的搜索轮次）时返回部分结果，避免整体丢弃
                if sink.blocks.is_empty() {
                    map_provider_error(e)
                } else {
                    tracing::warn!("循环中途失败，返回已完成的部分内容: {}", e);
                    let output_tokens = token::estimate_output_tokens(&sink.blocks);
                    let body = json!({
                        "id": message_id,
                        "type": "message",
                        "role": "assistant",
                        "content": sink.blocks,
                        "model": model,
                        "stop_reason": "end_turn",
                        "stop_sequence": null,
                        "usage": {
                            "input_tokens": input_tokens,
                            "output_tokens": output_tokens
                        }
                    });
                    (StatusCode::OK, Json(body)).into_response()
                }
            }
        }
    }
}

/// 把事件通道包装成带 ping 保活的 SSE 字节流
fn channel_sse_stream(
    rx: tokio::sync::mpsc::UnboundedReceiver<Vec<SseEvent>>,
) -> impl Stream<Item = Result<Bytes, Infallible>> {
    // 首个 ping 延后一个周期，保证 message_start 先行
    let first_tick = tokio::time::Instant::now() + Duration::from_secs(PING_INTERVAL_SECS);
    let ping_interval =
        tokio::time::interval_at(first_tick, Duration::from_secs(PING_INTERVAL_SECS));

    stream::unfold((rx, ping_interval), |(mut rx, mut ping)| async move {
        tokio::select! {
            batch = rx.recv() => match batch {
                Some(events) => {
                    let bytes: Vec<Result<Bytes, Infallible>> = events
                        .into_iter()
                        .map(|e| Ok(Bytes::from(e.to_sse_string())))
                        .collect();
                    Some((stream::iter(bytes), (rx, ping)))
                }
                None => None,
            },
            _ = ping.tick() => {
                tracing::trace!("发送 ping 保活事件（server tool 混用）");
                Some((stream::iter(vec![Ok(create_ping_sse())]), (rx, ping)))
            }
        }
    })
    .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_tool(name: &str, tool_type: Option<&str>) -> Tool {
        Tool {
            tool_type: tool_type.map(|s| s.to_string()),
            name: name.to_string(),
            description: String::new(),
            input_schema: Default::default(),
            max_uses: None,
            cache_control: None,
        }
    }

    fn make_request(tools: Vec<Tool>) -> MessagesRequest {
        MessagesRequest {
            model: "claude-sonnet-4-5".to_string(),
            max_tokens: 1024,
            messages: vec![super::super::types::Message {
                role: "user".to_string(),
                content: serde_json::json!("test"),
            }],
            stream: true,
            system: None,
            tools: Some(tools),
            tool_choice: None,
            thinking: None,
            output_config: None,
            cache_control: None,
            metadata: None,
        }
    }

    #[test]
    fn test_is_server_tool() {
        assert!(is_server_tool(&make_tool(
            "web_search",
            Some("web_search_20250305")
        )));
        // web_fetch 不再是 server tool（Kiro 后端无此工具）
        assert!(!is_server_tool(&make_tool(
            "web_fetch",
            Some("web_fetch_20250910")
        )));
        // 无 type 的同名普通工具不算 server tool
        assert!(!is_server_tool(&make_tool("web_search", None)));
        assert!(!is_server_tool(&make_tool("Read", None)));
        // type 与 name 不匹配
        assert!(!is_server_tool(&make_tool(
            "web_search",
            Some("web_fetch_20250910")
        )));
    }

    #[test]
    fn test_has_mixed_server_tools() {
        // 单个 web_search：走快路径，不算混用
        let req = make_request(vec![make_tool("web_search", Some("web_search_20250305"))]);
        assert!(!has_mixed_server_tools(&req));

        // web_search + 普通工具
        let req = make_request(vec![
            make_tool("web_search", Some("web_search_20250305")),
            make_tool("Read", None),
        ]);
        assert!(has_mixed_server_tools(&req));

        // web_fetch（已非 server tool）+ 普通工具：不算混用
        let req = make_request(vec![
            make_tool("web_fetch", Some("web_fetch_20250910")),
            make_tool("Read", None),
        ]);
        assert!(!has_mixed_server_tools(&req));

        // 全是普通工具
        let req = make_request(vec![make_tool("Read", None), make_tool("Write", None)]);
        assert!(!has_mixed_server_tools(&req));
    }

    #[test]
    fn test_kiro_tool_spec() {
        let spec = kiro_tool_spec(&make_tool("web_search", Some("web_search_20250305"))).unwrap();
        assert_eq!(spec.tool_specification.name, "web_search");
        assert_eq!(
            spec.tool_specification.input_schema.json["required"][0],
            "query"
        );

        // web_fetch 不再被合成为 Kiro 工具
        assert!(kiro_tool_spec(&make_tool("web_fetch", Some("web_fetch_20250910"))).is_none());
        assert!(kiro_tool_spec(&make_tool("Read", None)).is_none());
    }

    #[test]
    fn test_collect_max_uses() {
        let mut search = make_tool("web_search", Some("web_search_20250305"));
        search.max_uses = Some(3);
        // web_fetch 已非 server tool，不应进入 limits
        let fetch = make_tool("web_fetch", Some("web_fetch_20250910"));
        let req = make_request(vec![search, fetch, make_tool("Read", None)]);

        let limits = collect_max_uses(&req);
        assert_eq!(limits.get("web_search"), Some(&3));
        assert!(!limits.contains_key("web_fetch"));
        assert!(!limits.contains_key("Read"));
    }

    #[test]
    fn test_advance_conversation() {
        let mut state = ConversationState::new("conv-1").with_current_message(CurrentMessage::new(
            UserInputMessage::new("question", "claude-sonnet-4.5").with_context(
                UserInputMessageContext::new().with_tools(vec![KiroTool {
                    tool_specification: ToolSpecification {
                        name: "web_search".to_string(),
                        description: "d".to_string(),
                        input_schema: InputSchema::default(),
                    },
                }]),
            ),
        ));

        let entries = vec![
            ToolUseEntry::new("tooluse_1", "web_search").with_input(json!({"query": "q"})),
        ];
        let results = vec![ToolResult::success("tooluse_1", "results json")];
        advance_conversation(&mut state, "Let me search.", entries, results);

        // 历史：原 user 消息 + assistant（含 tool_uses）
        assert_eq!(state.history.len(), 2);
        match &state.history[0] {
            KiroMessage::User(u) => {
                assert_eq!(u.user_input_message.content, "question");
                // 历史项不携带 tools 定义
                assert!(u.user_input_message.user_input_message_context.tools.is_empty());
            }
            _ => panic!("第一条历史应为 user"),
        }
        match &state.history[1] {
            KiroMessage::Assistant(a) => {
                assert_eq!(a.assistant_response_message.content, "Let me search.");
                assert_eq!(
                    a.assistant_response_message.tool_uses.as_ref().unwrap()[0].tool_use_id,
                    "tooluse_1"
                );
            }
            _ => panic!("第二条历史应为 assistant"),
        }

        // 新当前消息：空文本 + toolResults + 原 tools 定义
        let current = &state.current_message.user_input_message;
        assert_eq!(current.content, "");
        assert_eq!(current.user_input_message_context.tool_results.len(), 1);
        assert_eq!(current.user_input_message_context.tools.len(), 1);
    }

    #[test]
    fn test_server_tool_usage() {
        let mut result = LoopResult {
            stop_reason: "end_turn".to_string(),
            final_input_tokens: None,
            web_search_requests: 0,
        };
        assert!(server_tool_usage(&result).is_none());

        result.web_search_requests = 2;
        let usage = server_tool_usage(&result).unwrap();
        assert_eq!(usage["web_search_requests"], 2);
        assert!(usage.get("web_fetch_requests").is_none());
    }

    #[test]
    fn test_block_sink_stream_events() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let mut sink = BlockSink::new(Some(tx));

        sink.push_block(json!({"type": "text", "text": "hello"}));
        sink.push_block(json!({
            "type": "tool_use",
            "id": "toolu_1",
            "name": "Read",
            "input": {"path": "/tmp/x"}
        }));

        // text 块：start + delta + stop
        let events = rx.try_recv().unwrap();
        assert_eq!(events[0].event, "content_block_start");
        assert_eq!(events[0].data["index"], 0);
        assert_eq!(events[1].data["delta"]["text"], "hello");
        assert_eq!(events.last().unwrap().event, "content_block_stop");

        // tool_use 块：start(空 input) + input_json_delta + stop，索引递增
        let events = rx.try_recv().unwrap();
        assert_eq!(events[0].data["index"], 1);
        assert_eq!(events[0].data["content_block"]["input"], json!({}));
        assert_eq!(
            events[1].data["delta"]["partial_json"],
            "{\"path\":\"/tmp/x\"}"
        );

        assert_eq!(sink.blocks.len(), 2);
    }
}
