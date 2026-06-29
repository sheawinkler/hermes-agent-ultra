pub mod huawei;
pub mod oppo;
pub mod vivo;
pub mod xiaomi;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CnPushVendor {
    Xiaomi,
    Huawei,
    Vivo,
    Oppo,
}

pub async fn send_cn(
    vendor: CnPushVendor,
    token: &str,
    title: &str,
    body: &str,
) -> Result<(), String> {
    match vendor {
        CnPushVendor::Xiaomi => xiaomi::send(token, title, body).await,
        CnPushVendor::Huawei => huawei::send(token, title, body).await,
        CnPushVendor::Vivo => vivo::send(token, title, body).await,
        CnPushVendor::Oppo => oppo::send(token, title, body).await,
    }
}
