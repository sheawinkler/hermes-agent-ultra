//! Flowy API request/response types.

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize)]
pub struct SendEmailCodeRequest {
    pub email: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub channel: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub app: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LoginByEmailRequest {
    pub email: String,
    pub valid_code: String,
    pub valid_code_req_no: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub invite_code: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub channel: String,
    pub device: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub app: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WeChatMpSessionRequest {
    #[serde(skip_serializing_if = "String::is_empty")]
    pub channel: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub invite_code: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WeChatMpSessionResponse {
    pub session_id: String,
    pub qr_image_url: String,
    #[serde(default)]
    pub expires_in: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct WeChatMpPollData {
    pub status: String,
    #[serde(default)]
    pub token: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct UserMe {
    pub id: i64,
    #[serde(default)]
    pub open_id: Option<String>,
    #[serde(default)]
    pub union_id: Option<String>,
    #[serde(default)]
    pub nickname: Option<String>,
    #[serde(default)]
    pub avatar: Option<String>,
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub phone: Option<String>,
    #[serde(default)]
    pub channel: Option<String>,
    #[serde(default)]
    pub status: Option<i32>,
    #[serde(default)]
    pub app_flowymes: Option<i32>,
    #[serde(default)]
    pub current_plan: Option<Value>,
}

impl UserMe {
    pub fn display_name(&self) -> String {
        self.nickname
            .clone()
            .filter(|s| !s.trim().is_empty())
            .or_else(|| self.email.clone())
            .unwrap_or_else(|| format!("user#{}", self.id))
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreditsBalance {
    pub balance: i64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreditsUsageByType {
    #[serde(default)]
    pub server_time: Option<String>,
    #[serde(default)]
    pub include_team_seat: Option<bool>,
    #[serde(default)]
    pub list: Vec<CreditsUsageTypeItem>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreditsUsageTypeItem {
    #[serde(rename = "type")]
    pub usage_type: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub total: i64,
    #[serde(default)]
    pub used: i64,
    #[serde(default)]
    pub remaining: i64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreditsCheckinRequest {
    pub time_zone: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreditsCheckinResponse {
    pub already_checked_in: bool,
    #[serde(default)]
    pub granted_points: i64,
    #[serde(default)]
    pub balance: i64,
    #[serde(default)]
    pub check_in_at: Option<String>,
    #[serde(default)]
    pub day_key: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientPackageRequest {
    pub package_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub app_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub platform: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PresenceHeartbeatRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub platform: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub app_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct BindEmailCodeRequest {
    pub email: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BindEmailRequest {
    pub email: String,
    pub valid_code: String,
    pub valid_code_req_no: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceActivateRequest {
    #[serde(skip_serializing_if = "String::is_empty")]
    pub channel: String,
    pub mac: String,
    pub sn: String,
    pub activate_timestamp: i64,
    pub cpu_chip_id: String,
    pub app_version: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub os_version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub xpu_brand: Option<String>,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub public_ip: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub country_code: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub postal: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub latitude: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub longitude: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub isp: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub timezone: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub currency: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AvailableModelsClaw {
    #[serde(default)]
    pub cloud: Vec<ClawModelEntry>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ClawModelEntry {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub extra: String,
    #[serde(default)]
    pub endpoint: String,
    #[serde(default)]
    pub anthropic_endpoint: String,
    #[serde(default)]
    pub icon: String,
    #[serde(default)]
    pub category: i32,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatSessionReportRequest {
    pub session_id: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ChatSessionReportResponse {
    pub stored: bool,
}
