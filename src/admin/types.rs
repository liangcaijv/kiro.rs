//! Admin API 类型定义

use serde::{Deserialize, Serialize};

// ============ 凭据状态 ============

/// 所有凭据状态响应
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CredentialsStatusResponse {
    /// 凭据总数
    pub total: usize,
    /// 可用凭据数量（未禁用）
    pub available: usize,
    /// 当前活跃凭据 ID
    pub current_id: u64,
    /// 各凭据状态列表
    pub credentials: Vec<CredentialStatusItem>,
}

/// 单个凭据的状态信息
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CredentialStatusItem {
    /// 凭据唯一 ID
    pub id: u64,
    /// 优先级（数字越小优先级越高）
    pub priority: u32,
    /// 是否被禁用
    pub disabled: bool,
    /// 连续失败次数
    pub failure_count: u32,
    /// 是否为当前活跃凭据
    pub is_current: bool,
    /// Token 过期时间（RFC3339 格式）
    pub expires_at: Option<String>,
    /// 认证方式
    pub auth_method: Option<String>,
    /// 是否有 Profile ARN
    pub has_profile_arn: bool,
    /// refreshToken 的 SHA-256 哈希（仅 OAuth 凭据，用于前端去重）
    pub refresh_token_hash: Option<String>,
    /// kiroApiKey 的 SHA-256 哈希（仅 API Key 凭据，用于前端去重）
    pub api_key_hash: Option<String>,
    /// kiroApiKey 的脱敏展示（仅 API Key 凭据，用于前端显示）
    pub masked_api_key: Option<String>,
    /// 用户邮箱（用于前端显示）
    pub email: Option<String>,
    /// API 调用成功次数
    pub success_count: u64,
    /// 最后一次 API 调用时间（RFC3339 格式）
    pub last_used_at: Option<String>,
    /// 是否配置了凭据级代理
    pub has_proxy: bool,
    /// 代理 URL（用于前端展示）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proxy_url: Option<String>,
    /// Token 刷新连续失败次数
    pub refresh_failure_count: u32,
    /// 禁用原因
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disabled_reason: Option<String>,
    /// 端点名称（决定该凭据走哪套 Kiro API，已回退到默认端点）
    pub endpoint: String,
    /// 账号级中转开关（None=跟随全局，true=走中转，false=直连）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub use_relay: Option<bool>,
}

// ============ 操作请求 ============

/// 启用/禁用凭据请求
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetDisabledRequest {
    /// 是否禁用
    pub disabled: bool,
}

/// 修改优先级请求
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetPriorityRequest {
    /// 新优先级值
    pub priority: u32,
}

/// 添加凭据请求
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AddCredentialRequest {
    /// 刷新令牌（OAuth 凭据必填，API Key 凭据不需要）
    pub refresh_token: Option<String>,

    /// 认证方式（可选，默认 social）。支持 social / idc / api_key / external_idp
    #[serde(default = "default_auth_method")]
    pub auth_method: String,

    /// OIDC Client ID（IdC / external_idp 认证需要）
    pub client_id: Option<String>,

    /// OIDC Client Secret（IdC 认证需要；external_idp 不需要）
    pub client_secret: Option<String>,

    /// 外部 IdP Token 端点（external_idp / Microsoft Entra 刷新需要）
    #[serde(default, alias = "token_endpoint")]
    pub token_endpoint: Option<String>,

    /// 外部 IdP Issuer URL（external_idp，可选，仅记录）
    #[serde(default, alias = "issuer_url")]
    pub issuer_url: Option<String>,

    /// 外部 IdP 刷新 scope（external_idp 需要）
    #[serde(default)]
    pub scopes: Option<String>,

    /// 优先级（可选，默认 0）
    #[serde(default)]
    pub priority: u32,

    /// 凭据级 Region 配置（用于 OIDC token 刷新）
    /// 未配置时回退到 config.json 的全局 region
    pub region: Option<String>,

    /// 凭据级 Auth Region（用于 Token 刷新）
    pub auth_region: Option<String>,

    /// 凭据级 API Region（用于 API 请求）
    pub api_region: Option<String>,

    /// 凭据级 Machine ID（可选，64 位字符串）
    /// 未配置时回退到 config.json 的 machineId
    pub machine_id: Option<String>,

    /// 用户邮箱（可选，用于前端显示）
    pub email: Option<String>,

    /// 凭据级代理 URL（可选，特殊值 "direct" 表示不使用代理）
    pub proxy_url: Option<String>,

    /// 凭据级代理认证用户名（可选）
    pub proxy_username: Option<String>,

    /// 凭据级代理认证密码（可选）
    pub proxy_password: Option<String>,

    /// Kiro API Key（API Key 凭据必填，格式: ksk_xxxxxxxx）
    /// 设置后直接作为 Bearer Token 使用，无需 refreshToken
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kiro_api_key: Option<String>,

    /// 端点名称（可选，未配置时使用 config.defaultEndpoint）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
}

fn default_auth_method() -> String {
    "social".to_string()
}

// ============ 编辑凭据 ============

/// 编辑凭据请求（PATCH 语义）
///
/// 每个字段的解释：
/// - 缺省 / `null` → 保持原值不变
/// - `""`（空字符串）→ 清除该字段（回退到全局/默认）
/// - 非空值 → 设置为该值（自动 trim）
///
/// 仅覆盖运行时可安全修改的字段：代理、region、endpoint、email。
/// 不含 token / 认证核心字段（如需修改请删除后重新添加）。
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateCredentialRequest {
    /// 用户邮箱（显示名）
    #[serde(default)]
    pub email: Option<String>,

    /// 端点名称（须为已注册端点；空字符串回退默认端点）
    #[serde(default)]
    pub endpoint: Option<String>,

    /// 凭据级 Region
    #[serde(default)]
    pub region: Option<String>,

    /// 凭据级 Auth Region（用于 Token 刷新）
    #[serde(default)]
    pub auth_region: Option<String>,

    /// 凭据级 API Region（用于 API 请求）
    #[serde(default)]
    pub api_region: Option<String>,

    /// 凭据级代理 URL（支持 http/https/socks5，特殊值 "direct"）
    #[serde(default)]
    pub proxy_url: Option<String>,

    /// 凭据级代理认证用户名
    #[serde(default)]
    pub proxy_username: Option<String>,

    /// 凭据级代理认证密码
    #[serde(default)]
    pub proxy_password: Option<String>,

    /// 账号级中转开关（三态字符串）：
    /// - 缺省 / `null` → 保持原值
    /// - `"follow"` / `""` → 跟随全局（清除账号级覆盖）
    /// - `"on"` / `"true"` → 走中转
    /// - `"off"` / `"false"` → 直连
    #[serde(default)]
    pub use_relay: Option<String>,
}

/// 凭据可编辑字段详情（用于编辑表单预填，含代理机密回显）
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CredentialDetailResponse {
    /// 凭据唯一 ID
    pub id: u64,
    /// 认证方式（用于前端展示，不可编辑）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auth_method: Option<String>,
    /// 用户邮箱
    #[serde(skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    /// 端点名称
    #[serde(skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
    /// 凭据级 Region
    #[serde(skip_serializing_if = "Option::is_none")]
    pub region: Option<String>,
    /// 凭据级 Auth Region
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auth_region: Option<String>,
    /// 凭据级 API Region
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_region: Option<String>,
    /// 凭据级代理 URL
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proxy_url: Option<String>,
    /// 凭据级代理认证用户名
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proxy_username: Option<String>,
    /// 凭据级代理认证密码（明文回显，仅编辑预填用）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proxy_password: Option<String>,
    /// 账号级中转开关（None=跟随全局，true=走中转，false=直连）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub use_relay: Option<bool>,
}

/// 添加凭据成功响应
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AddCredentialResponse {
    pub success: bool,
    pub message: String,
    /// 新添加的凭据 ID
    pub credential_id: u64,
    /// 用户邮箱（如果获取成功）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
}

// ============ 余额查询 ============

/// 余额查询响应
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BalanceResponse {
    /// 凭据 ID
    pub id: u64,
    /// 订阅类型
    pub subscription_title: Option<String>,
    /// 当前使用量
    pub current_usage: f64,
    /// 使用限额
    pub usage_limit: f64,
    /// 剩余额度
    pub remaining: f64,
    /// 使用百分比
    pub usage_percentage: f64,
    /// 下次重置时间（Unix 时间戳）
    pub next_reset_at: Option<f64>,
}

// ============ 负载均衡配置 ============

/// 负载均衡模式响应
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LoadBalancingModeResponse {
    /// 当前模式（"priority" 或 "balanced"）
    pub mode: String,
}

/// 设置负载均衡模式请求
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetLoadBalancingModeRequest {
    /// 模式（"priority" 或 "balanced"）
    pub mode: String,
}

// ============ 模拟缓存配置 ============

/// 设置模拟缓存请求（省略的字段保持当前值不变）
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetSimulateCacheRequest {
    /// 是否启用模拟缓存
    pub enabled: Option<bool>,
    /// 缓存读取占比（0~1）
    pub read_ratio: Option<f64>,
    /// 缓存写入占比（0~1，read+write ≤ 1）
    pub write_ratio: Option<f64>,
}

// ============ 模型映射配置 ============

/// 模型映射列表响应
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelMappingsResponse {
    /// 当前映射规则（顺序即匹配优先级与 /v1/models 展示顺序）
    pub mappings: Vec<crate::model::model_mapping::ModelMapping>,
}

/// 设置模型映射请求（整表替换）
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetModelMappingsRequest {
    /// 新的完整映射规则列表
    pub mappings: Vec<crate::model::model_mapping::ModelMapping>,
}

// ============ 通用响应 ============

/// 操作成功响应
#[derive(Debug, Serialize)]
pub struct SuccessResponse {
    pub success: bool,
    pub message: String,
}

impl SuccessResponse {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            success: true,
            message: message.into(),
        }
    }
}

/// 错误响应
#[derive(Debug, Serialize)]
pub struct AdminErrorResponse {
    pub error: AdminError,
}

#[derive(Debug, Serialize)]
pub struct AdminError {
    #[serde(rename = "type")]
    pub error_type: String,
    pub message: String,
}

impl AdminErrorResponse {
    pub fn new(error_type: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            error: AdminError {
                error_type: error_type.into(),
                message: message.into(),
            },
        }
    }

    pub fn invalid_request(message: impl Into<String>) -> Self {
        Self::new("invalid_request", message)
    }

    pub fn authentication_error() -> Self {
        Self::new("authentication_error", "Invalid or missing admin API key")
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new("not_found", message)
    }

    pub fn api_error(message: impl Into<String>) -> Self {
        Self::new("api_error", message)
    }

    pub fn internal_error(message: impl Into<String>) -> Self {
        Self::new("internal_error", message)
    }
}
