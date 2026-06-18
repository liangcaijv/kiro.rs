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
//! 1. 只认客户端真正打的 `cache_control: {type:"ephemeral"}`。支持块级显式
//!    断点，也支持顶层 automatic caching 标记。
//! 2. 按规范顺序（tools → system → messages）把输入切成有序段，对每个断点
//!    位置计算"从头到该断点"的累积前缀哈希。
//! 3. 按模型使用对应的最小可缓存 token 门槛，避免低于门槛时误报缓存。
//! 4. 维护一个 TTL=5 分钟的前缀表（模拟 ephemeral 生命周期）：命中且未过期的
//!    最长前缀计入 `cache_read`，其余新前缀计入 `cache_creation`，断点之后的
//!    尾部新内容计入 `input_tokens`。
//! 5. 恒等式：`input + cache_creation + cache_read == 总输入 token`。

use std::sync::LazyLock;
use std::time::Duration;

use moka::sync::Cache;
use sha2::{Digest, Sha256};

use super::types::{Message, SystemMessage, Tool};

/// 模拟缓存的 TTL，与 Anthropic ephemeral 缓存一致（5 分钟）。
const CACHE_TTL: Duration = Duration::from_secs(5 * 60);

/// 模拟缓存最多记录的前缀数量，避免无界增长。
const MAX_CACHE_ENTRIES: u64 = 100_000;

/// 缓存断点的最小 token 数门槛，与 Anthropic 一致（小于此值不计入缓存）。
const DEFAULT_MIN_CACHEABLE_TOKENS: i32 = 1024;

struct PrefixCache {
    cache: Cache<u64, ()>,
}

impl PrefixCache {
    fn new(ttl: Duration, max_capacity: u64) -> Self {
        Self {
            // 用 time_to_idle（空闲超时）而非 time_to_live：每次命中（get）或写入
            // 都会刷新计时，贴近 Anthropic ephemeral 的滑动 TTL 语义——持续被使用的
            // 前缀保持存活，5 分钟无人问津才过期。
            cache: Cache::builder()
                .time_to_idle(ttl)
                .max_capacity(max_capacity)
                .build(),
        }
    }

    fn contains(&self, hash: u64) -> bool {
        self.cache.get(&hash).is_some()
    }

    fn insert(&self, hash: u64) {
        self.cache.insert(hash, ());
    }

    #[cfg(test)]
    fn entry_count(&self) -> u64 {
        self.cache.entry_count()
    }

    #[cfg(test)]
    fn run_pending_tasks(&self) {
        self.cache.run_pending_tasks();
    }
}

/// 全局前缀缓存表：累积前缀哈希 -> 最近 5 分钟（空闲窗口）内是否出现过。
static PREFIX_CACHE: LazyLock<PrefixCache> =
    LazyLock::new(|| PrefixCache::new(CACHE_TTL, MAX_CACHE_ENTRIES));

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

    /// 按折扣系数衰减伪造出的 `cache_read`，差额回退到 `cache_creation`。
    ///
    /// 缓存读取按约 1/10 价计费，会把大段稳定前缀的扣费压得极低；本方法只保留
    /// `cache_read × factor`，把被砍掉的部分计入 `cache_creation`（约 1.25 倍计费），
    /// 用于抬高面板计费。`factor` 夹取到 `[0.0, 1.0]`，恒等式
    /// `input + cache_creation + cache_read == 总输入` 保持不变。`factor == 1.0`
    /// 时结果与衰减前完全一致（向后兼容）。
    pub fn dampen_read(self, factor: f64) -> Self {
        let f = factor.clamp(0.0, 1.0);
        let kept = (self.cache_read_input_tokens as f64 * f).round() as i32;
        let moved = self.cache_read_input_tokens - kept;
        Self {
            input_tokens: self.input_tokens,
            cache_creation_input_tokens: self.cache_creation_input_tokens + moved,
            cache_read_input_tokens: kept,
        }
    }
}

/// 一个前缀点：到此块边界为止的累积前缀哈希 + 累积 token 数。
///
/// `is_breakpoint` 标记该位置是否带客户端 `cache_control` 断点：
/// - 写入缓存表只在断点处发生（稀疏，控制基数）；
/// - 查询命中时遍历**所有**前缀点（不只断点），因此即使客户端断点逐轮
///   移动，只要前缀内容一致，当前请求在相同块边界会算出相同哈希而命中
///   历史断点写入的前缀——这是贴近 Anthropic「逐前缀匹配」的关键。
struct PrefixPoint {
    cumulative_hash: u64,
    cumulative_tokens: i32,
    is_breakpoint: bool,
}

fn min_cacheable_tokens(model: &str) -> i32 {
    let normalized = model.to_ascii_lowercase().replace('.', "-");

    if normalized.contains("fable-5") {
        return 512;
    }

    if normalized.contains("opus-4-6")
        || normalized.contains("opus-4-5")
        || normalized.contains("haiku-4-5")
    {
        return 4096;
    }

    if normalized.contains("opus-4-7")
        || normalized.contains("haiku-3-5")
        || normalized.contains("mythos-preview")
    {
        return 2048;
    }

    DEFAULT_MIN_CACHEABLE_TOKENS
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

fn cache_control_enabled(value: &serde_json::Value) -> bool {
    !value.is_null()
        && value
            .as_object()
            .and_then(|obj| obj.get("type"))
            .and_then(|v| v.as_str())
            .map(|t| t == "ephemeral")
            .unwrap_or(true)
}

/// 递归规范化 JSON 对象键顺序，避免 HashMap/客户端字段顺序导致相同前缀哈希不同。
fn canonicalize_json(value: &serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Array(arr) => {
            serde_json::Value::Array(arr.iter().map(canonicalize_json).collect())
        }
        serde_json::Value::Object(map) => {
            let mut keys: Vec<_> = map.keys().collect();
            keys.sort();
            let mut out = serde_json::Map::new();
            for key in keys {
                out.insert(key.clone(), canonicalize_json(&map[key]));
            }
            serde_json::Value::Object(out)
        }
        _ => value.clone(),
    }
}

/// `cache_control` 是断点标记，不是提示词内容；哈希内容时去掉块顶层标记。
fn without_top_level_cache_control(value: &serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(map) => {
            let mut out = map.clone();
            out.remove("cache_control");
            serde_json::Value::Object(out)
        }
        _ => value.clone(),
    }
}

fn stable_json_without_cache_control(value: &serde_json::Value) -> String {
    serde_json::to_string(&canonicalize_json(&without_top_level_cache_control(value)))
        .unwrap_or_default()
}

/// 客户端（如 Claude Code）会在 system 文本里注入传输层元数据行，其中带有
/// **每请求变化的 nonce**——例如：
/// `x-anthropic-billing-header: cc_version=...; cc_entrypoint=...; cch=<随机>;`
/// 的 `cch=` 每次请求都不同。
///
/// 这类行不是提示词内容，但因为前缀按 tools→system→messages **顺序累积**哈希，
/// 任何排在前面的易变行都会污染其后**所有**前缀点的哈希，使稳定的大段 system /
/// 历史消息永远算不出与历史一致的哈希 → cache_read 恒为 0。
///
/// 因此仅在**计算前缀哈希时**剔除这些行；token 计数（`token_text`）与转发给
/// Kiro 上游的实际内容都不受影响，恒等式 `input+creation+read==total` 仍成立。
fn strip_volatile_lines(text: &str) -> String {
    if !text.contains("x-anthropic-billing-header:") {
        return text.to_string();
    }
    text.lines()
        .filter(|line| !line.trim_start().starts_with("x-anthropic-billing-header:"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn feed_segment(
    hasher: &mut Sha256,
    cumulative_tokens: &mut i32,
    label: &str,
    hash_text: &str,
    token_text: &str,
) {
    hasher.update(label.as_bytes());
    hasher.update(b"\x1f");
    // 哈希前剔除每请求变化的传输层元数据行，避免污染累积前缀。
    hasher.update(strip_volatile_lines(hash_text).as_bytes());
    hasher.update(b"\x1e");
    *cumulative_tokens += crate::token::count_tokens(token_text) as i32;
}

/// 按规范顺序（tools → system → messages）遍历输入，在**每个块边界**记录一个
/// 累积前缀点（哈希 + 累积 token），并标记其中带客户端 `cache_control` 的断点。
///
/// 返回 (前缀点列表, 本地累积总 token)。token 计数复用 `crate::token::count_tokens`，
/// 与现有总数估算同源，但这里是按段累加（同一套 tokenizer，不引入新的遍历语义）。
///
/// 关键：记录**每个**块边界（而非仅断点），是为了让查询能命中「客户端断点逐轮
/// 移动、但前缀内容一致」的历史写入——这正是之前只在断点位置存/查导致大量 miss
/// 的根因。写入仍只发生在断点（见 `is_breakpoint`），保持缓存基数稀疏。
fn collect_breakpoints(
    scope_key: &str,
    top_level_cache_control: Option<&serde_json::Value>,
    system: Option<&[SystemMessage]>,
    messages: &[Message],
    tools: Option<&[Tool]>,
) -> (Vec<PrefixPoint>, i32) {
    let mut hasher = Sha256::new();
    hasher.update(scope_key.as_bytes());
    hasher.update(b"\x00");

    let mut cumulative_tokens = 0i32;
    let mut points: Vec<PrefixPoint> = Vec::new();

    // 在当前累积状态上记录一个前缀点（克隆 hasher 以固化累积前缀）。
    let push_point = |hasher: &Sha256,
                      cumulative_tokens: i32,
                      is_breakpoint: bool,
                      points: &mut Vec<PrefixPoint>| {
        points.push(PrefixPoint {
            cumulative_hash: finish_hash(hasher.clone()),
            cumulative_tokens,
            is_breakpoint,
        });
    };

    // 1) tools
    if let Some(tools) = tools {
        for tool in tools {
            let schema = stable_json_without_cache_control(
                &serde_json::to_value(&tool.input_schema).unwrap_or_default(),
            );
            let tool_hash = serde_json::json!({
                "type": tool.tool_type,
                "name": tool.name,
                "description": tool.description,
                "input_schema": serde_json::from_str::<serde_json::Value>(&schema)
                    .unwrap_or(serde_json::Value::Null),
                "max_uses": tool.max_uses,
            });
            let tool_hash = stable_json_without_cache_control(&tool_hash);
            feed_segment(
                &mut hasher,
                &mut cumulative_tokens,
                "tool",
                &tool_hash,
                &format!("{} {} {}", tool.name, tool.description, schema),
            );
            let is_bp = tool
                .cache_control
                .as_ref()
                .map(|v| !v.is_null())
                .unwrap_or(false);
            push_point(&hasher, cumulative_tokens, is_bp, &mut points);
        }
    }

    // 2) system
    if let Some(system) = system {
        for msg in system {
            feed_segment(
                &mut hasher,
                &mut cumulative_tokens,
                "system",
                &msg.text,
                &msg.text,
            );
            let is_bp = msg
                .cache_control
                .as_ref()
                .map(|v| !v.is_null())
                .unwrap_or(false);
            push_point(&hasher, cumulative_tokens, is_bp, &mut points);
        }
    }

    // 3) messages（content 已是 serde_json::Value，直接 walk，无额外解析）
    for msg in messages {
        hasher.update(b"role\x1f");
        hasher.update(msg.role.as_bytes());
        hasher.update(b"\x1e");
        match &msg.content {
            serde_json::Value::String(s) => {
                feed_segment(&mut hasher, &mut cumulative_tokens, "message_text", s, s);
                push_point(&hasher, cumulative_tokens, false, &mut points);
            }
            serde_json::Value::Array(arr) => {
                for block in arr {
                    let stable = stable_json_without_cache_control(block);
                    let token_text = block
                        .get("text")
                        .and_then(|v| v.as_str())
                        .unwrap_or(&stable);
                    feed_segment(
                        &mut hasher,
                        &mut cumulative_tokens,
                        "message_block",
                        &stable,
                        token_text,
                    );
                    push_point(&hasher, cumulative_tokens, has_cache_control(block), &mut points);
                }
            }
            _ => {}
        }
    }

    // 顶层 cache_control 是 Anthropic automatic caching：自动把断点放到最后一个
    // 可缓存块。这里把最末前缀点标记为断点（若尚未标记），避免 sub2api 接入
    // 自动缓存请求时看不到 cache_*。
    if top_level_cache_control
        .map(cache_control_enabled)
        .unwrap_or(false)
        && cumulative_tokens > 0
    {
        if let Some(last) = points.last_mut() {
            last.is_breakpoint = true;
        }
    }

    (points, cumulative_tokens)
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

    fn test_cache() -> PrefixCache {
        PrefixCache::new(CACHE_TTL, MAX_CACHE_ENTRIES)
    }

    fn compute_with_cache(
        cache: &PrefixCache,
        scope_key: &str,
        model: &str,
        top_level_cache_control: Option<&serde_json::Value>,
        system: Option<&[SystemMessage]>,
        messages: &[Message],
        tools: Option<&[Tool]>,
        total_input_tokens: i32,
    ) -> CacheSplit {
        compute_split_with_cache(
            cache,
            scope_key,
            model,
            top_level_cache_control,
            system,
            messages,
            tools,
            total_input_tokens,
        )
    }

    #[test]
    fn no_breakpoint_is_passthrough() {
        let split = compute_split(
            "scope-a",
            "claude-sonnet-4-6",
            None,
            None,
            &[user_msg("hi")],
            None,
            5000,
        );
        assert_eq!(split, CacheSplit::passthrough(5000));
    }

    #[test]
    fn identity_always_holds() {
        let big = "x ".repeat(5000); // 远超最小缓存门槛
        let system = vec![sys(&big, true)];
        let split = compute_split(
            "scope-id",
            "claude-sonnet-4-6",
            None,
            Some(&system),
            &[user_msg("question")],
            None,
            9000,
        );
        assert_eq!(
            split.input_tokens + split.cache_creation_input_tokens + split.cache_read_input_tokens,
            9000
        );
    }

    #[test]
    fn first_call_creates_second_call_reads() {
        let cache = test_cache();
        let big = "token ".repeat(5000);
        let system = vec![sys(&big, true)];

        // 第一次：应当是 creation（read 为 0）
        let first = compute_with_cache(
            &cache,
            "scope-flow",
            "claude-sonnet-4-6",
            None,
            Some(&system),
            &[user_msg("q1")],
            None,
            8000,
        );
        assert_eq!(first.cache_read_input_tokens, 0);
        assert!(first.cache_creation_input_tokens > 0);

        // 第二次：相同前缀应命中 read
        let second = compute_with_cache(
            &cache,
            "scope-flow",
            "claude-sonnet-4-6",
            None,
            Some(&system),
            &[user_msg("q2 different tail")],
            None,
            8000,
        );
        assert!(second.cache_read_input_tokens > 0);
    }

    #[test]
    fn moving_breakpoint_still_hits_stable_prefix() {
        // 复现真实场景：Claude Code 每轮把 cache_control 断点移到对话末尾，
        // 但前面的 system + 历史消息逐字节稳定。第二轮断点虽移到了更靠后的
        // 位置，仍应命中第一轮在较前位置写入的稳定前缀。
        let cache = test_cache();
        let big_sys = "stable-system ".repeat(5000); // 稳定前缀，带断点
        let system = vec![sys(&big_sys, true)];

        // 第一轮：system 断点写入，message 只有一条短的（无断点）。
        let first = compute_with_cache(
            &cache,
            "scope-move",
            "claude-sonnet-4-6",
            None,
            Some(&system),
            &[user_msg("round one question")],
            None,
            9000,
        );
        assert_eq!(first.cache_read_input_tokens, 0, "首轮无命中");
        assert!(first.cache_creation_input_tokens > 0);

        // 第二轮：相同 system（断点仍在），但对话变长——追加历史消息，
        // 末尾再补一条带断点的消息（模拟断点“移动”到更靠后）。
        let tail_block = json!([{
            "type": "text",
            "text": "round two appended content ".repeat(20),
            "cache_control": {"type": "ephemeral"}
        }]);
        let second = compute_with_cache(
            &cache,
            "scope-move",
            "claude-sonnet-4-6",
            None,
            Some(&system),
            &[
                user_msg("round one question"),
                Message { role: "assistant".to_string(), content: json!("ok") },
                Message { role: "user".to_string(), content: tail_block },
            ],
            None,
            9000,
        );

        // 关键断言：尽管断点移动了，稳定的 system 前缀仍应命中 read。
        assert!(
            second.cache_read_input_tokens > 0,
            "断点移动后，稳定前缀仍应命中缓存读取（修复前这里为 0）"
        );
    }

    #[test]
    fn scope_isolation() {
        let cache = test_cache();
        let big = "alpha ".repeat(5000);
        let system = vec![sys(&big, true)];
        let _ = compute_with_cache(
            &cache,
            "scope-A",
            "claude-sonnet-4-6",
            None,
            Some(&system),
            &[user_msg("q")],
            None,
            8000,
        );
        // 不同 scope 不应命中
        let other = compute_with_cache(
            &cache,
            "scope-B",
            "claude-sonnet-4-6",
            None,
            Some(&system),
            &[user_msg("q")],
            None,
            8000,
        );
        assert_eq!(other.cache_read_input_tokens, 0);
    }

    #[test]
    fn volatile_billing_header_does_not_break_prefix_hit() {
        // 复现真实根因：Claude Code 在 system 文本里注入传输层元数据行，
        // 其 `cch=` 是每请求变化的 nonce。它排在前缀哈希最前面，若不剔除会污染
        // 其后所有累积前缀点的哈希，使稳定的大段 system 永远 miss（cache_read 恒 0）。
        let cache = test_cache();
        let stable = "stable-system-prompt ".repeat(5000); // 稳定大段，带断点

        // 两轮唯一区别：billing header 的 cch nonce 不同（其余逐字节一致）。
        let sys_round = |nonce: &str| {
            vec![sys(
                &format!(
                    "x-anthropic-billing-header: cc_version=2.1; cc_entrypoint=vscode; cch={nonce};\n{stable}"
                ),
                true,
            )]
        };

        let first = compute_with_cache(
            &cache,
            "scope-volatile",
            "claude-sonnet-4-6",
            None,
            Some(&sys_round("c35ac")),
            &[user_msg("q1")],
            None,
            8000,
        );
        assert_eq!(first.cache_read_input_tokens, 0, "首轮无命中");
        assert!(first.cache_creation_input_tokens > 0);

        // 第二轮：nonce 变了，但稳定前缀应仍命中（修复前这里为 0）。
        let second = compute_with_cache(
            &cache,
            "scope-volatile",
            "claude-sonnet-4-6",
            None,
            Some(&sys_round("664fc")),
            &[user_msg("q2 different tail")],
            None,
            8000,
        );
        assert!(
            second.cache_read_input_tokens > 0,
            "billing header nonce 变化不应破坏稳定前缀的缓存命中"
        );
    }

    #[test]
    fn non_text_tail_after_breakpoint_stays_input() {
        let prefix = "stable ".repeat(5000);
        let tail = "tail ".repeat(12000);
        let system = vec![sys(&prefix, true)];
        let messages = vec![Message {
            role: "user".to_string(),
            content: json!([{
                "type": "document",
                "source": {
                    "type": "text",
                    "media_type": "text/plain",
                    "data": tail
                }
            }]),
        }];

        let split = compute_split(
            "scope-tail",
            "claude-sonnet-4-6",
            None,
            Some(&system),
            &messages,
            None,
            12000,
        );

        assert!(
            split.input_tokens > 0,
            "content after the cache breakpoint must remain regular input"
        );
        assert!(
            split.cache_creation_input_tokens < 12000,
            "cache creation must not consume the whole request when a large tail follows"
        );
    }

    #[test]
    fn top_level_cache_control_marks_last_block() {
        let cache = test_cache();
        let big = "auto ".repeat(5000);
        let cache_control = json!({"type": "ephemeral"});
        let messages = vec![user_msg(&big)];

        let first = compute_with_cache(
            &cache,
            "scope-auto",
            "claude-sonnet-4-6",
            Some(&cache_control),
            None,
            &messages,
            None,
            7000,
        );
        assert!(first.cache_creation_input_tokens > 0);
        assert_eq!(first.cache_read_input_tokens, 0);

        let second = compute_with_cache(
            &cache,
            "scope-auto",
            "claude-sonnet-4-6",
            Some(&cache_control),
            None,
            &messages,
            None,
            7000,
        );
        assert!(second.cache_read_input_tokens > 0);
    }

    #[test]
    fn model_specific_minimum_is_respected() {
        let medium = "m ".repeat(3000);
        let system = vec![sys(&medium, true)];

        let split = compute_split(
            "scope-opus-min",
            "claude-opus-4-6",
            None,
            Some(&system),
            &[user_msg("q")],
            None,
            3000,
        );

        assert_eq!(split, CacheSplit::passthrough(3000));
    }

    #[test]
    fn expired_prefix_does_not_hit() {
        let cache = PrefixCache::new(Duration::from_millis(25), MAX_CACHE_ENTRIES);
        let big = "expire ".repeat(5000);
        let system = vec![sys(&big, true)];

        let first = compute_with_cache(
            &cache,
            "scope-expire",
            "claude-sonnet-4-6",
            None,
            Some(&system),
            &[user_msg("q")],
            None,
            8000,
        );
        assert!(first.cache_creation_input_tokens > 0);
        assert_eq!(first.cache_read_input_tokens, 0);

        std::thread::sleep(Duration::from_millis(50));
        cache.run_pending_tasks();

        let second = compute_with_cache(
            &cache,
            "scope-expire",
            "claude-sonnet-4-6",
            None,
            Some(&system),
            &[user_msg("q again")],
            None,
            8000,
        );
        assert_eq!(second.cache_read_input_tokens, 0);
        assert!(second.cache_creation_input_tokens > 0);
    }

    #[test]
    fn active_prefix_stays_alive_via_idle_refresh() {
        // time_to_idle 滑动窗口：在空闲超时内持续访问应一直命中，
        // 即便总时长已超过单个 TTL。
        let idle = Duration::from_millis(80);
        let cache = PrefixCache::new(idle, MAX_CACHE_ENTRIES);
        let big = "active ".repeat(5000);
        let system = vec![sys(&big, true)];

        let call = |tail: &str| {
            compute_with_cache(
                &cache,
                "scope-active",
                "claude-sonnet-4-6",
                None,
                Some(&system),
                &[user_msg(tail)],
                None,
                8000,
            )
        };

        // 首次写入。
        assert_eq!(call("q0").cache_read_input_tokens, 0);

        // 在空闲窗口内反复访问，累计时长超过单个 idle TTL，仍应保持命中。
        for i in 1..=4 {
            std::thread::sleep(Duration::from_millis(40));
            cache.run_pending_tasks();
            let split = call(&format!("q{i}"));
            assert!(
                split.cache_read_input_tokens > 0,
                "第 {i} 次访问应命中（滑动窗口续期）"
            );
        }
    }

    #[test]
    fn prefix_cache_respects_capacity() {
        // 用较大的容量与远超容量的插入量，避免 moka 在极小容量（如 1）下
        // 淘汰时机不精确导致的脆弱断言。max_capacity 是近似上限，这里只断言
        // 最终条目数被约束在容量附近的合理范围内。
        const CAP: u64 = 100;
        let cache = PrefixCache::new(CACHE_TTL, CAP);

        for key in 0..CAP * 10 {
            cache.insert(key);
        }
        cache.run_pending_tasks();

        let count = cache.entry_count();
        assert!(
            count <= CAP + CAP / 10,
            "entry_count {} 超出容量 {} 的合理范围",
            count,
            CAP
        );
    }

    #[test]
    fn no_breakpoint_does_not_write_cache() {
        let cache = test_cache();
        let split = compute_with_cache(
            &cache,
            "scope-no-write",
            "claude-sonnet-4-6",
            None,
            None,
            &[user_msg("hi")],
            None,
            5000,
        );

        assert_eq!(split, CacheSplit::passthrough(5000));
        cache.run_pending_tasks();
        assert_eq!(cache.entry_count(), 0);
    }

    #[test]
    fn dampen_read_moves_to_creation_and_preserves_identity() {
        let split = CacheSplit {
            input_tokens: 100,
            cache_creation_input_tokens: 200,
            cache_read_input_tokens: 1000,
        };
        let out = split.dampen_read(0.3);
        // read 衰减到 30%，被砍掉的 700 转入 creation。
        assert_eq!(out.cache_read_input_tokens, 300);
        assert_eq!(out.cache_creation_input_tokens, 200 + 700);
        assert_eq!(out.input_tokens, 100); // input 不动
        // 恒等式保持。
        assert_eq!(
            out.input_tokens + out.cache_creation_input_tokens + out.cache_read_input_tokens,
            split.input_tokens
                + split.cache_creation_input_tokens
                + split.cache_read_input_tokens
        );
    }

    #[test]
    fn dampen_read_factor_one_is_identity() {
        let split = CacheSplit {
            input_tokens: 42,
            cache_creation_input_tokens: 7,
            cache_read_input_tokens: 999,
        };
        assert_eq!(split.dampen_read(1.0), split);
    }

    #[test]
    fn dampen_read_clamps_out_of_range_factor() {
        let split = CacheSplit {
            input_tokens: 0,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 500,
        };
        // factor < 0 夹到 0：read 全部转入 creation。
        let low = split.dampen_read(-1.0);
        assert_eq!(low.cache_read_input_tokens, 0);
        assert_eq!(low.cache_creation_input_tokens, 500);
        // factor > 1 夹到 1：保持不变。
        let high = split.dampen_read(2.0);
        assert_eq!(high, split);
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
    model: &str,
    top_level_cache_control: Option<&serde_json::Value>,
    system: Option<&[SystemMessage]>,
    messages: &[Message],
    tools: Option<&[Tool]>,
    total_input_tokens: i32,
) -> CacheSplit {
    compute_split_with_cache(
        &PREFIX_CACHE,
        scope_key,
        model,
        top_level_cache_control,
        system,
        messages,
        tools,
        total_input_tokens,
    )
}

fn compute_split_with_cache(
    cache: &PrefixCache,
    scope_key: &str,
    model: &str,
    top_level_cache_control: Option<&serde_json::Value>,
    system: Option<&[SystemMessage]>,
    messages: &[Message],
    tools: Option<&[Tool]>,
    total_input_tokens: i32,
) -> CacheSplit {
    let total = total_input_tokens.max(0);

    // 收集每个块边界的累积前缀点，并标记其中的客户端断点。
    // 累积 token 用本地估算单位，最后按真实总数 `total` 缩放，保证恒等式成立。
    let (points, local_total) =
        collect_breakpoints(scope_key, top_level_cache_control, system, messages, tools);

    // 没有任何客户端断点 —— 与真实 Anthropic 行为一致：不显示缓存。
    let has_breakpoint = points.iter().any(|p| p.is_breakpoint);
    if !has_breakpoint || local_total == 0 {
        return CacheSplit::passthrough(total);
    }

    // 把本地累积 token 缩放到真实总数单位。
    let scale = |local: i32| -> i32 { ((local as i64 * total as i64) / local_total as i64) as i32 };

    let min_cacheable_tokens = min_cacheable_tokens(model);

    // 查询：遍历**所有**达标前缀点找最长命中。即使客户端断点逐轮移动，只要
    // 前缀内容一致，当前请求在相同块边界算出的哈希就与历史断点写入相同 → 命中。
    // 命中的 `contains`（内部 get）在 time_to_idle 下已刷新空闲计时。
    let mut hit_local = 0i32;
    for p in &points {
        if p.cumulative_tokens >= min_cacheable_tokens && cache.contains(p.cumulative_hash) {
            hit_local = p.cumulative_tokens;
        }
    }

    // 写入：只在客户端断点处写（稀疏，控制缓存基数），供后续请求命中。
    for p in &points {
        if p.is_breakpoint && p.cumulative_tokens >= min_cacheable_tokens {
            cache.insert(p.cumulative_hash);
        }
    }

    // 最大可缓存边界 = 最后一个达标断点的累积 token（本地单位）。
    let cacheable_local = points
        .iter()
        .rev()
        .find(|p| p.is_breakpoint && p.cumulative_tokens >= min_cacheable_tokens)
        .map(|p| p.cumulative_tokens)
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
