//! Token and billing counters (`SessionDB.update_token_counts` parity).

use std::sync::{Arc, Mutex};

use rusqlite::{params, Connection};

use super::error::StateDbError;

/// Token/billing update payload (Python `update_token_counts` kwargs).
#[derive(Debug, Clone, Default)]
pub struct TokenCountUpdate {
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_tokens: i64,
    pub cache_write_tokens: i64,
    pub reasoning_tokens: i64,
    pub model: Option<String>,
    pub estimated_cost_usd: Option<f64>,
    pub actual_cost_usd: Option<f64>,
    pub cost_status: Option<String>,
    pub cost_source: Option<String>,
    pub pricing_version: Option<String>,
    pub billing_provider: Option<String>,
    pub billing_base_url: Option<String>,
    pub billing_mode: Option<String>,
    pub api_call_count: i64,
    /// When true, set totals directly (gateway); otherwise increment (CLI per-turn).
    pub absolute: bool,
}

impl TokenCountUpdate {
    pub fn increment(
        input_tokens: i64,
        output_tokens: i64,
        model: Option<String>,
        estimated_cost_usd: Option<f64>,
    ) -> Self {
        Self {
            input_tokens,
            output_tokens,
            model,
            estimated_cost_usd,
            api_call_count: 1,
            ..Default::default()
        }
    }
}

pub fn ensure_session_row(
    conn: &Connection,
    session_id: &str,
    source: &str,
    model: Option<&str>,
) -> Result<(), StateDbError> {
    super::sessions::insert_session_if_missing(
        conn,
        session_id,
        source,
        model,
        None,
        None,
        None,
        {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs_f64())
                .unwrap_or(0.0)
        },
    )
}

pub fn update_token_counts(
    conn: &Arc<Mutex<Connection>>,
    session_id: &str,
    update: &TokenCountUpdate,
) -> Result<(), StateDbError> {
    let sid = session_id.to_string();
    let update = update.clone();
    let guard = conn
        .lock()
        .map_err(|_| StateDbError("state db lock poisoned".into()))?;
    ensure_session_row(
        &guard,
        &sid,
        "unknown",
        update.model.as_deref(),
    )?;
    drop(guard);

    let guard = conn
        .lock()
        .map_err(|_| StateDbError("state db lock poisoned".into()))?;
    if update.absolute {
        guard.execute(
            "UPDATE sessions SET
                input_tokens = ?1,
                output_tokens = ?2,
                cache_read_tokens = ?3,
                cache_write_tokens = ?4,
                reasoning_tokens = ?5,
                estimated_cost_usd = COALESCE(?6, 0),
                actual_cost_usd = CASE WHEN ?7 IS NULL THEN actual_cost_usd ELSE ?7 END,
                cost_status = COALESCE(?8, cost_status),
                cost_source = COALESCE(?9, cost_source),
                pricing_version = COALESCE(?10, pricing_version),
                billing_provider = COALESCE(billing_provider, ?11),
                billing_base_url = COALESCE(billing_base_url, ?12),
                billing_mode = COALESCE(billing_mode, ?13),
                model = COALESCE(model, ?14),
                api_call_count = ?15
             WHERE id = ?16",
            params![
                update.input_tokens,
                update.output_tokens,
                update.cache_read_tokens,
                update.cache_write_tokens,
                update.reasoning_tokens,
                update.estimated_cost_usd,
                update.actual_cost_usd,
                update.cost_status,
                update.cost_source,
                update.pricing_version,
                update.billing_provider,
                update.billing_base_url,
                update.billing_mode,
                update.model,
                update.api_call_count,
                sid,
            ],
        )?;
    } else {
        guard.execute(
            "UPDATE sessions SET
                input_tokens = input_tokens + ?1,
                output_tokens = output_tokens + ?2,
                cache_read_tokens = cache_read_tokens + ?3,
                cache_write_tokens = cache_write_tokens + ?4,
                reasoning_tokens = reasoning_tokens + ?5,
                estimated_cost_usd = COALESCE(estimated_cost_usd, 0) + COALESCE(?6, 0),
                actual_cost_usd = CASE
                    WHEN ?7 IS NULL THEN actual_cost_usd
                    ELSE COALESCE(actual_cost_usd, 0) + ?7
                END,
                cost_status = COALESCE(?8, cost_status),
                cost_source = COALESCE(?9, cost_source),
                pricing_version = COALESCE(?10, pricing_version),
                billing_provider = COALESCE(billing_provider, ?11),
                billing_base_url = COALESCE(billing_base_url, ?12),
                billing_mode = COALESCE(billing_mode, ?13),
                model = COALESCE(?14, model),
                api_call_count = COALESCE(api_call_count, 0) + ?15
             WHERE id = ?16",
            params![
                update.input_tokens,
                update.output_tokens,
                update.cache_read_tokens,
                update.cache_write_tokens,
                update.reasoning_tokens,
                update.estimated_cost_usd,
                update.actual_cost_usd,
                update.cost_status,
                update.cost_source,
                update.pricing_version,
                update.billing_provider,
                update.billing_base_url,
                update.billing_mode,
                update.model,
                update.api_call_count,
                sid,
            ],
        )?;
    }
    Ok(())
}
