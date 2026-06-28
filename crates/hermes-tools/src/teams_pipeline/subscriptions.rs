pub fn default_change_type_for_resource(resource: &str) -> &'static str {
    let normalized = resource.trim().to_ascii_lowercase();
    if normalized.starts_with("communications/onlinemeetings/getalltranscripts")
        || normalized.starts_with("communications/onlinemeetings/getallrecordings")
        || normalized.starts_with("communications/callrecords")
    {
        "created"
    } else {
        "updated"
    }
}

pub fn expected_client_state(raw: Option<&str>) -> Option<String> {
    raw.map(ToOwned::to_owned)
        .or_else(|| env_nonempty("MSGRAPH_WEBHOOK_CLIENT_STATE"))
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

pub fn sync_graph_subscription_record(
    store: &TeamsPipelineStore,
    subscription_payload: Value,
    status: Option<&str>,
    renewed: bool,
) -> TeamsPipelineResult<Value> {
    let mut normalized = GraphSubscription::from_value(subscription_payload)?;
    if normalized.status.is_none() {
        normalized.status = Some(
            if let Some(expiration) = parse_datetime_utc(&normalized.expiration_datetime) {
                if expiration <= Utc::now() {
                    "expired".into()
                } else {
                    status.unwrap_or("active").into()
                }
            } else {
                status.unwrap_or("active").into()
            },
        );
    }
    if let Some(status) = status {
        normalized.status = Some(status.into());
    }
    if renewed {
        normalized.latest_renewal_at = Some(utc_now_iso());
    }
    let subscription_id = normalized.subscription_id.clone();
    store.upsert_subscription(
        &subscription_id,
        strip_empty_object_keys(serde_json::to_value(normalized)?),
    )
}

pub async fn maintain_graph_subscriptions(
    client: &MicrosoftGraphClient,
    store: &TeamsPipelineStore,
    renew_within_hours: u32,
    extend_hours: u32,
    dry_run: bool,
    client_state: Option<&str>,
) -> TeamsPipelineResult<Value> {
    let threshold_hours = renew_within_hours.max(1);
    let extend_hours = extend_hours.max(1);
    let managed_client_state = expected_client_state(client_state);
    let now = Utc::now();
    let remote_subscriptions = client.collect_paginated("/subscriptions").await?;
    let mut remote_ids = HashSet::new();
    let mut synced = 0usize;
    let mut candidates = Vec::new();
    let mut renewed = Vec::new();
    let mut skipped = Vec::new();

    for raw in &remote_subscriptions {
        let subscription_id = raw
            .get("id")
            .and_then(value_to_string)
            .or_else(|| raw.get("subscription_id").and_then(value_to_string))
            .unwrap_or_default();
        if subscription_id.is_empty() {
            continue;
        }
        let managed = store.get_subscription(&subscription_id)?.is_some()
            || managed_client_state
                .as_deref()
                .and_then(|expected| {
                    raw.get("clientState")
                        .and_then(value_to_string)
                        .map(|actual| actual == expected)
                })
                .unwrap_or(false);
        if !managed {
            skipped.push(json!({
                "subscription_id": subscription_id,
                "reason": "not_managed_by_teams_pipeline"
            }));
            continue;
        }
        remote_ids.insert(subscription_id.clone());
        sync_graph_subscription_record(store, raw.clone(), None, false)?;
        synced += 1;
        let Some(expiration_text) = raw.get("expirationDateTime").and_then(value_to_string) else {
            skipped
                .push(json!({"subscription_id": subscription_id, "reason": "missing_expiration"}));
            continue;
        };
        let Some(expiration) = parse_datetime_utc(&expiration_text) else {
            skipped
                .push(json!({"subscription_id": subscription_id, "reason": "invalid_expiration"}));
            continue;
        };
        let seconds_until_expiry = (expiration - now).num_seconds();
        if seconds_until_expiry < 0 {
            store.upsert_subscription(
                &subscription_id,
                json!({
                    "status": "expired",
                    "expiration_datetime": expiration.to_rfc3339_opts(SecondsFormat::Secs, true)
                }),
            )?;
            skipped.push(json!({
                "subscription_id": subscription_id,
                "reason": "already_expired",
                "expiration_datetime": expiration.to_rfc3339_opts(SecondsFormat::Secs, true)
            }));
            continue;
        }
        if seconds_until_expiry > i64::from(threshold_hours) * 3600 {
            skipped.push(json!({
                "subscription_id": subscription_id,
                "reason": "not_due",
                "expires_in_seconds": seconds_until_expiry
            }));
            continue;
        }
        let new_expiration = (std::cmp::max(now, expiration)
            + ChronoDuration::hours(i64::from(extend_hours)))
        .to_rfc3339_opts(SecondsFormat::Secs, true);
        let candidate = json!({
            "subscription_id": subscription_id,
            "resource": raw.get("resource").cloned().unwrap_or(Value::Null),
            "current_expiration": expiration_text,
            "new_expiration": new_expiration
        });
        candidates.push(candidate.clone());
        if dry_run {
            continue;
        }
        let patched = client
            .patch_json(
                &format!("/subscriptions/{}", path_percent_encode(&subscription_id)),
                json!({"expirationDateTime": new_expiration}),
            )
            .await?;
        let mut merged = raw.clone();
        if let (Some(base), Some(patch)) = (merged.as_object_mut(), patched.as_object()) {
            for (key, value) in patch {
                base.insert(key.clone(), value.clone());
            }
        }
        sync_graph_subscription_record(store, merged, Some("active"), true)?;
        renewed.push(json!({"candidate": candidate, "result": patched}));
    }

    for subscription_id in store.list_subscriptions()?.keys() {
        if !remote_ids.contains(subscription_id) {
            store.upsert_subscription(
                subscription_id,
                json!({
                    "status": "missing_remote",
                    "last_seen_missing_remote_at": utc_now_iso()
                }),
            )?;
        }
    }

    Ok(json!({
        "success": true,
        "dry_run": dry_run,
        "store_path": store.path(),
        "remote_subscription_count": remote_subscriptions.len(),
        "synced_subscription_count": synced,
        "candidate_count": candidates.len(),
        "renewed_count": renewed.len(),
        "threshold_hours": threshold_hours,
        "extend_hours": extend_hours,
        "candidates": candidates,
        "renewed": renewed,
        "skipped": skipped
    }))
}
