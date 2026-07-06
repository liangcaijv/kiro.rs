//! 模拟 prompt 缓存（按固定比例拆分）
//!
//! ⚠️ 重要：Kiro 后端**不支持** prompt 缓存，也不返回任何缓存 token 信息。
//! 本模块在代理出口处**伪造** `cache_creation_input_tokens` /
//! `cache_read_input_tokens` 字段，仅用于让 sub2api 等统计面板的缓存指标
//! 不为 0、成本曲线接近真实 Anthropic。它**不会**真的节省 token、额度或耗时——
//! 真实开销在 Kiro 侧一分没少。
//!
//! 工作原理（2026-07 起的简化实现）：
//! 每次请求把估算的总输入 token 按可配置比例直接拆成三份——
//! `cache_read = total × read_ratio`、`cache_creation = total × write_ratio`、
//! 剩余计入 `input_tokens`。不看客户端 `cache_control` 断点、不做前缀哈希追踪、
//! 无任何跨请求状态。比例可在 admin 控制台实时调整（见
//! `crate::model::sim_cache::SimulateCacheSettings`）。
//!
//! 恒等式：`input + cache_creation + cache_read == 总输入 token` 恒成立。
//!
//! （历史：2026-06 曾实现有状态的前缀哈希追踪 + moka 缓存 + dampen_read 折扣，
//! 见 git 历史 `docs/superpowers/specs/2026-06-17-moka-prefix-cache-design.md`；
//! 已按「固定比例」需求整体移除。）

/// 缓存拆分结果：三者之和等于总输入 token。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CacheSplit {
    /// 计入正常 input 的 token（总数扣除两个缓存字段后的剩余）。
    pub input_tokens: i32,
    /// 本次计入缓存写入的 token（按约 1.25 倍价计费）。
    pub cache_creation_input_tokens: i32,
    /// 本次计入缓存读取的 token（按约 1/10 价计费）。
    pub cache_read_input_tokens: i32,
}

/// 把比例夹到 `[0.0, 1.0]`；非有限值（NaN/±inf）按 0 处理。
fn sanitize_ratio(ratio: f64) -> f64 {
    if ratio.is_finite() {
        ratio.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

/// 按固定比例计算请求的缓存拆分。
///
/// - `total_input_tokens`：已算好的总输入 token（复用现有计数，负数按 0 处理）。
/// - `read_ratio`：缓存读取占比（0~1）。
/// - `write_ratio`：缓存写入占比（0~1）。两者之和超过 1 时读取优先、写入让位。
///
/// 取整规则：read/write 各自四舍五入，write 不超过剩余额度，保证
/// `input + cache_creation + cache_read == total` 且三者均非负。
pub fn compute_split(total_input_tokens: i32, read_ratio: f64, write_ratio: f64) -> CacheSplit {
    let total = total_input_tokens.max(0);
    let read_ratio = sanitize_ratio(read_ratio);
    let mut write_ratio = sanitize_ratio(write_ratio);
    if read_ratio + write_ratio > 1.0 {
        write_ratio = 1.0 - read_ratio;
    }

    let read = ((total as f64) * read_ratio).round() as i32;
    let read = read.min(total);
    let write = ((total as f64) * write_ratio).round() as i32;
    let write = write.min(total - read);

    CacheSplit {
        input_tokens: total - read - write,
        cache_creation_input_tokens: write,
        cache_read_input_tokens: read,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_identity(split: CacheSplit, total: i32) {
        assert_eq!(
            split.input_tokens + split.cache_creation_input_tokens + split.cache_read_input_tokens,
            total,
            "恒等式必须成立: {split:?}"
        );
        assert!(split.input_tokens >= 0, "{split:?}");
        assert!(split.cache_creation_input_tokens >= 0, "{split:?}");
        assert!(split.cache_read_input_tokens >= 0, "{split:?}");
    }

    #[test]
    fn default_ratios_split_as_expected() {
        // 默认配置：读 80% / 写 10% / 剩余 10% 正常 input。
        let split = compute_split(10_000, 0.8, 0.1);
        assert_eq!(split.cache_read_input_tokens, 8_000);
        assert_eq!(split.cache_creation_input_tokens, 1_000);
        assert_eq!(split.input_tokens, 1_000);
        assert_identity(split, 10_000);
    }

    #[test]
    fn identity_holds_across_totals_and_ratios() {
        let ratios = [0.0, 0.1, 0.3, 0.5, 0.8, 0.9, 1.0];
        for total in [0, 1, 2, 3, 7, 999, 1024, 52_341, i32::MAX] {
            for &r in &ratios {
                for &w in &ratios {
                    assert_identity(compute_split(total, r, w), total.max(0));
                }
            }
        }
    }

    #[test]
    fn ratios_over_one_favor_read() {
        // read + write > 1：读取优先，写入让位到剩余额度。
        let split = compute_split(10_000, 0.8, 0.5);
        assert_eq!(split.cache_read_input_tokens, 8_000);
        assert_eq!(split.cache_creation_input_tokens, 2_000);
        assert_eq!(split.input_tokens, 0);
        assert_identity(split, 10_000);
    }

    #[test]
    fn out_of_range_ratios_are_clamped() {
        // 负比例夹到 0，超过 1 夹到 1。
        let split = compute_split(1_000, -0.5, 2.0);
        assert_eq!(split.cache_read_input_tokens, 0);
        assert_eq!(split.cache_creation_input_tokens, 1_000);
        assert_eq!(split.input_tokens, 0);

        // NaN 按 0 处理。
        let split = compute_split(1_000, f64::NAN, f64::INFINITY);
        assert_eq!(split.cache_read_input_tokens, 0);
        assert_eq!(split.cache_creation_input_tokens, 0);
        assert_eq!(split.input_tokens, 1_000);
    }

    #[test]
    fn zero_ratios_are_passthrough() {
        let split = compute_split(5_000, 0.0, 0.0);
        assert_eq!(split.input_tokens, 5_000);
        assert_eq!(split.cache_creation_input_tokens, 0);
        assert_eq!(split.cache_read_input_tokens, 0);
    }

    #[test]
    fn negative_total_treated_as_zero() {
        let split = compute_split(-42, 0.8, 0.1);
        assert_eq!(split, compute_split(0, 0.8, 0.1));
        assert_identity(split, 0);
    }

    #[test]
    fn tiny_totals_never_break_identity() {
        // total=1、双 0.5：四舍五入后 read=1，write 被剩余额度压回 0。
        let split = compute_split(1, 0.5, 0.5);
        assert_eq!(split.cache_read_input_tokens, 1);
        assert_eq!(split.cache_creation_input_tokens, 0);
        assert_eq!(split.input_tokens, 0);
        assert_identity(split, 1);
    }

    #[test]
    fn full_read_ratio_consumes_everything() {
        let split = compute_split(8_192, 1.0, 0.5);
        assert_eq!(split.cache_read_input_tokens, 8_192);
        assert_eq!(split.cache_creation_input_tokens, 0);
        assert_eq!(split.input_tokens, 0);
    }
}
