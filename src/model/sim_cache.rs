//! 模拟缓存的运行时可调设置
//!
//! 三个字段（开关 + 读/写比例）都用原子存储：admin 控制台更新后，
//! 后续请求立刻读到新值（实时生效），无锁、无需重启。
//! `f64` 通过 `to_bits`/`from_bits` 存进 `AtomicU64`。
//!
//! 启动时从 `Config` 初始化；admin 更新时同步写回配置文件，重启后保持。

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

/// 模拟缓存设置的一致性快照（admin GET 响应 / 写回配置用）。
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SimulateCacheSnapshot {
    /// 是否启用模拟缓存。
    pub enabled: bool,
    /// 缓存读取占比（0~1）。
    pub read_ratio: f64,
    /// 缓存写入占比（0~1）。
    pub write_ratio: f64,
}

/// 运行时可调的模拟缓存设置（进程内共享，`Arc` 持有）。
#[derive(Debug)]
pub struct SimulateCacheSettings {
    enabled: AtomicBool,
    read_ratio_bits: AtomicU64,
    write_ratio_bits: AtomicU64,
}

impl SimulateCacheSettings {
    pub fn new(enabled: bool, read_ratio: f64, write_ratio: f64) -> Self {
        Self {
            enabled: AtomicBool::new(enabled),
            read_ratio_bits: AtomicU64::new(sanitize(read_ratio).to_bits()),
            write_ratio_bits: AtomicU64::new(sanitize(write_ratio).to_bits()),
        }
    }

    pub fn enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    pub fn read_ratio(&self) -> f64 {
        f64::from_bits(self.read_ratio_bits.load(Ordering::Relaxed))
    }

    pub fn write_ratio(&self) -> f64 {
        f64::from_bits(self.write_ratio_bits.load(Ordering::Relaxed))
    }

    /// 整体更新（admin 用）。比例夹到 [0,1]；read+write 是否 ≤1 由 API 层校验。
    pub fn update(&self, snapshot: SimulateCacheSnapshot) {
        self.enabled.store(snapshot.enabled, Ordering::Relaxed);
        self.read_ratio_bits
            .store(sanitize(snapshot.read_ratio).to_bits(), Ordering::Relaxed);
        self.write_ratio_bits
            .store(sanitize(snapshot.write_ratio).to_bits(), Ordering::Relaxed);
    }

    pub fn snapshot(&self) -> SimulateCacheSnapshot {
        SimulateCacheSnapshot {
            enabled: self.enabled(),
            read_ratio: self.read_ratio(),
            write_ratio: self.write_ratio(),
        }
    }
}

/// 比例夹到 `[0.0, 1.0]`；非有限值（NaN/±inf）按 0 处理。
fn sanitize(ratio: f64) -> f64 {
    if ratio.is_finite() {
        ratio.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn update_is_visible_immediately() {
        let settings = SimulateCacheSettings::new(false, 0.8, 0.1);
        assert!(!settings.enabled());

        settings.update(SimulateCacheSnapshot {
            enabled: true,
            read_ratio: 0.5,
            write_ratio: 0.2,
        });
        assert!(settings.enabled());
        assert_eq!(settings.read_ratio(), 0.5);
        assert_eq!(settings.write_ratio(), 0.2);
    }

    #[test]
    fn ratios_are_sanitized_on_write() {
        let settings = SimulateCacheSettings::new(true, -1.0, f64::NAN);
        assert_eq!(settings.read_ratio(), 0.0);
        assert_eq!(settings.write_ratio(), 0.0);

        settings.update(SimulateCacheSnapshot {
            enabled: true,
            read_ratio: 2.0,
            write_ratio: f64::INFINITY,
        });
        assert_eq!(settings.read_ratio(), 1.0);
        assert_eq!(settings.write_ratio(), 0.0);
    }
}
