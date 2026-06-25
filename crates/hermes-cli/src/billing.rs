//! Rust-native Nous billing client and terminal surface.
//!
//! This ports the upstream terminal billing contract into the Rust runtime:
//! typed billing state parsing, decimal-string money handling, fail-open
//! overview rendering, scoped OAuth step-up, and explicit-confirmation write
//! operations for charge / auto-reload endpoints.

use crate::auth::{
    login_nous_device_code, read_nous_auth_state, NousAuthState, NousDeviceCodeOptions,
    DEFAULT_NOUS_SCOPE,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use hermes_core::AgentError;
use reqwest::{header, Method, StatusCode, Url};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::fmt;
use std::time::Duration;
use uuid::Uuid;

pub const DEFAULT_PORTAL_BASE_URL: &str = "https://portal.nousresearch.com";
pub const BILLING_MANAGE_SCOPE: &str = "billing:manage";
const DEFAULT_TIMEOUT_SECONDS: f64 = 15.0;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CardInfo {
    pub brand: String,
    pub last4: String,
}

impl CardInfo {
    pub fn masked(&self) -> String {
        format!("{} ....{}", self.brand, self.last4)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MonthlyCap {
    pub limit_usd: Option<String>,
    pub spent_this_month_usd: Option<String>,
    pub is_default_ceiling: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutoReload {
    pub enabled: bool,
    pub threshold_usd: Option<String>,
    pub reload_to_usd: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BillingState {
    pub logged_in: bool,
    pub org_id: Option<String>,
    pub org_slug: Option<String>,
    pub org_name: Option<String>,
    pub role: Option<String>,
    pub balance_usd: Option<String>,
    pub cli_billing_enabled: bool,
    pub charge_presets: Vec<String>,
    pub min_usd: Option<String>,
    pub max_usd: Option<String>,
    pub card: Option<CardInfo>,
    pub monthly_cap: Option<MonthlyCap>,
    pub auto_reload: Option<AutoReload>,
    pub portal_url: Option<String>,
    pub error: Option<String>,
}

impl BillingState {
    pub fn logged_out(portal_url: Option<String>) -> Self {
        Self {
            logged_in: false,
            org_id: None,
            org_slug: None,
            org_name: None,
            role: None,
            balance_usd: None,
            cli_billing_enabled: false,
            charge_presets: Vec::new(),
            min_usd: None,
            max_usd: None,
            card: None,
            monthly_cap: None,
            auto_reload: None,
            portal_url,
            error: None,
        }
    }

    pub fn fail_open(error: impl Into<String>, portal_url: Option<String>) -> Self {
        let mut state = Self::logged_out(portal_url);
        state.error = Some(error.into());
        state
    }

    pub fn is_admin(&self) -> bool {
        matches!(
            self.role.as_deref().map(str::trim).map(str::to_ascii_uppercase),
            Some(role) if role == "OWNER" || role == "ADMIN"
        )
    }

    pub fn can_charge(&self) -> bool {
        self.is_admin() && self.cli_billing_enabled
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BillingErrorKind {
    Auth,
    ScopeRequired,
    RateLimited,
    Network,
    InvalidRequest,
    Http,
}

#[derive(Debug, Clone)]
pub struct BillingError {
    pub kind: BillingErrorKind,
    pub status: Option<u16>,
    pub error: Option<String>,
    pub message: String,
    pub portal_url: Option<String>,
    pub retry_after: Option<u64>,
    pub payload: Option<Box<Value>>,
}

impl BillingError {
    fn auth(message: impl Into<String>) -> Self {
        Self {
            kind: BillingErrorKind::Auth,
            status: Some(401),
            error: Some("invalid_token".into()),
            message: message.into(),
            portal_url: None,
            retry_after: None,
            payload: None,
        }
    }

    fn invalid_request(message: impl Into<String>, error: impl Into<String>) -> Self {
        Self {
            kind: BillingErrorKind::InvalidRequest,
            status: None,
            error: Some(error.into()),
            message: message.into(),
            portal_url: None,
            retry_after: None,
            payload: None,
        }
    }

    fn from_response(
        status: StatusCode,
        payload: Value,
        retry_after: Option<u64>,
        portal_base_url: &str,
    ) -> Self {
        let error = payload
            .get("error")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned);
        let message = payload
            .get("message")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
            .or_else(|| error.clone())
            .unwrap_or_else(|| format!("Billing request failed ({status})."));
        let portal_url = payload
            .get("portalUrl")
            .and_then(Value::as_str)
            .and_then(|url| absolutize_portal_url_with_base(url, portal_base_url));
        let kind = if status == StatusCode::UNAUTHORIZED {
            BillingErrorKind::Auth
        } else if status == StatusCode::FORBIDDEN && error.as_deref() == Some("insufficient_scope")
        {
            BillingErrorKind::ScopeRequired
        } else if status == StatusCode::TOO_MANY_REQUESTS
            || status == StatusCode::SERVICE_UNAVAILABLE
        {
            BillingErrorKind::RateLimited
        } else {
            BillingErrorKind::Http
        };
        Self {
            kind,
            status: Some(status.as_u16()),
            error,
            message,
            portal_url,
            retry_after,
            payload: Some(Box::new(payload)),
        }
    }

    fn network(error: impl Into<String>) -> Self {
        Self {
            kind: BillingErrorKind::Network,
            status: None,
            error: Some("network_error".into()),
            message: error.into(),
            portal_url: None,
            retry_after: None,
            payload: None,
        }
    }
}

impl fmt::Display for BillingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(status) = self.status {
            write!(f, "{} ({status})", self.message)
        } else {
            write!(f, "{}", self.message)
        }
    }
}

impl std::error::Error for BillingError {}

pub struct NousBillingClient {
    access_token: String,
    portal_base_url: String,
    client: reqwest::Client,
}

impl NousBillingClient {
    pub fn from_auth_store() -> Result<Self, BillingError> {
        let state = read_nous_auth_state()
            .map_err(|e| BillingError::auth(e.to_string()))?
            .ok_or_else(|| BillingError::auth("Not logged into Nous Portal."))?;
        Self::from_state(&state)
    }

    pub fn from_state(state: &NousAuthState) -> Result<Self, BillingError> {
        let access_token = state.access_token.trim();
        if access_token.is_empty() {
            return Err(BillingError::auth("Not logged into Nous Portal."));
        }
        let portal_base_url = resolve_portal_base_url(Some(state));
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs_f64(DEFAULT_TIMEOUT_SECONDS))
            .user_agent(format!("hermes-agent-ultra/{}", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| BillingError::network(format!("build billing client: {e}")))?;
        Ok(Self {
            access_token: access_token.to_string(),
            portal_base_url,
            client,
        })
    }

    pub fn portal_base_url(&self) -> &str {
        &self.portal_base_url
    }

    pub async fn get_state(&self) -> Result<BillingState, BillingError> {
        let payload = self
            .request(Method::GET, "/api/billing/state", None, None)
            .await?;
        Ok(billing_state_from_payload(
            &payload,
            fallback_portal_url(&self.portal_base_url),
        ))
    }

    pub async fn post_charge(
        &self,
        amount_usd: &str,
        idempotency_key: &str,
    ) -> Result<Value, BillingError> {
        let key = idempotency_key.trim();
        if key.is_empty() {
            return Err(BillingError::invalid_request(
                "Idempotency-Key is required for a charge.",
                "idempotency_key_required",
            ));
        }
        let amount = parse_amount_number(amount_usd).ok_or_else(|| {
            BillingError::invalid_request("A valid charge amount is required.", "invalid_amount")
        })?;
        self.request(
            Method::POST,
            "/api/billing/charge",
            Some(json!({ "amountUsd": amount })),
            Some(("Idempotency-Key", key.to_string())),
        )
        .await
    }

    pub async fn get_charge_status(&self, charge_id: &str) -> Result<Value, BillingError> {
        let charge_id = charge_id.trim();
        if charge_id.is_empty() {
            return Err(BillingError::invalid_request(
                "A charge id is required.",
                "invalid_charge_id",
            ));
        }
        let encoded = urlencoding::encode(charge_id);
        self.request(
            Method::GET,
            &format!("/api/billing/charge/{encoded}"),
            None,
            None,
        )
        .await
    }

    pub async fn patch_auto_reload(
        &self,
        enabled: bool,
        threshold: &str,
        top_up_amount: &str,
    ) -> Result<Value, BillingError> {
        let threshold = parse_amount_number(threshold).ok_or_else(|| {
            BillingError::invalid_request("A valid threshold is required.", "invalid_threshold")
        })?;
        let top_up_amount = parse_amount_number(top_up_amount).ok_or_else(|| {
            BillingError::invalid_request(
                "A valid top-up amount is required.",
                "invalid_top_up_amount",
            )
        })?;
        self.request(
            Method::PATCH,
            "/api/billing/auto-top-up",
            Some(json!({
                "enabled": enabled,
                "threshold": threshold,
                "topUpAmount": top_up_amount,
            })),
            None,
        )
        .await
    }

    async fn request(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
        extra_header: Option<(&str, String)>,
    ) -> Result<Value, BillingError> {
        let url = format!("{}{}", self.portal_base_url.trim_end_matches('/'), path);
        let mut req = self
            .client
            .request(method, url)
            .bearer_auth(&self.access_token)
            .header(header::ACCEPT, "application/json");
        if let Some((name, value)) = extra_header {
            req = req.header(name, value);
        }
        if let Some(body) = body {
            req = req
                .header(header::CONTENT_TYPE, "application/json")
                .json(&body);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| BillingError::network(format!("Could not reach Nous Portal: {e}")))?;
        let status = resp.status();
        let retry_after = resp
            .headers()
            .get(header::RETRY_AFTER)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.trim().parse::<u64>().ok());
        let text = resp.text().await.unwrap_or_default();
        let payload = if text.trim().is_empty() {
            json!({})
        } else {
            serde_json::from_str::<Value>(&text).unwrap_or_else(|_| json!({}))
        };
        if !status.is_success() {
            return Err(BillingError::from_response(
                status,
                payload,
                retry_after,
                &self.portal_base_url,
            ));
        }
        Ok(payload)
    }
}

pub async fn build_billing_state() -> BillingState {
    match NousBillingClient::from_auth_store() {
        Ok(client) => match client.get_state().await {
            Ok(state) => state,
            Err(err) if err.kind == BillingErrorKind::Auth => {
                BillingState::logged_out(Some(fallback_portal_url(client.portal_base_url())))
            }
            Err(err) => BillingState::fail_open(
                err.to_string(),
                err.portal_url
                    .clone()
                    .or_else(|| Some(fallback_portal_url(client.portal_base_url()))),
            ),
        },
        Err(_) => {
            BillingState::logged_out(Some(fallback_portal_url(&resolve_portal_base_url(None))))
        }
    }
}

pub async fn handle_billing_args(args: &[String]) -> Result<String, AgentError> {
    let action = args.first().map(String::as_str).unwrap_or("overview");
    match action {
        "overview" | "status" | "state" => Ok(render_billing_state(&build_billing_state().await)),
        "portal" => Ok(format!(
            "Nous billing portal\n{}",
            fallback_portal_url(&resolve_portal_base_url_from_store())
        )),
        "limit" => {
            let state = build_billing_state().await;
            Ok(render_billing_limit(&state))
        }
        "step-up" | "stepup" => run_billing_step_up().await,
        "charge" => run_charge_command(args).await,
        "charge-status" | "charge_status" => run_charge_status_command(args).await,
        "auto-reload" | "auto_reload" => run_auto_reload_command(args).await,
        "help" | "--help" | "-h" => Ok(billing_usage().to_string()),
        other => Ok(format!(
            "Unknown billing action `{other}`.\n{}",
            billing_usage()
        )),
    }
}

pub async fn handle_billing_slash_args(args: &[&str]) -> Result<String, AgentError> {
    let owned = args
        .iter()
        .map(|arg| (*arg).to_string())
        .collect::<Vec<_>>();
    handle_billing_args(&owned).await
}

fn billing_usage() -> &'static str {
    "Usage:\n  hermes billing\n  hermes billing portal\n  hermes billing limit\n  hermes billing step-up\n  hermes billing charge <amount-usd> --confirm\n  hermes billing charge-status <charge-id>\n  hermes billing auto-reload <threshold-usd> <top-up-usd> --confirm"
}

async fn run_billing_step_up() -> Result<String, AgentError> {
    let current = read_nous_auth_state()?;
    let scope = format!("{} {}", DEFAULT_NOUS_SCOPE, BILLING_MANAGE_SCOPE);
    let mut options = NousDeviceCodeOptions {
        scope: Some(scope),
        ..NousDeviceCodeOptions::default()
    };
    if let Some(state) = current {
        options.portal_base_url = Some(state.portal_base_url);
        options.inference_base_url = Some(state.inference_base_url);
        options.client_id = Some(state.client_id);
    }
    let state = login_nous_device_code(options).await?;
    let granted = nous_token_has_billing_scope(state.scope.as_deref(), Some(&state.access_token));
    Ok(if granted {
        "Billing permission granted for Nous Portal.".to_string()
    } else {
        "Nous Portal login completed, but billing:manage was not granted. Ask an org admin/owner to allow terminal billing, then run `hermes billing step-up` again.".to_string()
    })
}

async fn run_charge_command(args: &[String]) -> Result<String, AgentError> {
    let Some(amount) = args.get(1).map(String::as_str) else {
        return Ok("Usage: hermes billing charge <amount-usd> --confirm".to_string());
    };
    if !args.iter().any(|arg| arg == "--confirm" || arg == "--yes") {
        return Ok(format!(
            "Refusing to create a live billing charge without `--confirm`.\nManage on portal: {}",
            fallback_portal_url(&resolve_portal_base_url_from_store())
        ));
    }
    let client = NousBillingClient::from_auth_store().map_err(billing_error_to_agent_error)?;
    let idempotency_key = args
        .iter()
        .find_map(|arg| arg.strip_prefix("--idempotency-key="))
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| Uuid::new_v4().to_string());
    let payload = client
        .post_charge(amount, &idempotency_key)
        .await
        .map_err(billing_error_to_agent_error)?;
    let charge_id = payload
        .get("chargeId")
        .or_else(|| payload.get("charge_id"))
        .and_then(Value::as_str)
        .unwrap_or("<missing>");
    Ok(format!(
        "Billing charge submitted\n  charge_id: {charge_id}\n  idempotency_key: {idempotency_key}\nPoll with: hermes billing charge-status {charge_id}"
    ))
}

async fn run_charge_status_command(args: &[String]) -> Result<String, AgentError> {
    let Some(charge_id) = args.get(1).map(String::as_str) else {
        return Ok("Usage: hermes billing charge-status <charge-id>".to_string());
    };
    let client = NousBillingClient::from_auth_store().map_err(billing_error_to_agent_error)?;
    let payload = client
        .get_charge_status(charge_id)
        .await
        .map_err(billing_error_to_agent_error)?;
    Ok(format!(
        "Billing charge status\n{}",
        serde_json::to_string_pretty(&payload).unwrap_or_else(|_| payload.to_string())
    ))
}

async fn run_auto_reload_command(args: &[String]) -> Result<String, AgentError> {
    let (Some(threshold), Some(top_up)) = (args.get(1), args.get(2)) else {
        return Ok(
            "Usage: hermes billing auto-reload <threshold-usd> <top-up-usd> --confirm".to_string(),
        );
    };
    if !args.iter().any(|arg| arg == "--confirm" || arg == "--yes") {
        return Ok(format!(
            "Refusing to change auto-reload without `--confirm`.\nManage on portal: {}",
            fallback_portal_url(&resolve_portal_base_url_from_store())
        ));
    }
    let client = NousBillingClient::from_auth_store().map_err(billing_error_to_agent_error)?;
    let payload = client
        .patch_auto_reload(true, threshold, top_up)
        .await
        .map_err(billing_error_to_agent_error)?;
    Ok(format!(
        "Billing auto-reload updated\n{}",
        serde_json::to_string_pretty(&payload).unwrap_or_else(|_| payload.to_string())
    ))
}

fn billing_error_to_agent_error(err: BillingError) -> AgentError {
    let hint = match err.kind {
        BillingErrorKind::ScopeRequired => {
            " Run `hermes billing step-up` to request billing:manage.".to_string()
        }
        BillingErrorKind::RateLimited => err
            .retry_after
            .map(|seconds| format!(" Retry after {seconds}s."))
            .unwrap_or_default(),
        BillingErrorKind::Auth => " Run `hermes portal` to log in.".to_string(),
        _ => String::new(),
    };
    AgentError::AuthFailed(format!("{err}{hint}"))
}

pub fn billing_state_from_payload(payload: &Value, fallback_portal_url: String) -> BillingState {
    let org = payload.get("org").and_then(Value::as_object);
    let bounds = payload.get("bounds").and_then(Value::as_object);
    let charge_presets = payload
        .get("chargePresets")
        .and_then(Value::as_array)
        .map(|items| items.iter().filter_map(parse_money_value).collect())
        .unwrap_or_default();
    let portal_base_url = portal_base_from_fallback_url(&fallback_portal_url);
    let portal_url = payload
        .get("portalUrl")
        .and_then(Value::as_str)
        .and_then(|url| absolutize_portal_url_with_base(url, &portal_base_url))
        .or(Some(fallback_portal_url));

    BillingState {
        logged_in: true,
        org_id: org
            .and_then(|o| value_string(o.get("id")))
            .map(ToOwned::to_owned),
        org_slug: org
            .and_then(|o| value_string(o.get("slug")))
            .map(ToOwned::to_owned),
        org_name: org
            .and_then(|o| value_string(o.get("name")))
            .map(ToOwned::to_owned),
        role: org
            .and_then(|o| value_string(o.get("role")))
            .map(ToOwned::to_owned),
        balance_usd: payload.get("balanceUsd").and_then(parse_money_value),
        cli_billing_enabled: payload
            .get("cliBillingEnabled")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        charge_presets,
        min_usd: bounds
            .and_then(|b| b.get("minUsd"))
            .and_then(parse_money_value),
        max_usd: bounds
            .and_then(|b| b.get("maxUsd"))
            .and_then(parse_money_value),
        card: parse_card(payload.get("card")),
        monthly_cap: parse_monthly_cap(payload.get("monthlyCap")),
        auto_reload: parse_auto_reload(payload.get("autoReload")),
        portal_url,
        error: None,
    }
}

pub fn render_billing_state(state: &BillingState) -> String {
    if !state.logged_in {
        let mut out =
            "Nous billing\nNot logged into Nous Portal. Run `hermes portal` to log in.".to_string();
        if let Some(error) = state
            .error
            .as_deref()
            .filter(|value| !value.trim().is_empty())
        {
            out.push_str(&format!("\nLast error: {error}"));
        }
        if let Some(url) = state.portal_url.as_deref() {
            out.push_str(&format!("\nManage on portal: {url}"));
        }
        return out;
    }

    let mut out = "Nous billing".to_string();
    if let Some(org) = state.org_name.as_deref().or(state.org_slug.as_deref()) {
        out.push_str(&format!("\nOrg: {org}"));
    }
    if let Some(role) = state.role.as_deref() {
        out.push_str(&format!("\nRole: {role}"));
    }
    out.push_str(&format!(
        "\nBalance: {}",
        state
            .balance_usd
            .as_deref()
            .map(format_money)
            .unwrap_or_else(|| "-".to_string())
    ));
    out.push_str(&format!(
        "\nTerminal billing: {}",
        if state.cli_billing_enabled {
            "enabled"
        } else {
            "off"
        }
    ));
    if let (Some(min), Some(max)) = (state.min_usd.as_deref(), state.max_usd.as_deref()) {
        out.push_str(&format!(
            "\nCharge bounds: {} - {}",
            format_money(min),
            format_money(max)
        ));
    }
    if !state.charge_presets.is_empty() {
        let presets = state
            .charge_presets
            .iter()
            .map(|value| format_money(value))
            .collect::<Vec<_>>()
            .join(", ");
        out.push_str(&format!("\nCharge presets: {presets}"));
    }

    if let Some(cap) = &state.monthly_cap {
        out.push('\n');
        out.push_str(&render_monthly_cap(cap));
    }
    if let Some(card) = &state.card {
        out.push_str(&format!("\nSaved card: {}", card.masked()));
    } else if state.can_charge() {
        out.push_str("\nSaved card: none. Set up a saved card on the portal before charging.");
    }
    if let Some(auto) = &state.auto_reload {
        out.push_str(&format!(
            "\nAuto-reload: {}",
            if auto.enabled { "enabled" } else { "off" }
        ));
        if auto.enabled {
            let threshold = auto
                .threshold_usd
                .as_deref()
                .map(format_money)
                .unwrap_or_else(|| "-".to_string());
            let reload_to = auto
                .reload_to_usd
                .as_deref()
                .map(format_money)
                .unwrap_or_else(|| "-".to_string());
            out.push_str(&format!(" ({threshold} -> {reload_to})"));
        }
    }

    if !state.is_admin() {
        out.push_str("\nBilling changes require an org admin/owner.");
    } else if !state.cli_billing_enabled {
        out.push_str("\nTerminal billing is turned off for this org.");
    } else {
        out.push_str("\nWrite actions require explicit confirmation: `hermes billing charge <amount> --confirm`.");
    }
    if let Some(url) = state.portal_url.as_deref() {
        out.push_str(&format!("\nManage on portal: {url}"));
    }
    out
}

pub fn render_billing_limit(state: &BillingState) -> String {
    if !state.logged_in {
        return render_billing_state(state);
    }
    let mut out = "Monthly spend limit".to_string();
    match &state.monthly_cap {
        Some(cap) => out.push_str(&format!("\n{}", render_monthly_cap(cap))),
        None => out.push_str("\nNo monthly cap data available."),
    }
    out.push_str("\nLimit changes are read-only from the terminal; manage limits on the portal.");
    if let Some(url) = state.portal_url.as_deref() {
        out.push_str(&format!("\nManage on portal: {url}"));
    }
    out
}

fn render_monthly_cap(cap: &MonthlyCap) -> String {
    let spent = cap
        .spent_this_month_usd
        .as_deref()
        .map(format_money)
        .unwrap_or_else(|| "-".to_string());
    let limit = cap
        .limit_usd
        .as_deref()
        .map(format_money)
        .unwrap_or_else(|| "-".to_string());
    let default = if cap.is_default_ceiling {
        " (default ceiling)"
    } else {
        ""
    };
    let bar = billing_usage_bar(
        cap.spent_this_month_usd.as_deref(),
        cap.limit_usd.as_deref(),
    );
    format!("Monthly spend: {spent} of {limit} used{default}{bar}")
}

fn billing_usage_bar(spent: Option<&str>, limit: Option<&str>) -> String {
    let (Some(spent), Some(limit)) = (spent, limit) else {
        return String::new();
    };
    let Some(spent_cents) = parse_cents(spent) else {
        return String::new();
    };
    let Some(limit_cents) = parse_cents(limit) else {
        return String::new();
    };
    if limit_cents <= 0 {
        return String::new();
    }
    let pct = ((spent_cents.max(0) as f64 / limit_cents as f64) * 100.0).clamp(0.0, 100.0);
    let filled = ((pct / 100.0) * 10.0).round() as usize;
    let empty = 10usize.saturating_sub(filled);
    format!(" [{}{}] {:.0}%", "#".repeat(filled), "-".repeat(empty), pct)
}

pub fn parse_money_value(value: &Value) -> Option<String> {
    match value {
        Value::String(raw) => parse_money_str(raw),
        Value::Number(number) => parse_money_str(&number.to_string()),
        _ => None,
    }
}

pub fn parse_money_str(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let unsigned = trimmed.strip_prefix('-').unwrap_or(trimmed);
    let mut parts = unsigned.split('.');
    let whole = parts.next()?;
    let frac = parts.next();
    if parts.next().is_some() || whole.is_empty() || !whole.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    if let Some(frac) = frac {
        if frac.is_empty() || !frac.chars().all(|c| c.is_ascii_digit()) {
            return None;
        }
    }
    Some(trimmed.to_string())
}

pub fn format_money(raw: &str) -> String {
    let Some(cents) = parse_cents(raw) else {
        return "-".to_string();
    };
    let sign = if cents < 0 { "-" } else { "" };
    let abs = cents.checked_abs().unwrap_or(i128::MAX);
    let dollars = abs / 100;
    let cents = abs % 100;
    if cents == 0 {
        format!("{sign}${dollars}")
    } else {
        format!("{sign}${dollars}.{cents:02}")
    }
}

fn parse_amount_number(raw: &str) -> Option<f64> {
    let normalized = parse_money_str(raw)?;
    let amount = normalized.parse::<f64>().ok()?;
    if amount.is_finite() && amount > 0.0 {
        Some(amount)
    } else {
        None
    }
}

fn parse_cents(raw: &str) -> Option<i128> {
    let normalized = parse_money_str(raw)?;
    let negative = normalized.starts_with('-');
    let unsigned = normalized.strip_prefix('-').unwrap_or(&normalized);
    let mut parts = unsigned.split('.');
    let dollars = parts.next()?.parse::<i128>().ok()?;
    let frac = parts.next().unwrap_or("");
    let mut digits = frac.chars().collect::<Vec<_>>();
    while digits.len() < 3 {
        digits.push('0');
    }
    let cents = digits[0].to_digit(10).unwrap_or(0) as i128 * 10
        + digits[1].to_digit(10).unwrap_or(0) as i128;
    let round = digits.get(2).and_then(|c| c.to_digit(10)).unwrap_or(0) >= 5;
    let mut total = dollars.checked_mul(100)?.checked_add(cents)?;
    if round {
        total = total.checked_add(1)?;
    }
    if negative {
        total = -total;
    }
    Some(total)
}

fn parse_card(value: Option<&Value>) -> Option<CardInfo> {
    let obj = value?.as_object()?;
    Some(CardInfo {
        brand: value_string(obj.get("brand"))?.to_string(),
        last4: value_string(obj.get("last4"))?.to_string(),
    })
}

fn parse_monthly_cap(value: Option<&Value>) -> Option<MonthlyCap> {
    let obj = value?.as_object()?;
    Some(MonthlyCap {
        limit_usd: obj.get("limitUsd").and_then(parse_money_value),
        spent_this_month_usd: obj.get("spentThisMonthUsd").and_then(parse_money_value),
        is_default_ceiling: obj
            .get("isDefaultCeiling")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    })
}

fn parse_auto_reload(value: Option<&Value>) -> Option<AutoReload> {
    let obj = value?.as_object()?;
    Some(AutoReload {
        enabled: obj.get("enabled").and_then(Value::as_bool).unwrap_or(false),
        threshold_usd: obj.get("thresholdUsd").and_then(parse_money_value),
        reload_to_usd: obj.get("reloadToUsd").and_then(parse_money_value),
    })
}

fn value_string(value: Option<&Value>) -> Option<&str> {
    value
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
}

pub fn resolve_portal_base_url(state: Option<&NousAuthState>) -> String {
    for key in ["HERMES_PORTAL_BASE_URL", "NOUS_PORTAL_BASE_URL"] {
        if let Ok(value) = std::env::var(key) {
            let trimmed = value.trim();
            if !trimmed.is_empty() {
                return trimmed.trim_end_matches('/').to_string();
            }
        }
    }
    state
        .map(|state| state.portal_base_url.trim())
        .filter(|value| !value.is_empty())
        .map(|value| value.trim_end_matches('/').to_string())
        .unwrap_or_else(|| DEFAULT_PORTAL_BASE_URL.to_string())
}

fn resolve_portal_base_url_from_store() -> String {
    read_nous_auth_state()
        .ok()
        .flatten()
        .as_ref()
        .map(|state| resolve_portal_base_url(Some(state)))
        .unwrap_or_else(|| resolve_portal_base_url(None))
}

pub fn fallback_portal_url(base: &str) -> String {
    format!("{}/billing?topup=open", base.trim_end_matches('/'))
}

fn portal_base_from_fallback_url(fallback_url: &str) -> String {
    match Url::parse(fallback_url) {
        Ok(parsed) => {
            let Some(host) = parsed.host_str() else {
                return resolve_portal_base_url(None);
            };
            let mut base = format!("{}://{}", parsed.scheme(), host);
            if let Some(port) = parsed.port() {
                base.push_str(&format!(":{port}"));
            }
            base
        }
        Err(_) => resolve_portal_base_url(None),
    }
}

fn absolutize_portal_url_with_base(url: &str, base: &str) -> Option<String> {
    let url = url.trim();
    if url.is_empty() {
        return None;
    }
    if url.starts_with("https://") || url.starts_with("http://") {
        return Some(url.to_string());
    }
    if url.starts_with('/') {
        return Some(format!("{}{}", base.trim_end_matches('/'), url));
    }
    Some(format!("{}/{}", base.trim_end_matches('/'), url))
}

pub fn nous_token_has_billing_scope(scope: Option<&str>, access_token: Option<&str>) -> bool {
    if scope_string_has(scope, BILLING_MANAGE_SCOPE) {
        return true;
    }
    let Some(token) = access_token else {
        return false;
    };
    let Some(payload) = token.trim().split('.').nth(1) else {
        return false;
    };
    let Ok(decoded) = URL_SAFE_NO_PAD.decode(payload.as_bytes()) else {
        return false;
    };
    let Ok(claims) = serde_json::from_slice::<Value>(&decoded) else {
        return false;
    };
    claim_has_scope(claims.get("scope"), BILLING_MANAGE_SCOPE)
        || claim_has_scope(claims.get("scp"), BILLING_MANAGE_SCOPE)
}

fn scope_string_has(scope: Option<&str>, needle: &str) -> bool {
    scope
        .unwrap_or_default()
        .split(|c: char| c.is_ascii_whitespace() || c == ',')
        .any(|value| value.trim() == needle)
}

fn claim_has_scope(value: Option<&Value>, needle: &str) -> bool {
    match value {
        Some(Value::String(raw)) => scope_string_has(Some(raw), needle),
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(Value::as_str)
            .any(|item| item.trim() == needle),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::{save_nous_auth_state, DEFAULT_NOUS_CLIENT_ID, DEFAULT_NOUS_INFERENCE_URL};
    use crate::test_env_lock;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use serde_json::json;
    use tempfile::tempdir;
    use wiremock::matchers::{body_json, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    struct EnvVarGuard {
        key: &'static str,
        previous: Option<String>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: impl AsRef<std::ffi::OsStr>) -> Self {
            let previous = std::env::var(key).ok();
            std::env::set_var(key, value);
            Self { key, previous }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            match self.previous.take() {
                Some(value) => std::env::set_var(self.key, value),
                None => std::env::remove_var(self.key),
            }
        }
    }

    fn sample_state(access_token: String, portal_base_url: String) -> NousAuthState {
        NousAuthState {
            portal_base_url,
            inference_base_url: DEFAULT_NOUS_INFERENCE_URL.to_string(),
            client_id: DEFAULT_NOUS_CLIENT_ID.to_string(),
            scope: Some(DEFAULT_NOUS_SCOPE.to_string()),
            token_type: "Bearer".to_string(),
            access_token,
            refresh_token: Some("refresh".to_string()),
            obtained_at: "2026-06-25T00:00:00Z".to_string(),
            expires_at: None,
            expires_in: Some(900),
            agent_key: None,
            agent_key_id: None,
            agent_key_expires_at: None,
            agent_key_expires_in: None,
            agent_key_reused: None,
            agent_key_obtained_at: None,
        }
    }

    fn scoped_jwt(scope: Value) -> String {
        let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"none"}"#);
        let payload = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&json!({"scope": scope})).unwrap());
        format!("{header}.{payload}.sig")
    }

    #[test]
    fn money_parse_and_format_matches_terminal_contract() {
        assert_eq!(parse_money_str("142.5").as_deref(), Some("142.5"));
        assert_eq!(format_money("142.5"), "$142.50");
        assert_eq!(format_money("100"), "$100");
        assert_eq!(format_money("0.01"), "$0.01");
        assert_eq!(format_money("1.239"), "$1.24");
        assert!(parse_money_str("12x").is_none());
    }

    #[test]
    fn billing_state_parses_payload_and_renders_overview() {
        let state = billing_state_from_payload(
            &json!({
                "org": {"id": "org1", "slug": "acme", "name": "Acme", "role": "OWNER"},
                "balanceUsd": "142.5",
                "cliBillingEnabled": true,
                "chargePresets": ["100", "250"],
                "bounds": {"minUsd": "10", "maxUsd": "10000"},
                "card": {"brand": "visa", "last4": "4242"},
                "monthlyCap": {"limitUsd": "1000", "spentThisMonthUsd": "180", "isDefaultCeiling": true},
                "autoReload": {"enabled": true, "thresholdUsd": "20", "reloadToUsd": "100"},
                "portalUrl": "/billing?topup=open"
            }),
            "https://portal.example/billing?topup=open".to_string(),
        );
        assert!(state.can_charge());
        assert_eq!(state.balance_usd.as_deref(), Some("142.5"));
        assert_eq!(state.charge_presets, vec!["100", "250"]);
        let rendered = render_billing_state(&state);
        assert!(rendered.contains("Nous billing"));
        assert!(rendered.contains("$142.50"));
        assert!(rendered.contains("$180 of $1000 used"));
        assert!(rendered.contains("18%"));
        assert!(rendered.contains("Charge bounds: $10 - $10000"));
        assert!(rendered.contains("Charge presets: $100, $250"));
        assert!(rendered.contains("Manage on portal:"));
    }

    #[test]
    fn scope_detection_is_tokenized_not_substring() {
        assert!(nous_token_has_billing_scope(
            Some("inference:invoke billing:manage"),
            None
        ));
        assert!(!nous_token_has_billing_scope(
            Some("inference:invoke notbilling:manage"),
            None
        ));
        let token = scoped_jwt(json!(["profile", BILLING_MANAGE_SCOPE]));
        assert!(nous_token_has_billing_scope(None, Some(&token)));
    }

    #[tokio::test]
    async fn client_maps_state_and_charge_contracts() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/billing/state"))
            .and(header("authorization", "Bearer access"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "org": {"name": "Acme", "role": "ADMIN"},
                "balanceUsd": "10",
                "cliBillingEnabled": true,
                "portalUrl": "/billing?topup=open"
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/billing/charge"))
            .and(header("idempotency-key", "key-1"))
            .and(body_json(json!({"amountUsd": 50.0})))
            .respond_with(ResponseTemplate::new(202).set_body_json(json!({"chargeId": "ch_1"})))
            .mount(&server)
            .await;

        let client = NousBillingClient::from_state(&sample_state("access".into(), server.uri()))
            .expect("client");
        let state = client.get_state().await.expect("state");
        let expected_portal_url = format!("{}/billing?topup=open", server.uri());
        assert_eq!(
            state.portal_url.as_deref(),
            Some(expected_portal_url.as_str())
        );
        let charge = client.post_charge("50", "key-1").await.expect("charge");
        assert_eq!(charge.get("chargeId").and_then(Value::as_str), Some("ch_1"));
    }

    #[tokio::test]
    async fn client_maps_typed_error_envelopes() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/billing/charge"))
            .respond_with(ResponseTemplate::new(403).set_body_json(json!({
                "error": "insufficient_scope",
                "message": "need scope",
                "portalUrl": "/billing"
            })))
            .mount(&server)
            .await;
        let client = NousBillingClient::from_state(&sample_state("access".into(), server.uri()))
            .expect("client");
        let err = client.post_charge("50", "key-1").await.unwrap_err();
        assert_eq!(err.kind, BillingErrorKind::ScopeRequired);
        let expected_portal_url = format!("{}/billing", server.uri());
        assert_eq!(
            err.portal_url.as_deref(),
            Some(expected_portal_url.as_str())
        );
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn handle_billing_overview_fails_open_when_logged_out() {
        let _guard = test_env_lock::lock();
        let tmp = tempdir().expect("tempdir");
        let _hermes_home = EnvVarGuard::set("HERMES_HOME", tmp.path());
        let _home = EnvVarGuard::set("HOME", tmp.path());
        let _auth_file = EnvVarGuard::set("HERMES_AUTH_FILE", tmp.path().join("auth.json"));
        let _nous_oauth =
            EnvVarGuard::set("HERMES_NOUS_OAUTH_FILE", tmp.path().join("nous_oauth.json"));
        let out = handle_billing_args(&[]).await.expect("overview");
        assert!(out.contains("Not logged into Nous Portal"));
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn handle_charge_requires_explicit_confirmation() {
        let _guard = test_env_lock::lock();
        let tmp = tempdir().expect("tempdir");
        let _hermes_home = EnvVarGuard::set("HERMES_HOME", tmp.path());
        let _home = EnvVarGuard::set("HOME", tmp.path());
        let _auth_file = EnvVarGuard::set("HERMES_AUTH_FILE", tmp.path().join("auth.json"));
        let _nous_oauth =
            EnvVarGuard::set("HERMES_NOUS_OAUTH_FILE", tmp.path().join("nous_oauth.json"));
        save_nous_auth_state(&sample_state(
            "access".into(),
            "https://portal.example".to_string(),
        ))
        .expect("save state");
        let out = handle_billing_args(&["charge".into(), "50".into()])
            .await
            .expect("charge");
        assert!(out.contains("without `--confirm`"));
    }
}
