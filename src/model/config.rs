use anyhow::Context;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum TlsBackend {
    Rustls,
    NativeTls,
}

impl Default for TlsBackend {
    fn default() -> Self {
        Self::Rustls
    }
}

/// 中转接口配置
///
/// 启用后，聊天请求（generateAssistantResponse）会优先发送到中转接口，
/// 中转接口任何失败（发送出错或非 2xx 响应）都会回退到真实 Kiro 流程。
/// 中转接口固定不走代理。
///
/// 聊天接口（`url`）与 MCP/WebSearch 接口（`mcp_url`）各自独立：
/// 仅配置 `url` 时只有聊天走中转，WebSearch 仍直连真实 Kiro；
/// 同时配置 `mcp_url` 后 WebSearch 也走中转。两者共用 `api_key`。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RelayConfig {
    /// 是否启用中转
    #[serde(default)]
    pub enabled: bool,

    /// 聊天接口中转地址（如 http://host:port/sendMessage）
    #[serde(default)]
    pub url: Option<String>,

    /// MCP/WebSearch 接口中转地址（如 http://host:port/mcp）
    #[serde(default)]
    pub mcp_url: Option<String>,

    /// 中转接口密钥（写入 X-Api-Key 请求头，聊天与 MCP 共用）
    #[serde(default)]
    pub api_key: Option<String>,
}

impl RelayConfig {
    /// 聊天中转是否生效：enabled 且 url、api_key 均非空
    pub fn is_active(&self) -> bool {
        self.enabled
            && self.url.as_deref().is_some_and(|u| !u.trim().is_empty())
            && self.api_key.as_deref().is_some_and(|k| !k.trim().is_empty())
    }

    /// MCP/WebSearch 中转是否生效：enabled 且 mcp_url、api_key 均非空
    pub fn is_mcp_active(&self) -> bool {
        self.enabled
            && self.mcp_url.as_deref().is_some_and(|u| !u.trim().is_empty())
            && self.api_key.as_deref().is_some_and(|k| !k.trim().is_empty())
    }
}

/// KNA 应用配置
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Config {
    #[serde(default = "default_host")]
    pub host: String,

    #[serde(default = "default_port")]
    pub port: u16,

    #[serde(default = "default_region")]
    pub region: String,

    /// Auth Region（用于 Token 刷新），未配置时回退到 region
    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auth_region: Option<String>,

    /// API Region（用于 API 请求），未配置时回退到 region
    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_region: Option<String>,

    #[serde(default = "default_kiro_version")]
    pub kiro_version: String,

    #[serde(default)]
    pub machine_id: Option<String>,

    #[serde(default)]
    pub api_key: Option<String>,

    #[serde(default = "default_system_version")]
    pub system_version: String,

    #[serde(default = "default_node_version")]
    pub node_version: String,

    #[serde(default = "default_tls_backend")]
    pub tls_backend: TlsBackend,

    /// 外部 count_tokens API 地址（可选）
    #[serde(default)]
    pub count_tokens_api_url: Option<String>,

    /// count_tokens API 密钥（可选）
    #[serde(default)]
    pub count_tokens_api_key: Option<String>,

    /// count_tokens API 认证类型（可选，"x-api-key" 或 "bearer"，默认 "x-api-key"）
    #[serde(default = "default_count_tokens_auth_type")]
    pub count_tokens_auth_type: String,

    /// HTTP 代理地址（可选）
    /// 支持格式: http://host:port, https://host:port, socks5://host:port
    #[serde(default)]
    pub proxy_url: Option<String>,

    /// 代理认证用户名（可选）
    #[serde(default)]
    pub proxy_username: Option<String>,

    /// 代理认证密码（可选）
    #[serde(default)]
    pub proxy_password: Option<String>,

    /// Admin API 密钥（可选，启用 Admin API 功能）
    #[serde(default)]
    pub admin_api_key: Option<String>,

    /// 负载均衡模式（"priority" 或 "balanced"）
    #[serde(default = "default_load_balancing_mode")]
    pub load_balancing_mode: String,

    /// 是否开启非流式响应的 thinking 块提取（默认 true）
    ///
    /// 启用后，非流式响应中的 `<thinking>...</thinking>` 标签会被解析为
    /// 独立的 `{"type": "thinking", ...}` 内容块,与流式响应行为一致。
    #[serde(default = "default_extract_thinking")]
    pub extract_thinking: bool,

    /// 是否模拟 prompt 缓存命中（默认 false）
    ///
    /// ⚠️ Kiro 后端不支持 prompt 缓存，也不返回缓存 token。启用后代理会在出口
    /// **伪造** `cache_creation_input_tokens` / `cache_read_input_tokens` 字段，
    /// 仅用于让 sub2api 等统计面板的缓存指标不为 0、成本曲线接近真实 Anthropic。
    /// 它**不会**真的节省 token、额度或耗时。只认客户端真正打的 `cache_control`
    /// 断点；关闭时行为与原先完全一致。
    #[serde(default)]
    pub simulate_cache: bool,

    /// 模拟缓存的读取折扣系数（0.0~1.0，默认 0.3）
    ///
    /// 仅在 `simulate_cache=true` 时生效。Kiro 后端不支持 prompt 缓存，本代理
    /// 伪造的 `cache_read_input_tokens` 往往把整段稳定的大 system / 历史消息全部
    /// 算成缓存读取，而缓存读取按约 1/10 价计费，导致面板上每条大请求的扣费极低。
    ///
    /// 本系数把伪造出的 cache_read 按 `factor` 衰减：只保留 `read × factor`，差额
    /// 回退计入 `cache_creation`（按约 1.25 倍计费），从而抬高面板计费。值越小，
    /// 缓存读取越少、计费越高；`1.0` 表示不衰减（与历史行为一致）。
    /// 恒等式 `input + cache_creation + cache_read == 总输入` 始终成立。
    #[serde(default = "default_simulate_cache_read_factor")]
    pub simulate_cache_read_factor: f64,

    /// 默认端点名称（凭据未显式指定 endpoint 时使用，默认 "ide"）
    #[serde(default = "default_endpoint")]
    pub default_endpoint: String,

    /// 端点特定的配置
    ///
    /// 键为端点名（如 "ide" / "cli"），值为该端点自由定义的参数对象。
    /// 未在此表出现的端点沿用实现内置默认值。
    #[serde(default)]
    pub endpoints: HashMap<String, serde_json::Value>,

    /// 中转接口配置（可选）
    ///
    /// 启用后聊天请求优先走中转接口，失败回退真实 Kiro。
    #[serde(default)]
    pub relay: RelayConfig,

    /// 配置文件路径（运行时元数据，不写入 JSON）
    #[serde(skip)]
    config_path: Option<PathBuf>,
}

fn default_host() -> String {
    "127.0.0.1".to_string()
}

fn default_port() -> u16 {
    8080
}

fn default_region() -> String {
    "us-east-1".to_string()
}

fn default_kiro_version() -> String {
    "0.11.107".to_string()
}

fn default_system_version() -> String {
    const SYSTEM_VERSIONS: &[&str] = &["darwin#24.6.0", "win32#10.0.22631"];
    SYSTEM_VERSIONS[fastrand::usize(..SYSTEM_VERSIONS.len())].to_string()
}

fn default_node_version() -> String {
    "22.22.0".to_string()
}

fn default_count_tokens_auth_type() -> String {
    "x-api-key".to_string()
}

fn default_tls_backend() -> TlsBackend {
    TlsBackend::Rustls
}

fn default_load_balancing_mode() -> String {
    "priority".to_string()
}

fn default_extract_thinking() -> bool {
    true
}

fn default_simulate_cache_read_factor() -> f64 {
    0.3
}

fn default_endpoint() -> String {
    crate::kiro::endpoint::ide::IDE_ENDPOINT_NAME.to_string()
}

impl Default for Config {
    fn default() -> Self {
        Self {
            host: default_host(),
            port: default_port(),
            region: default_region(),
            auth_region: None,
            api_region: None,
            kiro_version: default_kiro_version(),
            machine_id: None,
            api_key: None,
            system_version: default_system_version(),
            node_version: default_node_version(),
            tls_backend: default_tls_backend(),
            count_tokens_api_url: None,
            count_tokens_api_key: None,
            count_tokens_auth_type: default_count_tokens_auth_type(),
            proxy_url: None,
            proxy_username: None,
            proxy_password: None,
            admin_api_key: None,
            load_balancing_mode: default_load_balancing_mode(),
            extract_thinking: default_extract_thinking(),
            simulate_cache: false,
            simulate_cache_read_factor: default_simulate_cache_read_factor(),
            default_endpoint: default_endpoint(),
            endpoints: HashMap::new(),
            relay: RelayConfig::default(),
            config_path: None,
        }
    }
}

impl Config {
    /// 获取默认配置文件路径
    pub fn default_config_path() -> &'static str {
        "config.json"
    }

    /// 获取有效的 Auth Region（用于 Token 刷新）
    /// 优先使用 auth_region，未配置时回退到 region
    pub fn effective_auth_region(&self) -> &str {
        self.auth_region.as_deref().unwrap_or(&self.region)
    }

    /// 获取有效的 API Region（用于 API 请求）
    /// 优先使用 api_region，未配置时回退到 region
    pub fn effective_api_region(&self) -> &str {
        self.api_region.as_deref().unwrap_or(&self.region)
    }

    /// 从文件加载配置
    pub fn load<P: AsRef<Path>>(path: P) -> anyhow::Result<Self> {
        let path = path.as_ref();
        if !path.exists() {
            // 配置文件不存在，返回默认配置
            let mut config = Self::default();
            config.config_path = Some(path.to_path_buf());
            return Ok(config);
        }

        let content = fs::read_to_string(path)?;
        let mut config: Config = serde_json::from_str(&content)?;
        config.config_path = Some(path.to_path_buf());
        Ok(config)
    }

    /// 获取配置文件路径（如果有）
    pub fn config_path(&self) -> Option<&Path> {
        self.config_path.as_deref()
    }

    /// 将当前配置写回原始配置文件
    pub fn save(&self) -> anyhow::Result<()> {
        let path = self
            .config_path
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("配置文件路径未知，无法保存配置"))?;

        let content = serde_json::to_string_pretty(self).context("序列化配置失败")?;
        fs::write(path, content).with_context(|| format!("写入配置文件失败: {}", path.display()))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_relay_config_is_active() {
        // 全部齐备 → 生效
        let cfg = RelayConfig {
            enabled: true,
            url: Some("http://example.com/sendMessage".to_string()),
            mcp_url: None,
            api_key: Some("sk-test".to_string()),
        };
        assert!(cfg.is_active());
    }

    #[test]
    fn test_relay_config_disabled() {
        let cfg = RelayConfig {
            enabled: false,
            url: Some("http://example.com/sendMessage".to_string()),
            mcp_url: Some("http://example.com/mcp".to_string()),
            api_key: Some("sk-test".to_string()),
        };
        assert!(!cfg.is_active());
        assert!(!cfg.is_mcp_active());
    }

    #[test]
    fn test_relay_config_missing_url_or_key() {
        // 缺 url
        let cfg = RelayConfig {
            enabled: true,
            url: None,
            mcp_url: None,
            api_key: Some("sk-test".to_string()),
        };
        assert!(!cfg.is_active());

        // 缺 api_key
        let cfg = RelayConfig {
            enabled: true,
            url: Some("http://example.com/sendMessage".to_string()),
            mcp_url: None,
            api_key: None,
        };
        assert!(!cfg.is_active());

        // 空白字符串视为未配置
        let cfg = RelayConfig {
            enabled: true,
            url: Some("   ".to_string()),
            mcp_url: None,
            api_key: Some("sk-test".to_string()),
        };
        assert!(!cfg.is_active());
    }

    #[test]
    fn test_relay_config_mcp_active() {
        // 同时配置 url 和 mcp_url：两者都生效
        let cfg = RelayConfig {
            enabled: true,
            url: Some("http://example.com/sendMessage".to_string()),
            mcp_url: Some("http://example.com/mcp".to_string()),
            api_key: Some("sk-test".to_string()),
        };
        assert!(cfg.is_active());
        assert!(cfg.is_mcp_active());
    }

    #[test]
    fn test_relay_config_mcp_independent_of_chat() {
        // 仅配置 mcp_url（无 url）：MCP 生效，聊天不生效
        let cfg = RelayConfig {
            enabled: true,
            url: None,
            mcp_url: Some("http://example.com/mcp".to_string()),
            api_key: Some("sk-test".to_string()),
        };
        assert!(!cfg.is_active());
        assert!(cfg.is_mcp_active());

        // 仅配置 url（无 mcp_url）：聊天生效，MCP 不生效（向后兼容）
        let cfg = RelayConfig {
            enabled: true,
            url: Some("http://example.com/sendMessage".to_string()),
            mcp_url: None,
            api_key: Some("sk-test".to_string()),
        };
        assert!(cfg.is_active());
        assert!(!cfg.is_mcp_active());
    }

    #[test]
    fn test_relay_config_default_is_inactive() {
        assert!(!RelayConfig::default().is_active());
        assert!(!RelayConfig::default().is_mcp_active());
    }

    #[test]
    fn test_config_default_relay_inactive() {
        // Config 默认不应启用中转
        assert!(!Config::default().relay.is_active());
        assert!(!Config::default().relay.is_mcp_active());
    }
}
