//! 可用 Profile 查询数据模型
//!
//! 对应上游 `ListAvailableProfiles`（AWS JSON 1.0，target
//! `AmazonCodeWhispererService.ListAvailableProfiles`）的响应类型。
//!
//! 组织 SSO（external_idp / Enterprise / IAM Identity Center）账号需要真实的
//! `profileArn` 才能正常调用数据面：external_idp 刷新流程本身不返回 profileArn，
//! 也不应填 Builder ID 占位 ARN（否则 token 身份不匹配会被上游以 403 拒绝）。
//! 真实 profileArn 只能通过本接口懒解析获取并回填。

use serde::{Deserialize, Serialize};

/// `ListAvailableProfiles` 响应
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ListAvailableProfilesResponse {
    /// 该凭据可用的 profile 列表
    #[serde(default)]
    pub profiles: Vec<AvailableProfile>,

    /// 分页 token（本项目只取第一个 profile，通常无需翻页）
    #[serde(default)]
    pub next_token: Option<String>,
}

/// 单个可用 profile
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct AvailableProfile {
    /// Profile ARN（真实可用的 profileArn）
    #[serde(default)]
    pub arn: Option<String>,

    /// Profile 名称（如 `KiroProfile-us-east-1`）
    #[serde(default)]
    #[allow(dead_code)]
    pub profile_name: Option<String>,
}

impl ListAvailableProfilesResponse {
    /// 返回第一个非空的真实 profileArn（若有）。
    pub fn first_arn(&self) -> Option<&str> {
        self.profiles
            .iter()
            .filter_map(|p| p.arn.as_deref())
            .map(str::trim)
            .find(|arn| !arn.is_empty())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_profiles_and_first_arn() {
        let json = r#"{
            "profiles": [
                {
                    "arn": "arn:aws:codewhisperer:us-east-1:610548660232:profile/VNECVYCYYAWN",
                    "profileName": "KiroProfile-us-east-1"
                }
            ]
        }"#;
        let resp: ListAvailableProfilesResponse = serde_json::from_str(json).unwrap();
        assert_eq!(
            resp.first_arn(),
            Some("arn:aws:codewhisperer:us-east-1:610548660232:profile/VNECVYCYYAWN")
        );
    }

    #[test]
    fn test_first_arn_none_when_empty() {
        let resp: ListAvailableProfilesResponse =
            serde_json::from_str(r#"{"profiles":[]}"#).unwrap();
        assert_eq!(resp.first_arn(), None);
    }

    #[test]
    fn test_first_arn_skips_blank() {
        let json = r#"{"profiles":[{"arn":"  "},{"arn":"arn:real"}]}"#;
        let resp: ListAvailableProfilesResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.first_arn(), Some("arn:real"));
    }
}
