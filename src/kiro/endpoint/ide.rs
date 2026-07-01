//! Kiro IDE 端点
//!
//! 对应 Kiro IDE 客户端目前使用的 AWS CodeWhisperer 端点：
//! - API: `https://q.{api_region}.amazonaws.com/generateAssistantResponse`
//! - MCP: `https://q.{api_region}.amazonaws.com/mcp`
//!
//! 请求头使用 aws-sdk-js User-Agent 标识。请求体会在根对象上注入 `profileArn`。

use reqwest::RequestBuilder;
use uuid::Uuid;

use super::{KiroEndpoint, RequestContext};

/// Kiro IDE 端点名称
pub const IDE_ENDPOINT_NAME: &str = "ide";

/// Kiro IDE 端点
pub struct IdeEndpoint;

impl IdeEndpoint {
    pub fn new() -> Self {
        Self
    }

    fn api_region<'a>(&self, ctx: &'a RequestContext<'_>) -> &'a str {
        ctx.credentials.effective_api_region(ctx.config)
    }

    fn host(&self, ctx: &RequestContext<'_>) -> String {
        format!("q.{}.amazonaws.com", self.api_region(ctx))
    }

    fn x_amz_user_agent(&self, ctx: &RequestContext<'_>) -> String {
        format!(
            "aws-sdk-js/1.0.34 KiroIDE-{}-{}",
            ctx.config.kiro_version, ctx.machine_id
        )
    }

    fn user_agent(&self, ctx: &RequestContext<'_>) -> String {
        format!(
            "aws-sdk-js/1.0.34 ua/2.1 os/{} lang/js md/nodejs#{} api/codewhispererstreaming#1.0.34 m/E KiroIDE-{}-{}",
            ctx.config.system_version,
            ctx.config.node_version,
            ctx.config.kiro_version,
            ctx.machine_id
        )
    }
}

impl Default for IdeEndpoint {
    fn default() -> Self {
        Self::new()
    }
}

impl KiroEndpoint for IdeEndpoint {
    fn name(&self) -> &'static str {
        IDE_ENDPOINT_NAME
    }

    fn api_url(&self, ctx: &RequestContext<'_>) -> String {
        format!(
            "https://q.{}.amazonaws.com/generateAssistantResponse",
            self.api_region(ctx)
        )
    }

    fn mcp_url(&self, ctx: &RequestContext<'_>) -> String {
        format!("https://q.{}.amazonaws.com/mcp", self.api_region(ctx))
    }

    fn decorate_api(&self, req: RequestBuilder, ctx: &RequestContext<'_>) -> RequestBuilder {
        let mut req = req
            .header("x-amzn-codewhisperer-optout", "true")
            .header("x-amzn-kiro-agent-mode", "vibe")
            .header("x-amz-user-agent", self.x_amz_user_agent(ctx))
            .header("user-agent", self.user_agent(ctx))
            .header("host", self.host(ctx))
            .header("amz-sdk-invocation-id", Uuid::new_v4().to_string())
            .header("amz-sdk-request", "attempt=1; max=3")
            .header("Authorization", format!("Bearer {}", ctx.token));

        // external_idp → tokentype: EXTERNAL_IDP；api_key → tokentype: API_KEY；其余不发送
        if let Some(token_type) = ctx.credentials.token_type_header() {
            req = req.header("tokentype", token_type);
        }
        req
    }

    fn decorate_mcp(&self, req: RequestBuilder, ctx: &RequestContext<'_>) -> RequestBuilder {
        let mut req = req
            .header("x-amz-user-agent", self.x_amz_user_agent(ctx))
            .header("user-agent", self.user_agent(ctx))
            .header("host", self.host(ctx))
            .header("amz-sdk-invocation-id", Uuid::new_v4().to_string())
            .header("amz-sdk-request", "attempt=1; max=3")
            .header("Authorization", format!("Bearer {}", ctx.token));

        if let Some(ref arn) = ctx.credentials.profile_arn {
            req = req.header("x-amzn-kiro-profile-arn", arn);
        }
        // external_idp → tokentype: EXTERNAL_IDP；api_key → tokentype: API_KEY；其余不发送
        if let Some(token_type) = ctx.credentials.token_type_header() {
            req = req.header("tokentype", token_type);
        }
        req
    }

    fn transform_api_body(&self, body: &str, ctx: &RequestContext<'_>) -> String {
        inject_profile_arn(body, &ctx.credentials.profile_arn)
    }
}

/// 将 profile_arn 注入到请求体 JSON 根对象
fn inject_profile_arn(request_body: &str, profile_arn: &Option<String>) -> String {
    if let Some(arn) = profile_arn {
        if let Ok(mut json) = serde_json::from_str::<serde_json::Value>(request_body) {
            json["profileArn"] = serde_json::Value::String(arn.clone());
            if let Ok(body) = serde_json::to_string(&json) {
                return body;
            }
        }
    }
    request_body.to_string()
}

#[cfg(test)]
mod tests {
    use super::inject_profile_arn;
    use super::{IdeEndpoint, KiroEndpoint, RequestContext};
    use crate::kiro::model::credentials::KiroCredentials;
    use crate::model::config::Config;
    use serde_json::Value;

    fn tokentype_header_for(creds: &KiroCredentials) -> Option<String> {
        let endpoint = IdeEndpoint::new();
        let config = Config::default();
        let ctx = RequestContext {
            credentials: creds,
            token: "tok",
            machine_id: "m",
            config: &config,
        };
        let base = reqwest::Client::new().post("https://example.invalid/");
        let req = endpoint.decorate_api(base, &ctx).build().unwrap();
        req.headers()
            .get("tokentype")
            .map(|v| v.to_str().unwrap().to_string())
    }

    #[test]
    fn test_decorate_api_external_idp_sets_tokentype() {
        let mut creds = KiroCredentials::default();
        creds.auth_method = Some("external_idp".to_string());
        assert_eq!(
            tokentype_header_for(&creds),
            Some("EXTERNAL_IDP".to_string())
        );
    }

    #[test]
    fn test_decorate_api_api_key_sets_tokentype() {
        let mut creds = KiroCredentials::default();
        creds.kiro_api_key = Some("ksk_x".to_string());
        assert_eq!(tokentype_header_for(&creds), Some("API_KEY".to_string()));
    }

    #[test]
    fn test_decorate_api_social_omits_tokentype() {
        let mut creds = KiroCredentials::default();
        creds.auth_method = Some("social".to_string());
        assert_eq!(tokentype_header_for(&creds), None);
    }

    #[test]
    fn test_inject_profile_arn_with_some() {
        let body = r#"{"conversationState":{"conversationId":"c1"}}"#;
        let arn = Some("arn:aws:codewhisperer:us-east-1:123:profile/ABC".to_string());
        let result = inject_profile_arn(body, &arn);
        let json: Value = serde_json::from_str(&result).unwrap();
        assert_eq!(
            json["profileArn"],
            "arn:aws:codewhisperer:us-east-1:123:profile/ABC"
        );
        assert_eq!(json["conversationState"]["conversationId"], "c1");
    }

    #[test]
    fn test_inject_profile_arn_with_none() {
        let body = r#"{"conversationState":{"conversationId":"c1"}}"#;
        let result = inject_profile_arn(body, &None);
        let json: Value = serde_json::from_str(&result).unwrap();
        assert!(json.get("profileArn").is_none());
        assert_eq!(json["conversationState"]["conversationId"], "c1");
    }

    #[test]
    fn test_inject_profile_arn_overwrites_existing() {
        let body = r#"{"conversationState":{},"profileArn":"old-arn"}"#;
        let arn = Some("new-arn".to_string());
        let result = inject_profile_arn(body, &arn);
        let json: Value = serde_json::from_str(&result).unwrap();
        assert_eq!(json["profileArn"], "new-arn");
    }

    #[test]
    fn test_inject_profile_arn_invalid_json() {
        let body = "not-valid-json";
        let arn = Some("arn:test".to_string());
        let result = inject_profile_arn(body, &arn);
        assert_eq!(result, "not-valid-json");
    }
}
