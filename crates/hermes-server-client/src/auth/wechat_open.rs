//! WeChat Open Platform website QR login (WxLogin-compatible flow).
//!
//! Matches FlowyClaw `WeChatLogin.tsx` + `wxLogin.js`:
//! - QR is fetched from **WeChat** (`open.weixin.qq.com`), never from Flowy `/auth/wechat-mp/*`
//! - After scan, exchange OAuth `code` via Flowy `GET /auth/third/callback` (+ channel/app)

use std::io::Cursor;
use std::sync::LazyLock;
use std::time::Duration;

use image::ImageReader;
use regex::Regex;
use reqwest::Client;
use rqrr::PreparedImage;
use serde::{Deserialize, Serialize};
use tracing::debug;

use crate::error::ServerClientError;

const QRCONNECT_PAGE: &str = "https://open.weixin.qq.com/connect/qrconnect";
const QR_IMAGE_BASE: &str = "https://open.weixin.qq.com/connect/qrcode";
const QR_POLL_BASE: &str = "https://lp.open.weixin.qq.com/connect/l/qrconnect";

static UUID_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"fordevtool\s*=\s*"https://(?:long|lp)\.open\.weixin\.qq\.com/connect/l/qrconnect\?uuid=([0-9a-zA-Z]{10,32})""#)
        .expect("fordevtool uuid regex")
});
static G_UUID_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#",G="([0-9a-zA-Z]{10,32})""#).expect("G uuid regex"));
static APPID_ERROR_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)appid\s*参数错误|appid\s*不合法|invalid\s*appid")
        .expect("appid error regex")
});
static REDIRECT_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"wx_redirecturl\s*=\s*'([^']+)'"#).expect("redirect regex")
});
static WX_ERRCODE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"wx_err(?:or_)?code\s*=\s*(\d+)").expect("wx errcode regex")
});
static WX_CODE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"wx_code\s*=\s*'([^']+)'").expect("wx code regex"));

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WeChatPollResult {
    pub status: WeChatPollStatus,
    /// Pass as `last=` on the next long-poll (WeChat WxLogin passes 404 after scan).
    pub errcode: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WeChatOpenSession {
    pub state: String,
    pub uuid: String,
    /// Official WeChat PNG URL (`open.weixin.qq.com/connect/qrcode/{uuid}`).
    pub qr_image_url: String,
    /// Payload decoded from the official PNG — use this for terminal QR rendering.
    pub qr_scan_payload: String,
    pub redirect_uri: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WeChatPollStatus {
    Waiting,
    Scanned,
    Expired,
    Denied,
    Authorized { code: String },
}

/// `{wechatApiBase}/auth/third/callback?platform=WECHAT` — must match WeChat Open Platform config.
pub fn build_wechat_redirect_uri(wechat_api_base: &str) -> String {
    format!(
        "{}/auth/third/callback?platform=WECHAT",
        wechat_api_base.trim().trim_end_matches('/')
    )
}

/// WxLogin-compatible qrconnect page URL (`self_redirect=true`, `scope=snsapi_login`).
pub fn build_qrconnect_page_url(app_id: &str, redirect_uri: &str, state: &str) -> String {
    format!(
        "{QRCONNECT_PAGE}?appid={app_id}&redirect_uri={redirect}&response_type=code&scope=snsapi_login&state={state}&login_type=jssdk&self_redirect=true#wechat_redirect",
        app_id = app_id.trim(),
        redirect = url_encode(redirect_uri),
        state = url_encode(state),
    )
}

pub fn qr_image_url(uuid: &str) -> String {
    format!("{QR_IMAGE_BASE}/{uuid}")
}

pub fn random_wxlogin_state() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

pub async fn start_wechat_open_session(
    app_id: &str,
    redirect_uri: &str,
) -> Result<WeChatOpenSession, ServerClientError> {
    let state = random_wxlogin_state();
    let page_url = build_qrconnect_page_url(app_id, redirect_uri, &state);
    let client = wechat_http_client()?;
    let body = client
        .get(&page_url)
        .send()
        .await
        .map_err(|e| ServerClientError::Http(format!("wechat qrconnect page: {e}")))?
        .text()
        .await
        .map_err(|e| ServerClientError::Http(format!("wechat qrconnect body: {e}")))?;

    if let Some(msg) = detect_qrconnect_page_error(&body, app_id) {
        return Err(ServerClientError::InvalidResponse(msg));
    }

    let uuid = extract_uuid_from_qrconnect_page(&body).ok_or_else(|| {
        ServerClientError::InvalidResponse(format!(
            "wechat qrconnect page did not contain a valid uuid (appid={app_id}, redirect_uri={redirect_uri})"
        ))
    })?;

    let qr_image_url = qr_image_url(&uuid);
    let qr_scan_payload = fetch_wechat_qr_scan_payload(&uuid).await.map_err(|err| {
        ServerClientError::InvalidResponse(format!(
            "failed to load official WeChat QR image for uuid={uuid}: {err}"
        ))
    })?;

    debug!(%uuid, %app_id, %redirect_uri, "wechat open platform qr session started");
    Ok(WeChatOpenSession {
        qr_image_url,
        qr_scan_payload,
        uuid,
        state,
        redirect_uri: redirect_uri.to_string(),
    })
}

/// Download the official WxLogin QR PNG and decode its scan payload.
pub async fn fetch_wechat_qr_scan_payload(uuid: &str) -> Result<String, ServerClientError> {
    let url = qr_image_url(uuid);
    let client = wechat_http_client()?;
    let response = client
        .get(&url)
        .send()
        .await
        .map_err(|e| ServerClientError::Http(format!("wechat qrcode png: {e}")))?;
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let bytes = response
        .bytes()
        .await
        .map_err(|e| ServerClientError::Http(format!("wechat qrcode png body: {e}")))?;
    if !content_type.contains("image") {
        let preview = String::from_utf8_lossy(&bytes[..bytes.len().min(200)]);
        return Err(ServerClientError::InvalidResponse(format!(
            "wechat qrcode endpoint returned non-image (uuid={uuid}, content-type={content_type}): {preview}"
        )));
    }
    decode_qr_payload_from_png(&bytes)
}

pub async fn poll_wechat_open_session(
    uuid: &str,
    last_errcode: Option<u32>,
) -> Result<WeChatPollResult, ServerClientError> {
    let ts = chrono::Utc::now().timestamp_millis();
    let mut poll_url = format!("{QR_POLL_BASE}?uuid={uuid}&_={ts}");
    if let Some(last) = last_errcode {
        poll_url.push_str(&format!("&last={last}"));
    }
    let client = wechat_http_client()?;
    let body = client
        .get(&poll_url)
        .send()
        .await
        .map_err(|e| ServerClientError::Http(format!("wechat qr poll: {e}")))?
        .text()
        .await
        .map_err(|e| ServerClientError::Http(format!("wechat qr poll body: {e}")))?;

    Ok(parse_wechat_poll_response(&body))
}

/// Parse WxLogin long-poll JS (`window.wx_errcode=…`).
pub fn parse_wechat_poll_response(body: &str) -> WeChatPollResult {
    if let Some(code) = extract_oauth_code_from_poll_body(body) {
        return WeChatPollResult {
            status: WeChatPollStatus::Authorized { code },
            errcode: parse_wx_errcode(body),
        };
    }

    let errcode = parse_wx_errcode(body);
    let status = match errcode {
        Some(402) => WeChatPollStatus::Expired,
        Some(403) => WeChatPollStatus::Denied,
        Some(404) => WeChatPollStatus::Scanned,
        Some(405) => WeChatPollStatus::Waiting,
        Some(408) | None => WeChatPollStatus::Waiting,
        _ => WeChatPollStatus::Waiting,
    };
    WeChatPollResult { status, errcode }
}

fn parse_wx_errcode(body: &str) -> Option<u32> {
    WX_ERRCODE_RE
        .captures(body)
        .and_then(|c| c.get(1))
        .and_then(|m| m.as_str().parse().ok())
}

fn extract_oauth_code_from_poll_body(body: &str) -> Option<String> {
    if let Some(caps) = REDIRECT_RE.captures(body) {
        let redirect = caps.get(1).map(|m| m.as_str()).unwrap_or_default();
        if let Some(code) = extract_query_param(redirect, "code") {
            return Some(code);
        }
    }
    WX_CODE_RE
        .captures(body)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().to_string())
        .filter(|code| !code.is_empty())
}

fn decode_qr_payload_from_png(bytes: &[u8]) -> Result<String, ServerClientError> {
    let image = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| ServerClientError::InvalidResponse(format!("wechat qrcode format: {e}")))?
        .decode()
        .map_err(|e| ServerClientError::InvalidResponse(format!("wechat qrcode decode: {e}")))?
        .to_luma8();
    let mut prepared = PreparedImage::prepare(image);
    let grids = prepared.detect_grids();
    let grid = grids
        .iter()
        .next()
        .ok_or_else(|| ServerClientError::InvalidResponse("wechat qrcode: no QR grid".into()))?;
    let (_, content) = grid
        .decode()
        .map_err(|e| ServerClientError::InvalidResponse(format!("wechat qrcode grid: {e}")))?;
    Ok(content)
}

fn extract_uuid_from_qrconnect_page(body: &str) -> Option<String> {
    UUID_RE
        .captures(body)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().to_string())
        .or_else(|| {
            G_UUID_RE
                .captures(body)
                .and_then(|c| c.get(1))
                .map(|m| m.as_str().to_string())
        })
        .filter(|uuid| is_valid_wechat_qr_uuid(uuid))
}

fn is_valid_wechat_qr_uuid(value: &str) -> bool {
    let len = value.len();
    (10..=32).contains(&len) && value.bytes().all(|b| b.is_ascii_alphanumeric())
}

fn detect_qrconnect_page_error(body: &str, app_id: &str) -> Option<String> {
    if APPID_ERROR_RE.is_match(body) {
        return Some(format!(
            "WeChat rejected appid '{app_id}' — check server.auth.wechat_app_id and channel (flowy=wxc7a38fe55e162569, gmk=wx413de9863ef7ea1c)"
        ));
    }
    if body.contains("weui_msg_title") && body.len() < 4_000 && !body.contains("fordevtool") {
        return Some(format!(
            "WeChat qrconnect returned an error page for appid '{app_id}' (redirect_uri may also be unregistered)"
        ));
    }
    None
}

fn extract_query_param(url: &str, key: &str) -> Option<String> {
    let query = url.split('#').next()?.split('?').nth(1)?;
    for (k, v) in url::form_urlencoded::parse(query.as_bytes()) {
        if k == key && !v.is_empty() {
            return Some(v.into_owned());
        }
    }
    None
}

fn url_encode(value: &str) -> String {
    url::form_urlencoded::byte_serialize(value.as_bytes()).collect()
}

fn wechat_http_client() -> Result<Client, ServerClientError> {
    Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| ServerClientError::Http(format!("wechat http client: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_redirect_uri_matches_flowy_spec() {
        assert_eq!(
            build_wechat_redirect_uri("https://server.flowyaipc.cn/claw"),
            "https://server.flowyaipc.cn/claw/auth/third/callback?platform=WECHAT"
        );
    }

    #[test]
    fn qrconnect_url_matches_wxlogin_sdk() {
        let url = build_qrconnect_page_url(
            "wxc7a38fe55e162569",
            "https://server.flowyaipc.cn/claw/auth/third/callback?platform=WECHAT",
            "abc",
        );
        assert!(url.contains("appid=wxc7a38fe55e162569"));
        assert!(url.contains("scope=snsapi_login"));
        assert!(url.contains("self_redirect=true"));
        assert!(url.ends_with("#wechat_redirect"));
    }

    #[test]
    fn extract_uuid_from_fortool_line() {
        let html = r#"var fordevtool = "https://long.open.weixin.qq.com/connect/l/qrconnect?uuid=0817SWyE4nI7Ga1h""#;
        assert_eq!(
            extract_uuid_from_qrconnect_page(html).as_deref(),
            Some("0817SWyE4nI7Ga1h")
        );
    }

    #[test]
    fn extract_uuid_from_g_variable() {
        let html = r#",G="0311VhSr1wi51007",F=!1"#;
        assert_eq!(
            extract_uuid_from_qrconnect_page(html).as_deref(),
            Some("0311VhSr1wi51007")
        );
    }

    #[test]
    fn legacy_uuid_regex_does_not_match_js_noise() {
        let html = r#"uuid:"+G+(e?"#;
        assert!(extract_uuid_from_qrconnect_page(html).is_none());
    }

    #[test]
    fn detect_appid_error_page() {
        let html = r#"<h4 class="weui_msg_title">AppID 参数错误</h4>"#;
        assert!(detect_qrconnect_page_error(html, "flowymes").is_some());
    }

    #[test]
    fn parse_uuid_from_sample_html() {
        let html = r#"var fordevtool = "https://long.open.weixin.qq.com/connect/l/qrconnect?uuid=abc123xyz0""#;
        assert_eq!(
            extract_uuid_from_qrconnect_page(html).as_deref(),
            Some("abc123xyz0")
        );
    }

    #[test]
    fn poll_404_means_scanned_not_expired() {
        let result = parse_wechat_poll_response("window.wx_errcode=404;");
        assert_eq!(result.status, WeChatPollStatus::Scanned);
        assert_eq!(result.errcode, Some(404));
    }

    #[test]
    fn poll_402_means_expired() {
        let result = parse_wechat_poll_response("window.wx_errcode=402;");
        assert_eq!(result.status, WeChatPollStatus::Expired);
    }

    #[test]
    fn poll_408_means_waiting() {
        let result = parse_wechat_poll_response("window.wx_errcode=408;");
        assert_eq!(result.status, WeChatPollStatus::Waiting);
    }

    #[test]
    fn poll_405_with_wx_code_authorizes() {
        let result = parse_wechat_poll_response("window.wx_errcode=405;window.wx_code='ABC123';");
        assert_eq!(
            result.status,
            WeChatPollStatus::Authorized {
                code: "ABC123".into()
            }
        );
    }

    #[test]
    fn poll_redirect_with_code_authorizes() {
        let body = r"window.wx_errcode=405;window.wx_redirecturl='https://server.flowyaipc.cn/claw/auth/third/callback?platform=WECHAT&code=XYZ&state=abc';";
        let result = parse_wechat_poll_response(body);
        assert_eq!(
            result.status,
            WeChatPollStatus::Authorized {
                code: "XYZ".into()
            }
        );
    }

    #[test]
    fn extract_code_from_redirect() {
        let redirect = "https://server.flowyaipc.cn/claw/auth/third/callback?platform=WECHAT&code=ABC123&state=xyz";
        assert_eq!(
            extract_query_param(redirect, "code").as_deref(),
            Some("ABC123")
        );
    }
}
