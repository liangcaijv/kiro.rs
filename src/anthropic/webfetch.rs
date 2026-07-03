//! WebFetch 工具处理模块
//!
//! 实现 Anthropic WebFetch 请求到 Kiro MCP 的转换和响应生成。
//! 与 [`super::websearch`] 对称：检测「仅含单个 web_fetch 工具」的请求，
//! 从消息中提取 URL，调用 Kiro 内置的 MCP `web_fetch` 工具（与 web_search
//! 走同一个 `/mcp` 端点），再把抓取到的正文伪造成 Anthropic 的
//! `web_fetch_tool_result` SSE 事件返回。

use std::convert::Infallible;

use axum::{
    body::Body,
    http::{StatusCode, header},
    response::{IntoResponse, Json, Response},
};
use bytes::Bytes;
use futures::{Stream, stream};
use serde::Serialize;
use serde_json::json;
use uuid::Uuid;

use super::stream::SseEvent;
use super::types::{ErrorResponse, MessagesRequest};
// 复用 websearch 中定义的 MCP 响应类型（两者共用同一套 JSON-RPC 结构）
use super::websearch::McpResponse;

/// MCP 请求（web_fetch）
#[derive(Debug, Serialize)]
struct FetchMcpRequest {
    id: String,
    jsonrpc: String,
    method: String,
    params: FetchMcpParams,
}

/// MCP 请求参数
#[derive(Debug, Serialize)]
struct FetchMcpParams {
    name: String,
    arguments: FetchMcpArguments,
}

/// MCP 参数（web_fetch 的入参：url + mode）
#[derive(Debug, Serialize)]
struct FetchMcpArguments {
    url: String,
    /// 抓取模式：full=完整内容 / truncated=前若干字符 / selective=按关键词
    mode: String,
}

/// 检查请求是否为纯 WebFetch 请求
///
/// 条件：tools 有且只有一个，且 name 为 web_fetch
pub fn has_web_fetch_tool(req: &MessagesRequest) -> bool {
    req.tools.as_ref().is_some_and(|tools| {
        tools.len() == 1 && tools.first().is_some_and(|t| t.name == "web_fetch")
    })
}

/// 从消息中提取要抓取的 URL
///
/// 读取第一条消息的文本内容，扫描其中第一个 http(s):// URL。
/// 兼容纯字符串 content 与 内容块数组（拼接所有 text 块后再扫描）。
pub fn extract_fetch_url(req: &MessagesRequest) -> Option<String> {
    let first_msg = req.messages.first()?;

    let text = match &first_msg.content {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Array(arr) => {
            let mut combined = String::new();
            for block in arr {
                if block.get("type").and_then(|v| v.as_str()) == Some("text") {
                    if let Some(t) = block.get("text").and_then(|v| v.as_str()) {
                        combined.push_str(t);
                        combined.push(' ');
                    }
                }
            }
            combined
        }
        _ => return None,
    };

    find_first_url(&text)
}

/// 从文本中提取第一个 http(s):// URL
fn find_first_url(text: &str) -> Option<String> {
    let https_pos = text.find("https://");
    let http_pos = text.find("http://");
    let start = match (https_pos, http_pos) {
        (Some(a), Some(b)) => a.min(b),
        (Some(a), None) => a,
        (None, Some(b)) => b,
        (None, None) => return None,
    };

    let rest = &text[start..];
    // URL 在空白或常见分隔/包裹字符处结束
    let end = rest
        .find(|c: char| {
            c.is_whitespace() || matches!(c, '"' | '\'' | '<' | '>' | '`' | ')' | ']' | '}')
        })
        .unwrap_or(rest.len());
    // 去除结尾的标点（句号/逗号等常见误纳）
    let url = rest[..end].trim_end_matches(|c: char| matches!(c, '.' | ',' | ';' | ':' | '!' | '?'));

    (!url.is_empty()).then(|| url.to_string())
}

/// 创建 MCP 请求
///
/// ID 格式: web_fetch_tooluse_{22位随机}_{毫秒时间戳}_{8位随机}
fn create_fetch_mcp_request(url: &str) -> (String, FetchMcpRequest) {
    let random_22 = super::websearch::generate_random_id_22();
    let timestamp = chrono::Utc::now().timestamp_millis();
    let random_8 = super::websearch::generate_random_id_8();

    let request_id = format!("web_fetch_tooluse_{}_{}_{}", random_22, timestamp, random_8);

    // tool_use_id 与 web_search 同格式：srvtoolu_{32位hex}
    let tool_use_id = format!(
        "srvtoolu_{}",
        Uuid::new_v4().to_string().replace('-', "")[..32].to_string()
    );

    let request = FetchMcpRequest {
        id: request_id,
        jsonrpc: "2.0".to_string(),
        method: "tools/call".to_string(),
        params: FetchMcpParams {
            name: "web_fetch".to_string(),
            arguments: FetchMcpArguments {
                url: url.to_string(),
                // 取完整正文；Kiro 侧上限 10MB
                mode: "full".to_string(),
            },
        },
    };

    (tool_use_id, request)
}

/// 从 MCP 响应中提取抓取到的正文
///
/// Kiro 的返回落在 `result.content[].text`。该字段可能是纯文本，
/// 也可能是 JSON 包装（常见字段 content/text/data/markdown/body）。
/// 两种情况都尽量取出可读正文，取不到则回退为原始拼接文本。
fn parse_fetch_content(mcp_response: &McpResponse) -> Option<String> {
    let result = mcp_response.result.as_ref()?;

    let mut combined = String::new();
    for c in &result.content {
        if c.content_type == "text" {
            combined.push_str(&c.text);
        }
    }
    if combined.is_empty() {
        return None;
    }

    // 尝试 JSON 解包
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&combined) {
        if let Some(s) = v.as_str() {
            return (!s.is_empty()).then(|| s.to_string());
        }
        if let Some(obj) = v.as_object() {
            for key in ["content", "text", "data", "markdown", "body"] {
                if let Some(s) = obj.get(key).and_then(|x| x.as_str()) {
                    if !s.is_empty() {
                        return Some(s.to_string());
                    }
                }
            }
        }
    }

    Some(combined)
}

/// 生成 WebFetch SSE 响应流
fn create_webfetch_sse_stream(
    model: String,
    url: String,
    tool_use_id: String,
    fetched: Option<String>,
    input_tokens: i32,
    cache_split: Option<super::cache_sim::CacheSplit>,
) -> impl Stream<Item = Result<Bytes, Infallible>> {
    let events =
        generate_webfetch_events(&model, &url, &tool_use_id, fetched, input_tokens, cache_split);

    stream::iter(
        events
            .into_iter()
            .map(|e| Ok(Bytes::from(e.to_sse_string()))),
    )
}

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
                "delta": {
                    "type": "text_delta",
                    "text": chunk
                }
            }),
        ));
        start = end;
    }
}

/// 生成 WebFetch SSE 事件序列
fn generate_webfetch_events(
    model: &str,
    url: &str,
    tool_use_id: &str,
    fetched: Option<String>,
    input_tokens: i32,
    cache_split: Option<super::cache_sim::CacheSplit>,
) -> Vec<SseEvent> {
    let mut events = Vec::new();
    let message_id = format!(
        "msg_{}",
        Uuid::new_v4().to_string().replace('-', "")[..24].to_string()
    );

    // 1. message_start
    let (wf_input, wf_creation, wf_read) = match &cache_split {
        Some(split) => (
            split.input_tokens,
            split.cache_creation_input_tokens,
            split.cache_read_input_tokens,
        ),
        None => (input_tokens, 0, 0),
    };
    events.push(SseEvent::new(
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
                    "input_tokens": wf_input,
                    "output_tokens": 0,
                    "cache_creation_input_tokens": wf_creation,
                    "cache_read_input_tokens": wf_read
                }
            }
        }),
    ));

    // 2. content_block_start (text - 抓取决策说明, index 0)
    let decision_text = format!("I'll fetch content from {}.", url);
    events.push(SseEvent::new(
        "content_block_start",
        json!({
            "type": "content_block_start",
            "index": 0,
            "content_block": { "type": "text", "text": "" }
        }),
    ));
    events.push(SseEvent::new(
        "content_block_delta",
        json!({
            "type": "content_block_delta",
            "index": 0,
            "delta": { "type": "text_delta", "text": decision_text }
        }),
    ));
    events.push(SseEvent::new(
        "content_block_stop",
        json!({ "type": "content_block_stop", "index": 0 }),
    ));

    // 3. content_block_start (server_tool_use, index 1)
    // server_tool_use 是服务端工具，input 在 content_block_start 中一次性完整发送。
    // 对外的 input 只暴露 url（与 Anthropic web_fetch 一致，不含 Kiro 内部的 mode）。
    events.push(SseEvent::new(
        "content_block_start",
        json!({
            "type": "content_block_start",
            "index": 1,
            "content_block": {
                "id": tool_use_id,
                "type": "server_tool_use",
                "name": "web_fetch",
                "input": { "url": url }
            }
        }),
    ));
    events.push(SseEvent::new(
        "content_block_stop",
        json!({ "type": "content_block_stop", "index": 1 }),
    ));

    // 4. content_block_start (web_fetch_tool_result, index 2)
    let result_block = match &fetched {
        Some(content) => {
            let retrieved_at =
                chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
            json!({
                "type": "web_fetch_tool_result",
                "tool_use_id": tool_use_id,
                "content": {
                    "type": "web_fetch_result",
                    "url": url,
                    "retrieved_at": retrieved_at,
                    "content": {
                        "type": "document",
                        "source": {
                            "type": "text",
                            "media_type": "text/plain",
                            "data": content
                        },
                        "title": url,
                        "citations": { "enabled": false }
                    }
                }
            })
        }
        None => json!({
            "type": "web_fetch_tool_result",
            "tool_use_id": tool_use_id,
            "content": {
                "type": "web_fetch_tool_result_error",
                "error_code": "url_not_accessible"
            }
        }),
    };
    events.push(SseEvent::new(
        "content_block_start",
        json!({
            "type": "content_block_start",
            "index": 2,
            "content_block": result_block
        }),
    ));
    events.push(SseEvent::new(
        "content_block_stop",
        json!({ "type": "content_block_stop", "index": 2 }),
    ));

    // 5. content_block_start (text, index 3) - 把正文作为可读文本再给一份，
    // 方便只读取 assistant 文本（而不解析结构化块）的客户端。
    events.push(SseEvent::new(
        "content_block_start",
        json!({
            "type": "content_block_start",
            "index": 3,
            "content_block": { "type": "text", "text": "" }
        }),
    ));

    let body_text = match &fetched {
        Some(c) => c.clone(),
        None => format!("Failed to fetch content from {}.", url),
    };
    push_text_deltas(&mut events, 3, &body_text, 2000);

    events.push(SseEvent::new(
        "content_block_stop",
        json!({ "type": "content_block_stop", "index": 3 }),
    ));

    // 6. message_delta
    let output_tokens = (body_text.len() as i32 + 3) / 4; // 简单估算
    events.push(SseEvent::new(
        "message_delta",
        json!({
            "type": "message_delta",
            "delta": { "stop_reason": "end_turn" },
            "usage": {
                "output_tokens": output_tokens,
                "server_tool_use": { "web_fetch_requests": 1 }
            }
        }),
    ));

    // 7. message_stop
    events.push(SseEvent::new(
        "message_stop",
        json!({ "type": "message_stop" }),
    ));

    events
}

/// 处理 WebFetch 请求
pub async fn handle_webfetch_request(
    provider: std::sync::Arc<crate::kiro::provider::KiroProvider>,
    payload: &MessagesRequest,
    input_tokens: i32,
    cache_split: Option<super::cache_sim::CacheSplit>,
) -> Response {
    // 1. 提取 URL
    let url = match extract_fetch_url(payload) {
        Some(u) => u,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(ErrorResponse::new(
                    "invalid_request_error",
                    "无法从消息中提取要抓取的 URL",
                )),
            )
                .into_response();
        }
    };

    tracing::info!(url = %url, "处理 WebFetch 请求");

    // 2. 创建 MCP 请求
    let (tool_use_id, mcp_request) = create_fetch_mcp_request(&url);

    // 3. 调用 Kiro MCP API
    let fetched = match call_fetch_mcp_api(&provider, &mcp_request).await {
        Ok(response) => parse_fetch_content(&response),
        Err(e) => {
            tracing::warn!("MCP API 调用失败: {}", e);
            None
        }
    };

    // 4. 生成 SSE 响应
    let model = payload.model.clone();
    let stream =
        create_webfetch_sse_stream(model, url, tool_use_id, fetched, input_tokens, cache_split);

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .header(header::CACHE_CONTROL, "no-cache")
        .header(header::CONNECTION, "keep-alive")
        .body(Body::from_stream(stream))
        .unwrap()
}

/// 调用 Kiro MCP API
async fn call_fetch_mcp_api(
    provider: &crate::kiro::provider::KiroProvider,
    request: &FetchMcpRequest,
) -> anyhow::Result<McpResponse> {
    let request_body = serde_json::to_string(request)?;

    tracing::debug!("MCP fetch request: {}", request_body);

    let response = provider.call_mcp(&request_body).await?;

    let body = response.text().await?;
    tracing::debug!("MCP fetch response: {}", body);

    let mcp_response: McpResponse = serde_json::from_str(&body)?;

    if let Some(ref error) = mcp_response.error {
        anyhow::bail!(
            "MCP error: {} - {}",
            error.code.unwrap_or(-1),
            error.message.as_deref().unwrap_or("Unknown error")
        );
    }

    Ok(mcp_response)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::anthropic::websearch::{McpContent, McpResult};

    fn req_with_tools(
        content: serde_json::Value,
        tools: Option<Vec<crate::anthropic::types::Tool>>,
    ) -> MessagesRequest {
        use crate::anthropic::types::Message;
        MessagesRequest {
            model: "claude-sonnet-4".to_string(),
            max_tokens: 1024,
            messages: vec![Message {
                role: "user".to_string(),
                content,
            }],
            stream: true,
            system: None,
            tools,
            tool_choice: None,
            thinking: None,
            output_config: None,
            cache_control: None,
            metadata: None,
        }
    }

    fn web_fetch_tool() -> crate::anthropic::types::Tool {
        crate::anthropic::types::Tool {
            tool_type: Some("web_fetch_20250910".to_string()),
            name: "web_fetch".to_string(),
            description: String::new(),
            input_schema: Default::default(),
            max_uses: Some(5),
            cache_control: None,
        }
    }

    #[test]
    fn test_has_web_fetch_tool_only_one() {
        let req = req_with_tools(json!("test"), Some(vec![web_fetch_tool()]));
        assert!(has_web_fetch_tool(&req));
    }

    #[test]
    fn test_has_web_fetch_tool_multiple_tools() {
        let other = crate::anthropic::types::Tool {
            tool_type: None,
            name: "other_tool".to_string(),
            description: "Other tool".to_string(),
            input_schema: Default::default(),
            max_uses: None,
            cache_control: None,
        };
        let req = req_with_tools(json!("test"), Some(vec![web_fetch_tool(), other]));
        assert!(!has_web_fetch_tool(&req));
    }

    #[test]
    fn test_extract_fetch_url_plain_string() {
        let req = req_with_tools(
            json!("Please analyze the content at https://example.com/article now"),
            None,
        );
        assert_eq!(
            extract_fetch_url(&req),
            Some("https://example.com/article".to_string())
        );
    }

    #[test]
    fn test_extract_fetch_url_trailing_punctuation() {
        let req = req_with_tools(json!("Fetch https://example.com/page."), None);
        assert_eq!(
            extract_fetch_url(&req),
            Some("https://example.com/page".to_string())
        );
    }

    #[test]
    fn test_extract_fetch_url_from_blocks() {
        let req = req_with_tools(
            json!([{ "type": "text", "text": "read http://foo.bar/baz?q=1 please" }]),
            None,
        );
        assert_eq!(
            extract_fetch_url(&req),
            Some("http://foo.bar/baz?q=1".to_string())
        );
    }

    #[test]
    fn test_extract_fetch_url_none() {
        let req = req_with_tools(json!("no link here"), None);
        assert_eq!(extract_fetch_url(&req), None);
    }

    #[test]
    fn test_create_fetch_mcp_request() {
        let (tool_use_id, request) = create_fetch_mcp_request("https://example.com");
        assert!(tool_use_id.starts_with("srvtoolu_"));
        assert_eq!(request.jsonrpc, "2.0");
        assert_eq!(request.method, "tools/call");
        assert_eq!(request.params.name, "web_fetch");
        assert_eq!(request.params.arguments.url, "https://example.com");
        assert_eq!(request.params.arguments.mode, "full");
        assert!(request.id.starts_with("web_fetch_tooluse_"));
    }

    fn mcp_response_with_text(text: &str) -> McpResponse {
        McpResponse {
            error: None,
            id: "test".to_string(),
            jsonrpc: "2.0".to_string(),
            result: Some(McpResult {
                content: vec![McpContent {
                    content_type: "text".to_string(),
                    text: text.to_string(),
                }],
                is_error: false,
            }),
        }
    }

    #[test]
    fn test_parse_fetch_content_plain_text() {
        let resp = mcp_response_with_text("Hello page content");
        assert_eq!(
            parse_fetch_content(&resp),
            Some("Hello page content".to_string())
        );
    }

    #[test]
    fn test_parse_fetch_content_json_wrapped() {
        let resp = mcp_response_with_text(r#"{"content":"wrapped body","url":"https://x"}"#);
        assert_eq!(
            parse_fetch_content(&resp),
            Some("wrapped body".to_string())
        );
    }

    #[test]
    fn test_parse_fetch_content_empty() {
        let resp = mcp_response_with_text("");
        assert_eq!(parse_fetch_content(&resp), None);
    }

    #[test]
    fn test_generate_webfetch_events_success() {
        let events = generate_webfetch_events(
            "claude-sonnet-4-6",
            "https://example.com/page",
            "srvtoolu_abc",
            Some("PAGE BODY CONTENT".to_string()),
            100,
            None,
        );

        // server_tool_use 块：name=web_fetch，input.url 正确，且不泄露内部 mode
        let stu = events
            .iter()
            .find(|e| {
                e.event == "content_block_start"
                    && e.data["content_block"]["type"] == "server_tool_use"
            })
            .expect("server_tool_use block");
        assert_eq!(stu.data["content_block"]["name"], "web_fetch");
        assert_eq!(
            stu.data["content_block"]["input"]["url"],
            "https://example.com/page"
        );
        assert!(stu.data["content_block"]["input"].get("mode").is_none());

        // web_fetch_tool_result 块：document 携带抓取正文
        let res = events
            .iter()
            .find(|e| {
                e.event == "content_block_start"
                    && e.data["content_block"]["type"] == "web_fetch_tool_result"
            })
            .expect("web_fetch_tool_result block");
        assert_eq!(res.data["content_block"]["tool_use_id"], "srvtoolu_abc");
        assert_eq!(
            res.data["content_block"]["content"]["type"],
            "web_fetch_result"
        );
        assert_eq!(
            res.data["content_block"]["content"]["content"]["source"]["data"],
            "PAGE BODY CONTENT"
        );

        // 正文也作为 index 3 的 text_delta 输出
        let has_body = events.iter().any(|e| {
            e.event == "content_block_delta"
                && e.data["index"].as_i64() == Some(3)
                && e.data["delta"]["text"] == "PAGE BODY CONTENT"
        });
        assert!(has_body);

        // usage 计数与 stop_reason
        let md = events
            .iter()
            .find(|e| e.event == "message_delta")
            .expect("message_delta");
        assert_eq!(
            md.data["usage"]["server_tool_use"]["web_fetch_requests"],
            1
        );
        assert_eq!(md.data["delta"]["stop_reason"], "end_turn");
    }

    #[test]
    fn test_generate_webfetch_events_error() {
        let events = generate_webfetch_events(
            "claude-sonnet-4-6",
            "https://example.com/404",
            "srvtoolu_err",
            None,
            50,
            None,
        );

        let res = events
            .iter()
            .find(|e| {
                e.event == "content_block_start"
                    && e.data["content_block"]["type"] == "web_fetch_tool_result"
            })
            .expect("web_fetch_tool_result block");
        assert_eq!(
            res.data["content_block"]["content"]["type"],
            "web_fetch_tool_result_error"
        );
        assert_eq!(
            res.data["content_block"]["content"]["error_code"],
            "url_not_accessible"
        );
    }
}
