//! 模拟 prompt 缓存
//!
//! ⚠️ 重要：Kiro 后端**不支持** prompt 缓存，也不返回任何缓存 token 信息。
//! 本模块在代理出口处**伪造** `cache_creation_input_tokens` /
//! `cache_read_input_tokens` 字段，仅用于让 sub2api 等统计面板的缓存指标
//! 不为 0、成本曲线接近真实 Anthropic。它**不会**真的节省 token、额度或耗时——
//! 真实开销在 Kiro 侧一分没少。
//!
//! 仅在配置 `simulate-cache: true` 时启用，默认关闭（关闭时行为与原先完全一致）。
//!
//! 工作原理（贴近真实 Anthropic 语义）：
//! 1. 只认客户端真正打的 `cache_control: {type:"ephemeral"}` 断点。
//! 2. 按规范顺序（tools → system → messages）把输入切成有序段，对每个断点
//!    位置计算"从头到该断点"的累积前缀哈希。
//! 3. 维护一个 TTL=5 分钟的前缀表（模拟 ephemeral 生命周期）：命中且未过期的
//!    最长前缀计入 `cache_read`，其余新前缀计入 `cache_creation`，断点之后的
//!    尾部新内容计入 `input_tokens`。
//! 4. 恒等式：`input + cache_creation + cache_read == 总输入 token`。

use std::collections::HashMap;
use std::sync::LazyLock;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use sha2::{Digest, Sha256};

use super::types::{Message, SystemMessage, Tool};

/// 模拟缓存的 TTL，与 Anthropic ephemeral 缓存一致（5 分钟）。
const CACHE_TTL: Duration = Duration::from_secs(5 * 60);

/// 缓存断点的最小 token 数门槛，与 Anthropic 一致（小于此值不计入缓存）。
const MIN_CACHEABLE_TOKENS: i32 = 1024;

/// 全局前缀缓存表：累积前缀哈希 -> 写入时刻。
///
/// 只记录"这个前缀在最近 5 分钟内出现过"，token 数由当次请求重新计算，
/// 因此表本身只需存时间戳。
static PREFIX_CACHE: LazyLock<Mutex<HashMap<u64, Instant>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// 缓存拆分结果：三者之和等于总输入 token。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CacheSplit {
    /// 未缓存的输入 token（断点之后的新内容）。
    pub input_tokens: i32,
    /// 本次新写入缓存的 token。
    pub cache_creation_input_tokens: i32,
    /// 本次命中缓存的 token。
    pub cache_read_input_tokens: i32,
}

impl CacheSplit {
    /// 退化值：全部计入 input，缓存字段为 0（等价于不启用模拟缓存）。
    fn passthrough(total: i32) -> Self {
        Self {
            input_tokens: total,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
        }
    }
}

/// 一个缓存断点：到该断点为止的累积前缀哈希 + 累积 token 数。
struct Breakpoint {
    cumulative_hash: u64,
    cumulative_tokens: i32,
}

/// 把 64-bit 哈希从 sha256 前 8 字节折叠出来。
fn finish_hash(hasher: Sha256) -> u64 {
    let digest = hasher.finalize();
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&digest[..8]);
    u64::from_le_bytes(bytes)
}

/// 判断一个 JSON 值是否带有 `cache_control` 断点标记。
fn has_cache_control(value: &serde_json::Value) -> bool {
    value
        .get("cache_control")
        .map(|v| !v.is_null())
        .unwrap_or(false)
}

/// 按规范顺序（tools → system → messages）遍历输入，在每个客户端断点处
/// 记录累积前缀哈希与累积 token 数。
///
/// 返回 (断点列表, 本地累积总 token)。token 计数复用 `crate::token::count_tokens`，
/// 与现有总数估算同源，但这里是按段累加（同一套 tokenizer，不引入新的遍历语义）。
fn collect_breakpoints(
    scope_key: &str,
    system: Option<&[SystemMessage]>,
    messages: &[Message],
    tools: Option<&[Tool]>,
) -> (Vec<Breakpoint>, i32) {
    use crate::token::count_tokens;

    let mut hasher = Sha256::new();
    hasher.update(scope_key.as_bytes());
    hasher.update(b"\x00");

    let mut cumulative_tokens = 0i32;
    let mut breakpoints = Vec::new();

    // 在当前累积状态上记录一个断点（克隆 hasher 以保留累积前缀）。
    let push_bp =
        |hasher: &Sha256, cumulative_tokens: i32, breakpoints: &mut Vec<Breakpoint>| {
            breakpoints.push(Breakpoint {
                cumulative_hash: finish_hash(hasher.clone()),
                cumulative_tokens,
            });
        };

    // 1) tools
    if let Some(tools) = tools {
        for tool in tools {
            hasher.update(tool.name.as_bytes());
            hasher.update(tool.description.as_bytes());
            let schema = serde_json::to_string(&tool.input_schema).unwrap_or_default();
            hasher.update(schema.as_bytes());
            cumulative_tokens += count_tokens(&tool.name) as i32;
            cumulative_tokens += count_tokens(&tool.description) as i32;
            cumulative_tokens += count_tokens(&schema) as i32;
            if tool
                .cache_control
                .as_ref()
                .map(|v| !v.is_null())
                .unwrap_or(false)
            {
                push_bp(&hasher, cumulative_tokens, &mut breakpoints);
            }
        }
    }

    // 2) system
    if let Some(system) = system {
        for msg in system {
            hasher.update(msg.text.as_bytes());
            cumulative_tokens += count_tokens(&msg.text) as i32;
            if msg
                .cache_control
                .as_ref()
                .map(|v| !v.is_null())
                .unwrap_or(false)
            {
                push_bp(&hasher, cumulative_tokens, &mut breakpoints);
            }
        }
    }

    // 3) messages（content 已是 serde_json::Value，直接 walk，无额外解析）
    for msg in messages {
        match &msg.content {
            serde_json::Value::String(s) => {
                hasher.update(s.as_bytes());
                cumulative_tokens += count_tokens(s) as i32;
            }
            serde_json::Value::Array(arr) => {
                for block in arr {
                    if let Some(text) = block.get("text").and_then(|v| v.as_str()) {
                        hasher.update(text.as_bytes());
                        cumulative_tokens += count_tokens(text) as i32;
                    }
                    if has_cache_control(block) {
                        push_bp(&hasher, cumulative_tokens, &mut breakpoints);
                    }
                }
            }
            _ => {}
        }
    }

    (breakpoints, cumulative_tokens)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sys(text: &str, mark: bool) -> SystemMessage {
        SystemMessage {
            text: text.to_string(),
            cache_control: mark.then(|| json!({"type": "ephemeral"})),
        }
    }

    fn user_msg(text: &str) -> Message {
        Message {
            role: "user".to_string(),
            content: json!(text),
        }
    }

    #[test]
    fn no_breakpoint_is_passthrough() {
        let split = compute_split("scope-a", None, &[user_msg("hi")], None, 5000);
        assert_eq!(split, CacheSplit::passthrough(5000));
    }

    #[test]
    fn identity_always_holds() {
        let big = "x ".repeat(5000); // 远超 MIN_CACHEABLE_TOKENS
        let system = vec![sys(&big, true)];
        let split =
            compute_split("scope-id", Some(&system), &[user_msg("question")], None, 9000);
        assert_eq!(
            split.input_tokens
                + split.cache_creation_input_tokens
                + split.cache_read_input_tokens,
            9000
        );
    }

    #[test]
    fn first_call_creates_second_call_reads() {
        let big = "token ".repeat(5000);
        let system = vec![sys(&big, true)];

        // 第一次：应当是 creation（read 为 0）
        let first = compute_split(
            "scope-flow",
            Some(&system),
            &[user_msg("q1")],
            None,
            8000,
        );
        assert_eq!(first.cache_read_input_tokens, 0);
        assert!(first.cache_creation_input_tokens > 0);

        // 第二次：相同前缀应命中 read
        let second = compute_split(
            "scope-flow",
            Some(&system),
            &[user_msg("q2 different tail")],
            None,
            8000,
        );
        assert!(second.cache_read_input_tokens > 0);
    }

    #[test]
    fn scope_isolation() {
        let big = "alpha ".repeat(5000);
        let system = vec![sys(&big, true)];
        let _ = compute_split("scope-A", Some(&system), &[user_msg("q")], None, 8000);
        // 不同 scope 不应命中
        let other = compute_split("scope-B", Some(&system), &[user_msg("q")], None, 8000);
        assert_eq!(other.cache_read_input_tokens, 0);
    }
}

/// 计算请求的缓存拆分。
///
/// - `scope_key`：隔离标识（如 `model + user_id`），防止跨凭据/会话串味。
/// - `system` / `messages` / `tools`：请求内容（含 `cache_control` 断点信息）。
/// - `total_input_tokens`：已算好的总输入 token（复用现有计数，不重新 tokenize 文本）。
///
/// 返回的三字段之和恒等于 `total_input_tokens`。
pub fn compute_split(
    scope_key: &str,
    system: Option<&[SystemMessage]>,
    messages: &[Message],
    tools: Option<&[Tool]>,
    total_input_tokens: i32,
) -> CacheSplit {
    let total = total_input_tokens.max(0);

    // 按规范顺序收集断点（带 cache_control 的位置）的累积前缀。
    // 累积 token 用本地估算单位，最后按真实总数 `total` 缩放，保证恒等式成立。
    let (breakpoints, local_total) =
        collect_breakpoints(scope_key, system, messages, tools);

    // 没有客户端断点 —— 与真实 Anthropic 行为一致：不显示缓存。
    if breakpoints.is_empty() || local_total == 0 {
        return CacheSplit::passthrough(total);
    }

    // 把本地累积 token 缩放到真实总数单位。
    let scale = |local: i32| -> i32 {
        ((local as i64 * total as i64) / local_total as i64) as i32
    };

    let now = Instant::now();
    let mut cache = PREFIX_CACHE.lock();

    // 惰性清理过期项，避免无限增长。
    cache.retain(|_, written| now.duration_since(*written) < CACHE_TTL);

    // 找命中的最长前缀（断点按累积顺序排列，取最后一个命中的）。
    let mut hit_local = 0i32;
    for bp in &breakpoints {
        if bp.cumulative_tokens < MIN_CACHEABLE_TOKENS {
            continue;
        }
        let fresh = cache
            .get(&bp.cumulative_hash)
            .map(|written| now.duration_since(*written) < CACHE_TTL)
            .unwrap_or(false);
        if fresh {
            hit_local = bp.cumulative_tokens;
        }
    }

    // 把所有达标断点前缀写入/刷新缓存表，供后续请求命中。
    for bp in &breakpoints {
        if bp.cumulative_tokens >= MIN_CACHEABLE_TOKENS {
            cache.insert(bp.cumulative_hash, now);
        }
    }
    drop(cache);

    // 最大可缓存边界 = 最后一个达标断点的累积 token（本地单位）。
    let cacheable_local = breakpoints
        .iter()
        .rev()
        .find(|bp| bp.cumulative_tokens >= MIN_CACHEABLE_TOKENS)
        .map(|bp| bp.cumulative_tokens)
        .unwrap_or(0);

    // 命中的部分计入 read，未命中但可缓存的部分计入 creation，剩余尾部计入 input。
    let read = scale(hit_local).min(total);
    let creation = (scale(cacheable_local) - read).max(0).min(total - read);
    let input = total - read - creation;

    CacheSplit {
        input_tokens: input,
        cache_creation_input_tokens: creation,
        cache_read_input_tokens: read,
    }
}
