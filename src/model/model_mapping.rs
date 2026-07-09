//! 模型映射表：Anthropic 模型名 → Kiro 模型 ID（config 驱动，运行时可热更新）
//!
//! 映射规则存放在进程级全局表中：启动时由 `config.modelMappings` 初始化，
//! admin 控制台整表替换后对后续请求实时生效（改动会写回配置文件）。
//!
//! 之所以用全局表而非沿 `AppState` 注入：消费方 `map_model` /
//! `get_context_window_size` 是 converter/stream 深处的纯函数，逐层穿
//! `Arc` 改动面过大；全局 `RwLock<Arc<_>>` 读取为快照克隆，写入整表原子替换。
//!
//! 匹配规则：把请求模型名小写并把 `.` 归一为 `-` 后，逐条（按配置顺序）检查
//! 是否包含该条目的全部 `keywords`，第一条命中即映射到其 `target`。
//! 例如 `["sonnet", "4-6"]` 同时命中 `claude-sonnet-4-6-thinking` 与
//! `claude-sonnet-4.6`。条目顺序同时决定 `/v1/models` 的展示顺序。

use std::sync::{Arc, LazyLock};

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};

/// 单条模型映射规则
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelMapping {
    /// 对外展示的模型 ID（`/v1/models` 用，如 "claude-sonnet-4-6"）
    pub id: String,

    /// 展示名称（如 "Claude Sonnet 4.6"）
    pub display_name: String,

    /// 匹配关键字：归一化后的请求模型名须包含全部关键字才命中
    pub keywords: Vec<String>,

    /// Kiro 上游模型 ID（如 "claude-sonnet-4.6"）
    pub target: String,

    /// 上下文窗口大小（tokens）
    #[serde(default = "default_context_window")]
    pub context_window: i32,

    /// 最大输出 tokens（`/v1/models` 展示用）
    #[serde(default = "default_max_tokens")]
    pub max_tokens: i32,

    /// 发布时间（Unix 秒，`/v1/models` 展示用，可留 0）
    #[serde(default)]
    pub created: i64,
}

fn default_context_window() -> i32 {
    200_000
}

fn default_max_tokens() -> i32 {
    64_000
}

/// 内置默认映射表（与 config 缺省值一致；也是未初始化时的兜底）
pub fn default_mappings() -> Vec<ModelMapping> {
    fn entry(
        id: &str,
        display_name: &str,
        keywords: &[&str],
        target: &str,
        context_window: i32,
        max_tokens: i32,
        created: i64,
    ) -> ModelMapping {
        ModelMapping {
            id: id.to_string(),
            display_name: display_name.to_string(),
            keywords: keywords.iter().map(|k| k.to_string()).collect(),
            target: target.to_string(),
            context_window,
            max_tokens,
            created,
        }
    }

    vec![
        entry("claude-opus-4-8", "Claude Opus 4.8", &["opus", "4-8"], "claude-opus-4.8", 1_000_000, 128_000, 1_779_897_600),
        entry("claude-opus-4-7", "Claude Opus 4.7", &["opus", "4-7"], "claude-opus-4.7", 1_000_000, 64_000, 1_776_276_000),
        entry("claude-opus-4-6", "Claude Opus 4.6", &["opus", "4-6"], "claude-opus-4.6", 1_000_000, 64_000, 1_770_163_200),
        entry("claude-sonnet-4-6", "Claude Sonnet 4.6", &["sonnet", "4-6"], "claude-sonnet-4.6", 1_000_000, 64_000, 1_771_286_400),
        entry("claude-opus-4-5-20251101", "Claude Opus 4.5", &["opus", "4-5"], "claude-opus-4.5", 200_000, 64_000, 1_763_942_400),
        entry("claude-sonnet-4-5-20250929", "Claude Sonnet 4.5", &["sonnet", "4-5"], "claude-sonnet-4.5", 200_000, 64_000, 1_759_104_000),
        entry("claude-haiku-4-5-20251001", "Claude Haiku 4.5", &["haiku"], "claude-haiku-4.5", 200_000, 64_000, 1_760_486_400),
    ]
}

static TABLE: LazyLock<RwLock<Arc<Vec<ModelMapping>>>> =
    LazyLock::new(|| RwLock::new(Arc::new(default_mappings())));

/// 整表替换（启动初始化与 admin 更新共用），对后续请求实时生效
pub fn update(mappings: Vec<ModelMapping>) {
    *TABLE.write() = Arc::new(mappings);
}

/// 当前映射表快照
pub fn current() -> Arc<Vec<ModelMapping>> {
    TABLE.read().clone()
}

/// 按匹配规则解析模型名，返回当前表中第一条命中的映射
pub fn resolve(model: &str) -> Option<ModelMapping> {
    resolve_in(&current(), model).cloned()
}

/// 在给定映射表中解析模型名（keywords 为空的条目不参与匹配）
pub fn resolve_in<'a>(mappings: &'a [ModelMapping], model: &str) -> Option<&'a ModelMapping> {
    let normalized = normalize(model);
    mappings.iter().find(|m| {
        !m.keywords.is_empty()
            && m.keywords.iter().all(|k| normalized.contains(&normalize(k)))
    })
}

/// 小写 + `.` 归一为 `-`，让 "4.6" 与 "4-6" 等价
fn normalize(s: &str) -> String {
    s.to_lowercase().replace('.', "-")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(keywords: &[&str], target: &str, window: i32) -> ModelMapping {
        ModelMapping {
            id: target.to_string(),
            display_name: target.to_string(),
            keywords: keywords.iter().map(|k| k.to_string()).collect(),
            target: target.to_string(),
            context_window: window,
            max_tokens: 64_000,
            created: 0,
        }
    }

    #[test]
    fn default_table_matches_known_models() {
        // 默认表未显式初始化时即生效（LazyLock 兜底）
        let m = resolve("claude-sonnet-4-6-thinking").expect("sonnet 4.6 应命中");
        assert_eq!(m.target, "claude-sonnet-4.6");
        assert_eq!(m.context_window, 1_000_000);

        // "." 与 "-" 等价
        let m = resolve("claude-sonnet-4.5").expect("sonnet 4.5 应命中");
        assert_eq!(m.target, "claude-sonnet-4.5");

        // haiku 不限版本
        let m = resolve("claude-haiku-9-9").expect("haiku 应命中");
        assert_eq!(m.target, "claude-haiku-4.5");

        assert!(resolve("gpt-4").is_none());
        assert!(resolve("claude-sonnet-5").is_none(), "未配置的版本不应命中");
    }

    #[test]
    fn resolve_in_uses_first_match_and_skips_empty_keywords() {
        // 用局部表测纯函数，不动全局（converter 测试并行读全局表）
        let table = vec![
            rule(&[], "empty-keywords-never-matches", 1),
            rule(&["sonnet", "5"], "kiro-sonnet-5", 1_000_000),
            rule(&["sonnet"], "kiro-sonnet-fallback", 200_000),
        ];

        assert_eq!(
            resolve_in(&table, "claude-sonnet-5").unwrap().target,
            "kiro-sonnet-5"
        );
        // 兜底规则排在后面，仅在前面未命中时生效
        assert_eq!(
            resolve_in(&table, "claude-sonnet-4-9").unwrap().target,
            "kiro-sonnet-fallback"
        );
        assert!(resolve_in(&table, "claude-opus-4-6").is_none());
    }
}
