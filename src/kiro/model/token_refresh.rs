use serde::{Deserialize, Serialize};

/// 刷新 Token 的请求体 (Social 认证)
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RefreshRequest {
    pub refresh_token: String,
}

/// 刷新 Token 的响应体 (Social 认证)
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RefreshResponse {
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: Option<String>,
    #[serde(default)]
    pub profile_arn: Option<String>,
    #[serde(default)]
    pub expires_in: Option<i64>,
}

/// IdC Token 刷新请求体 (AWS SSO OIDC)
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IdcRefreshRequest {
    pub client_id: String,
    pub client_secret: String,
    pub refresh_token: String,
    pub grant_type: String,
}

/// IdC Token 刷新响应体 (AWS SSO OIDC)
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IdcRefreshResponse {
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: Option<String>,
    // #[serde(default)]
    // pub token_type: Option<String>,
    #[serde(default)]
    pub expires_in: Option<i64>,
    #[serde(default)]
    pub profile_arn: Option<String>,
}

/// 外部 IdP（external_idp / Microsoft Entra ID / Azure AD）Token 刷新响应体
///
/// Microsoft OAuth 2.0 v2.0 端点返回 **snake_case**，与 Kiro / AWS 的 camelCase 不同，
/// 因此需要独立结构（不能复用 [`IdcRefreshResponse`]）。
/// 若响应未携带新的 `refresh_token`，调用方需保留旧 refreshToken。
#[derive(Debug, Deserialize)]
pub struct ExternalIdpRefreshResponse {
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: Option<String>,
    #[serde(default)]
    pub expires_in: Option<i64>,
    // 错误字段仅在 HTTP 2xx 但语义失败的边缘情况下出现；正常错误走非 2xx 分支处理。
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub error_description: Option<String>,
}
