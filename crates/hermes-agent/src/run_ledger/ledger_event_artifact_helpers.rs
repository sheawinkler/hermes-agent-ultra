#[derive(Clone)]
struct EventDraft {
    event_id: EventId,
    run_id: RunId,
    kind: SessionRunEventKind,
    append_key: Option<AppendKey>,
    effect_intent: EffectIntent,
    artifact_degradation_reason: Option<ArtifactDegradationReason>,
    identity: Option<AppendIdentity>,
    sealed_artifact_ref: Option<SealedArtifactRef>,
    run_provenance: Option<CanonicalJson>,
    public_payload: CanonicalJson,
    created_at_ms: i64,
}

struct CommittedEvent {
    event_id: EventId,
    sequence: u64,
    event_hash: String,
    event: SessionRunEvent,
    outbox_ids: Vec<OutboxRowId>,
}

fn append_event_tx(
    transaction: &Transaction<'_>,
    config: &RunLedgerConfig,
    draft: EventDraft,
) -> LedgerResult<CommittedEvent> {
    if draft.effect_intent.requires_sealed_artifact() && draft.sealed_artifact_ref.is_none() {
        return Err(LedgerError::IntegrityViolation(
            "effect-capable event draft lacks a sealed artifact reference".to_string(),
        ));
    }
    let manual_degradation = draft.kind == SessionRunEventKind::ManualAppendDegradedAudit;
    if manual_degradation != draft.artifact_degradation_reason.is_some()
        || (manual_degradation
            && (draft.effect_intent != EffectIntent::None
                || draft.identity.is_some()
                || draft.sealed_artifact_ref.is_some()
                || draft.append_key.is_none()))
    {
        return Err(LedgerError::IntegrityViolation(
            "manual artifact degradation event draft has invalid effect or identity state"
                .to_string(),
        ));
    }
    let artifact_state_audit = draft.kind == SessionRunEventKind::ArtifactStateAudit;
    if artifact_state_audit
        && (draft.effect_intent != EffectIntent::None
            || draft.append_key.is_some()
            || draft.identity.is_some()
            || draft.sealed_artifact_ref.is_some()
            || draft.artifact_degradation_reason.is_some()
            || draft.run_provenance.is_some())
    {
        return Err(LedgerError::IntegrityViolation(
            "artifact state audit event draft has invalid effect or identity state".to_string(),
        ));
    }
    if artifact_state_audit {
        validate_artifact_state_audit_payload(&draft.public_payload)?;
    }
    let manifest = DispatchPolicyV1::derive(
        draft.event_id,
        draft.effect_intent,
        &draft.public_payload,
        draft.sealed_artifact_ref.as_ref(),
    )?;
    if manifest.sealed_artifact_ref != draft.sealed_artifact_ref {
        return Err(LedgerError::IntegrityViolation(
            "derived manifest sealed artifact reference does not equal event reference".to_string(),
        ));
    }
    let (current_sequence, previous_hash): (i64, String) = transaction.query_row(
        "SELECT next_event_sequence, head_hash FROM session_runs WHERE run_id = ?1",
        [draft.run_id.to_string()],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let next_sequence = current_sequence
        .checked_add(1)
        .ok_or(LedgerError::NumericOverflow)?;
    let sequence = u64::try_from(next_sequence).map_err(|_| LedgerError::NumericOverflow)?;
    let event = event_from_draft(sequence, previous_hash.clone(), &manifest.hash, &draft)?;

    let fingerprint_algorithm = event
        .private_fingerprint
        .as_ref()
        .map(|fingerprint| fingerprint.algorithm().as_str());
    let fingerprint_algorithm_version = event
        .private_fingerprint
        .as_ref()
        .map(|fingerprint| i64::from(fingerprint.algorithm_version()));
    let fingerprint_key_id = event
        .private_fingerprint
        .as_ref()
        .map(|fingerprint| fingerprint.key_id().as_str());
    let fingerprint_key_version = event
        .private_fingerprint
        .as_ref()
        .map(|fingerprint| i64::from(fingerprint.key_version()));
    let fingerprint = event
        .private_fingerprint
        .as_ref()
        .map(PrivateFingerprint::as_hex);
    let artifact_values = ArtifactRefDbValues::from(event.sealed_artifact_ref.as_ref());
    transaction.execute(
        "INSERT INTO session_run_events (
            event_id, run_id, event_sequence, event_kind, append_key, effect_intent,
            artifact_degradation_reason, public_identity_jcs, private_fingerprint_algorithm,
            private_fingerprint_algorithm_version, private_fingerprint_key_id,
            private_fingerprint_key_version, private_fingerprint,
            sealed_artifact_envelope_digest, sealed_artifact_key_id,
            sealed_artifact_schema_version, sealed_artifact_encryption_algorithm,
            sealed_artifact_canonicalization_algorithm,
            sealed_artifact_canonicalization_version, sealed_artifact_redactor_version,
            run_provenance_jcs, public_payload_jcs, manifest_hash, previous_hash, event_hash,
            canonicalization_algorithm, canonicalization_version, created_at_ms
        ) VALUES (
            ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16,
            ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27, ?28
        )",
        params![
            event.event_id.to_string(),
            event.run_id.to_string(),
            i64::try_from(event.sequence).map_err(|_| LedgerError::NumericOverflow)?,
            event.kind.as_str(),
            event.append_key.as_ref().map(AppendKey::as_str),
            event.effect_intent.as_str(),
            event
                .artifact_degradation_reason
                .map(ArtifactDegradationReason::as_str),
            event.public_identity.as_ref().map(CanonicalJson::as_str),
            fingerprint_algorithm,
            fingerprint_algorithm_version,
            fingerprint_key_id,
            fingerprint_key_version,
            fingerprint,
            artifact_values.envelope_digest,
            artifact_values.key_id,
            artifact_values.schema_version,
            artifact_values.encryption_algorithm,
            artifact_values.canonicalization_algorithm,
            artifact_values.canonicalization_version,
            artifact_values.redactor_version,
            event.run_provenance.as_ref().map(CanonicalJson::as_str),
            event.public_payload.as_str(),
            event.manifest_hash,
            event.previous_hash,
            event.event_hash,
            CANONICALIZATION_ALGORITHM,
            i64::from(CANONICALIZATION_VERSION),
            event.created_at_ms,
        ],
    )?;
    if let Some(reference) = event.sealed_artifact_ref.as_ref() {
        register_sealed_artifact_reference_tx(
            transaction,
            event.run_id,
            event.event_id,
            reference,
            event.created_at_ms,
        )?;
    }
    transaction.execute(
        "INSERT INTO session_run_manifests (
            event_id, manifest_jcs, manifest_hash, canonicalization_algorithm,
            canonicalization_version
         ) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            event.event_id.to_string(),
            manifest.canonical_json.as_str(),
            manifest.hash,
            CANONICALIZATION_ALGORITHM,
            i64::from(CANONICALIZATION_VERSION),
        ],
    )?;

    let mut outbox_ids = Vec::with_capacity(manifest.entries.len());
    for entry in &manifest.entries {
        if event.sealed_artifact_ref.as_ref() != Some(&entry.sealed_artifact_ref) {
            return Err(LedgerError::IntegrityViolation(
                "derived outbox reference does not equal event reference".to_string(),
            ));
        }
        let outbox_id = OutboxRowId::new();
        transaction.execute(
            "INSERT INTO session_run_outbox (
                outbox_id, run_id, event_id, manifest_order, destination, required,
                idempotency_key, public_payload_jcs,
                sealed_artifact_envelope_digest, sealed_artifact_key_id,
                sealed_artifact_schema_version, sealed_artifact_encryption_algorithm,
                sealed_artifact_canonicalization_algorithm,
                sealed_artifact_canonicalization_version, sealed_artifact_redactor_version,
                state, attempt_count, max_attempts, lease_generation
            ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15,
                'ready', 0, ?16, 0
            )",
            params![
                outbox_id.to_string(),
                event.run_id.to_string(),
                event.event_id.to_string(),
                i64::from(entry.order),
                entry.destination.as_str(),
                i64::from(entry.required),
                entry.idempotency_key.as_str(),
                entry.public_payload.as_str(),
                entry.sealed_artifact_ref.envelope_digest().as_str(),
                entry.sealed_artifact_ref.key_id().as_str(),
                i64::from(entry.sealed_artifact_ref.schema_version()),
                entry.sealed_artifact_ref.encryption_algorithm(),
                entry.sealed_artifact_ref.canonicalization_algorithm(),
                i64::from(entry.sealed_artifact_ref.canonicalization_version()),
                entry.sealed_artifact_ref.redactor_version().as_str(),
                i64::from(config.max_delivery_attempts),
            ],
        )?;
        outbox_ids.push(outbox_id);
    }

    insert_delivery_request_obligations_tx(transaction, &event, &outbox_ids)?;

    if event.effect_intent == EffectIntent::LocalOneShotV1 {
        let idempotency_key = derive_nonresumable_idempotency_key(event.event_id)?;
        transaction.execute(
            "INSERT INTO session_run_nonresumable_effects (
                event_id, run_id, state, idempotency_key
             ) VALUES (?1, ?2, 'armed', ?3)",
            params![
                event.event_id.to_string(),
                event.run_id.to_string(),
                idempotency_key.as_str(),
            ],
        )?;
    }

    let changed = transaction.execute(
        "UPDATE session_runs
         SET next_event_sequence = ?1, head_hash = ?2
         WHERE run_id = ?3 AND next_event_sequence = ?4 AND head_hash = ?5",
        params![
            next_sequence,
            event.event_hash,
            event.run_id.to_string(),
            current_sequence,
            previous_hash,
        ],
    )?;
    if changed != 1 {
        return Err(LedgerError::IntegrityViolation(
            "run chain head changed while appending an event".to_string(),
        ));
    }

    Ok(CommittedEvent {
        event_id: event.event_id,
        sequence: event.sequence,
        event_hash: event.event_hash.clone(),
        event,
        outbox_ids,
    })
}

/// Bind every freshly created outbox row to its own replay-critical delivery
/// request. The request references the source event's already sealed payload;
/// it does not duplicate artifact storage or assert a second payload.
fn insert_delivery_request_obligations_tx(
    transaction: &Transaction<'_>,
    event: &SessionRunEvent,
    outbox_ids: &[OutboxRowId],
) -> LedgerResult<()> {
    if outbox_ids.is_empty() {
        return Ok(());
    }
    let requested_fidelity = event
        .append_key
        .as_ref()
        .map(|append_key| {
            transaction
                .query_row(
                    "SELECT requested_fidelity
                     FROM session_run_replay_append_contracts WHERE append_key = ?1",
                    [append_key.as_str()],
                    |row| row.get::<_, String>(0),
                )
                .optional()
        })
        .transpose()?
        .flatten();
    let fidelity = match (requested_fidelity.as_deref(), event.sealed_artifact_ref.is_some()) {
        (Some("exact_private"), true) => ReplayObligationFidelity::ExactPrivate,
        _ => ReplayObligationFidelity::MetadataOnly,
    };
    for outbox_id in outbox_ids {
        transaction.execute(
            "INSERT INTO session_run_replay_obligations (
                run_id, source_event_id, operation_kind, phase, operation_instance_id,
                replay_fidelity, outcome, resolved_event_id, created_at_ms, resolved_at_ms
             ) VALUES (?1, ?2, 'delivery', 'request', ?3, ?4, 'not_applicable', ?2, ?5, ?5)",
            params![
                event.run_id.to_string(),
                event.event_id.to_string(),
                outbox_id.to_string(),
                fidelity.as_str(),
                event.created_at_ms,
            ],
        )?;
    }
    Ok(())
}
#[derive(Clone)]
struct PhysicalArtifactRecord {
    artifact: SealedArtifactRef,
    state: ArtifactPhysicalState,
    reason: Option<ArtifactStateReason>,
    transition_sequence: u64,
    tombstoned_at_ms: Option<i64>,
    retired_at_ms: Option<i64>,
}

fn load_physical_artifact(
    connection: &Connection,
    digest: &str,
) -> LedgerResult<(SealedArtifactRef, PhysicalArtifactRecord)> {
    let raw = connection
        .query_row(
            "SELECT envelope_digest, key_id, schema_version, encryption_algorithm,
                    canonicalization_algorithm, canonicalization_version, redactor_version,
                    state, state_reason, transition_sequence, tombstoned_at_ms, retired_at_ms
             FROM session_run_artifact_objects
             WHERE envelope_digest = ?1",
            [digest],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, Option<String>>(8)?,
                    row.get::<_, i64>(9)?,
                    row.get::<_, Option<i64>>(10)?,
                    row.get::<_, Option<i64>>(11)?,
                ))
            },
        )
        .optional()?
        .ok_or_else(|| {
            LedgerError::SealedArtifactReference(format!(
                "sealed artifact object {digest} was not registered"
            ))
        })?;
    let artifact = SealedArtifactRef::from_db(raw.0, raw.1, raw.2, raw.3, raw.4, raw.5, raw.6)?;
    let record = PhysicalArtifactRecord {
        artifact: artifact.clone(),
        state: ArtifactPhysicalState::from_db(&raw.7)?,
        reason: raw
            .8
            .as_deref()
            .map(ArtifactStateReason::from_db)
            .transpose()?,
        transition_sequence: parse_nonnegative_u64("artifact physical transition sequence", raw.9)?,
        tombstoned_at_ms: raw.10,
        retired_at_ms: raw.11,
    };
    validate_physical_state_record(&record)?;
    Ok((artifact, record))
}

fn load_artifact_reference_status(
    connection: &Connection,
    run_id: RunId,
    event_id: EventId,
    artifact: &SealedArtifactRef,
) -> LedgerResult<ArtifactReferenceStatus> {
    let raw = connection
        .query_row(
            "SELECT a.run_id, a.event_id,
                    o.envelope_digest, o.key_id, o.schema_version, o.encryption_algorithm,
                    o.canonicalization_algorithm, o.canonicalization_version, o.redactor_version,
                    o.state, o.state_reason,
                    a.state, a.state_reason,
                    x.exact_replay, x.state, x.state_reason, x.transition_sequence,
                    x.latched_at_ms, x.latched_by_event_id
             FROM session_run_artifact_refs AS a
             JOIN session_run_artifact_objects AS o
               ON o.envelope_digest = a.envelope_digest
             JOIN session_run_exact_replay AS x
               ON x.run_id = a.run_id
             WHERE a.run_id = ?1 AND a.event_id = ?2 AND a.envelope_digest = ?3",
            params![
                run_id.to_string(),
                event_id.to_string(),
                artifact.envelope_digest().as_str(),
            ],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, i64>(7)?,
                    row.get::<_, String>(8)?,
                    row.get::<_, String>(9)?,
                    row.get::<_, Option<String>>(10)?,
                    row.get::<_, String>(11)?,
                    row.get::<_, Option<String>>(12)?,
                    row.get::<_, i64>(13)?,
                    row.get::<_, String>(14)?,
                    row.get::<_, Option<String>>(15)?,
                    row.get::<_, i64>(16)?,
                    row.get::<_, Option<i64>>(17)?,
                    row.get::<_, Option<String>>(18)?,
                ))
            },
        )
        .optional()?
        .ok_or_else(|| LedgerError::ArtifactReferenceNotFound {
            run_id: run_id.to_string(),
            event_id: event_id.to_string(),
            digest: artifact.envelope_digest().as_str().to_string(),
        })?;
    let stored_artifact =
        SealedArtifactRef::from_db(raw.2, raw.3, raw.4, raw.5, raw.6, raw.7, raw.8)?;
    if &stored_artifact != artifact {
        return Err(LedgerError::ArtifactReferenceAliasConflict {
            digest: artifact.envelope_digest().as_str().to_string(),
        });
    }
    let exact_state = ExactReplayState::from_db(&raw.14)?;
    let exact_replay = parse_sql_bool(raw.13, "exact replay")?;
    if exact_replay != exact_state.exact_replay() {
        return Err(LedgerError::InvalidStoredState {
            kind: "exact replay latch",
            value: raw.14,
        });
    }
    let transition_sequence =
        parse_nonnegative_u64("exact replay transition sequence", raw.16)?;
    let latched_by_event_id = raw.18.map(parse_event_id).transpose()?;
    match (transition_sequence, raw.17, latched_by_event_id) {
        (0, None, None) => {}
        (0, _, _) => {
            return Err(LedgerError::InvalidStoredState {
                kind: "exact replay latch",
                value: "zero transition sequence has latch fields".to_string(),
            });
        }
        (_, Some(_), Some(latched_event_id)) => {
            let latched_run_id: Option<String> = connection
                .query_row(
                    "SELECT run_id FROM session_run_events WHERE event_id = ?1",
                    [latched_event_id.to_string()],
                    |row| row.get(0),
                )
                .optional()?;
            let expected_run_id = run_id.to_string();
            if latched_run_id.as_deref() != Some(expected_run_id.as_str()) {
                return Err(LedgerError::InvalidStoredState {
                    kind: "exact replay latch",
                    value: "latched event belongs to another run or is absent".to_string(),
                });
            }
        }
        (_, _, _) => {
            return Err(LedgerError::InvalidStoredState {
                kind: "exact replay latch",
                value: "nonzero transition sequence has partial latch fields".to_string(),
            });
        }
    }
    let status = ArtifactReferenceStatus {
        run_id: parse_run_id(raw.0)?,
        event_id: parse_event_id(raw.1)?,
        artifact: stored_artifact,
        physical_state: ArtifactPhysicalState::from_db(&raw.9)?,
        physical_reason: raw
            .10
            .as_deref()
            .map(ArtifactStateReason::from_db)
            .transpose()?,
        reference_state: ArtifactReferenceState::from_db(&raw.11)?,
        reference_reason: raw
            .12
            .as_deref()
            .map(ArtifactStateReason::from_db)
            .transpose()?,
        exact_replay: ExactReplayStatus {
            state: exact_state,
            reason: raw
                .15
                .as_deref()
                .map(ArtifactStateReason::from_db)
                .transpose()?,
            exact_replay,
            transition_sequence,
            latched_at_ms: raw.17,
            latched_by_event_id,
        },
    };
    validate_artifact_status(&status)?;
    Ok(status)
}

fn register_sealed_artifact_reference_tx(
    transaction: &Transaction<'_>,
    run_id: RunId,
    event_id: EventId,
    artifact: &SealedArtifactRef,
    now: i64,
) -> LedgerResult<()> {
    let digest = artifact.envelope_digest().as_str();
    let existing = transaction
        .query_row(
            "SELECT envelope_digest FROM session_run_artifact_objects WHERE envelope_digest = ?1",
            [digest],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    if existing.is_none() {
        transaction.execute(
            "INSERT INTO session_run_artifact_objects (
                envelope_digest, key_id, schema_version, encryption_algorithm,
                canonicalization_algorithm, canonicalization_version, redactor_version,
                state, state_reason, transition_sequence, tombstoned_at_ms, retired_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'available', NULL, 0, NULL, NULL)",
            params![
                digest,
                artifact.key_id().as_str(),
                i64::from(artifact.schema_version()),
                artifact.encryption_algorithm(),
                artifact.canonicalization_algorithm(),
                i64::from(artifact.canonicalization_version()),
                artifact.redactor_version().as_str(),
            ],
        )?;
    } else {
        let (_, physical) = load_physical_artifact(transaction, digest)?;
        if physical.artifact != *artifact {
            return Err(LedgerError::ArtifactReferenceAliasConflict {
                digest: digest.to_string(),
            });
        }
        if !matches!(
            physical.state,
            ArtifactPhysicalState::Unverified | ArtifactPhysicalState::Available
        ) {
            return Err(LedgerError::ArtifactStateTransitionInvalid {
                event_id: "<physical>".to_string(),
                digest: artifact.envelope_digest().as_str().to_string(),
                from: physical.state.as_str().to_string(),
                to: "available".to_string(),
            });
        }
    }
    let existing_ref = transaction
        .query_row(
            "SELECT run_id, state, state_reason
             FROM session_run_artifact_refs
             WHERE event_id = ?1 AND envelope_digest = ?2",
            params![event_id.to_string(), digest],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            },
        )
        .optional()?;
    if let Some((stored_run_id, _, _)) = existing_ref {
        if stored_run_id != run_id.to_string() {
            return Err(LedgerError::IntegrityViolation(
                "artifact event reference is bound to a different run".to_string(),
            ));
        }
        return Ok(());
    }
    transaction.execute(
        "INSERT INTO session_run_artifact_refs (
            event_id, run_id, envelope_digest, state, state_reason,
            transition_sequence, state_changed_at_ms
         ) VALUES (?1, ?2, ?3, 'available', NULL, 0, ?4)",
        params![event_id.to_string(), run_id.to_string(), digest, now],
    )?;
    Ok(())
}

fn mark_sealed_artifact_verified_tx(
    transaction: &Transaction<'_>,
    artifact: &SealedArtifactRef,
    now: i64,
) -> LedgerResult<Vec<ArtifactStateTransition>> {
    let (_, physical) = load_physical_artifact(transaction, artifact.envelope_digest().as_str())?;
    if physical.artifact != *artifact {
        return Err(LedgerError::ArtifactReferenceAliasConflict {
            digest: artifact.envelope_digest().as_str().to_string(),
        });
    }
    match physical.state {
        ArtifactPhysicalState::Unverified => {
            set_physical_artifact_state_tx(
                transaction,
                artifact,
                ArtifactPhysicalState::Available,
                None,
                now,
            )?;
        }
        ArtifactPhysicalState::Available => {}
        state => {
            return Err(LedgerError::ArtifactStateTransitionInvalid {
                event_id: "<physical>".to_string(),
                digest: artifact.envelope_digest().as_str().to_string(),
                from: state.as_str().to_string(),
                to: ArtifactPhysicalState::Available.as_str().to_string(),
            });
        }
    }
    let references = artifact_reference_keys_with_state_tx(
        transaction,
        artifact.envelope_digest().as_str(),
        ArtifactReferenceState::Unverified,
    )?;
    references
        .into_iter()
        .map(|(run_id, event_id)| {
            transition_artifact_reference_tx(
                transaction,
                run_id,
                event_id,
                artifact,
                ArtifactReferenceState::Available,
                None,
                now,
            )
        })
        .collect()
}

fn mark_sealed_artifact_unavailable_tx(
    transaction: &Transaction<'_>,
    artifact: &SealedArtifactRef,
    reason: ArtifactStateReason,
    now: i64,
) -> LedgerResult<Vec<ArtifactStateTransition>> {
    let to_physical = physical_state_for_reason(reason)?;
    let (_, physical) = load_physical_artifact(transaction, artifact.envelope_digest().as_str())?;
    if physical.artifact != *artifact {
        return Err(LedgerError::ArtifactReferenceAliasConflict {
            digest: artifact.envelope_digest().as_str().to_string(),
        });
    }
    if physical.state != to_physical {
        set_physical_artifact_state_tx(transaction, artifact, to_physical, Some(reason), now)?;
    } else if physical.reason != Some(reason) {
        return Err(LedgerError::ArtifactStateTransitionInvalid {
            event_id: "<physical>".to_string(),
            digest: artifact.envelope_digest().as_str().to_string(),
            from: physical.state.as_str().to_string(),
            to: to_physical.as_str().to_string(),
        });
    }
    let target = reference_state_for_reason(reason)?;
    let references =
        artifact_live_reference_keys_tx(transaction, artifact.envelope_digest().as_str())?;
    references
        .into_iter()
        .map(|(run_id, event_id)| {
            transition_artifact_reference_tx(
                transaction,
                run_id,
                event_id,
                artifact,
                target,
                Some(reason),
                now,
            )
        })
        .collect()
}

fn begin_exact_artifact_retirement_tx(
    transaction: &Transaction<'_>,
    run_id: RunId,
    event_id: EventId,
    artifact: &SealedArtifactRef,
    now: i64,
) -> LedgerResult<ArtifactStateTransition> {
    transition_artifact_reference_tx(
        transaction,
        run_id,
        event_id,
        artifact,
        ArtifactReferenceState::Retiring,
        Some(ArtifactStateReason::RetentionDeleted),
        now,
    )
}

fn finish_exact_artifact_retirement_tx(
    transaction: &Transaction<'_>,
    run_id: RunId,
    event_id: EventId,
    artifact: &SealedArtifactRef,
    now: i64,
) -> LedgerResult<ArtifactStateTransition> {
    let transition = transition_artifact_reference_tx(
        transaction,
        run_id,
        event_id,
        artifact,
        ArtifactReferenceState::Retired,
        Some(ArtifactStateReason::RetentionDeleted),
        now,
    )?;
    if protected_reference_count_tx(transaction, artifact.envelope_digest().as_str())? == 0 {
        let (_, physical) =
            load_physical_artifact(transaction, artifact.envelope_digest().as_str())?;
        if !matches!(
            physical.state,
            ArtifactPhysicalState::Retiring | ArtifactPhysicalState::Retired
        ) {
            set_physical_artifact_state_tx(
                transaction,
                artifact,
                ArtifactPhysicalState::Retiring,
                Some(ArtifactStateReason::RetentionDeleted),
                now,
            )?;
        }
    }
    Ok(transition)
}

fn observe_exact_artifact_unlinked_tx(
    transaction: &Transaction<'_>,
    artifact: &SealedArtifactRef,
    now: i64,
) -> LedgerResult<ArtifactPhysicalState> {
    if protected_reference_count_tx(transaction, artifact.envelope_digest().as_str())? != 0 {
        return Err(LedgerError::IntegrityViolation(
            "cannot retire a physical artifact while a logical reference remains protected"
                .to_string(),
        ));
    }
    let (_, physical) = load_physical_artifact(transaction, artifact.envelope_digest().as_str())?;
    if physical.artifact != *artifact {
        return Err(LedgerError::ArtifactReferenceAliasConflict {
            digest: artifact.envelope_digest().as_str().to_string(),
        });
    }
    match physical.state {
        ArtifactPhysicalState::Retiring => {
            set_physical_artifact_state_tx(
                transaction,
                artifact,
                ArtifactPhysicalState::Retired,
                Some(ArtifactStateReason::RetentionDeleted),
                now,
            )?;
            Ok(ArtifactPhysicalState::Retired)
        }
        ArtifactPhysicalState::Retired => Ok(ArtifactPhysicalState::Retired),
        state => Err(LedgerError::ArtifactStateTransitionInvalid {
            event_id: "<physical>".to_string(),
            digest: artifact.envelope_digest().as_str().to_string(),
            from: state.as_str().to_string(),
            to: ArtifactPhysicalState::Retired.as_str().to_string(),
        }),
    }
}

fn transition_artifact_reference_tx(
    transaction: &Transaction<'_>,
    run_id: RunId,
    event_id: EventId,
    artifact: &SealedArtifactRef,
    to_state: ArtifactReferenceState,
    reason: Option<ArtifactStateReason>,
    now: i64,
) -> LedgerResult<ArtifactStateTransition> {
    validate_state_reason(to_state, reason)?;
    let status = load_artifact_reference_status(transaction, run_id, event_id, artifact)?;
    if status.reference_state == to_state {
        if status.reference_reason != reason {
            return Err(LedgerError::ArtifactStateTransitionInvalid {
                event_id: event_id.to_string(),
                digest: artifact.envelope_digest().as_str().to_string(),
                from: status.reference_state.as_str().to_string(),
                to: to_state.as_str().to_string(),
            });
        }
        return load_existing_artifact_transition(
            transaction,
            run_id,
            event_id,
            artifact,
            to_state,
            reason,
        )?
        .ok_or_else(|| {
            LedgerError::IntegrityViolation(
                "artifact state transition retry lacks its immutable audit record".to_string(),
            )
        });
    }
    if !artifact_reference_transition_allowed(status.reference_state, to_state) {
        return Err(LedgerError::ArtifactStateTransitionInvalid {
            event_id: event_id.to_string(),
            digest: artifact.envelope_digest().as_str().to_string(),
            from: status.reference_state.as_str().to_string(),
            to: to_state.as_str().to_string(),
        });
    }
    let current_sequence = transaction
        .query_row(
            "SELECT transition_sequence FROM session_run_artifact_refs
             WHERE run_id = ?1 AND event_id = ?2 AND envelope_digest = ?3",
            params![
                run_id.to_string(),
                event_id.to_string(),
                artifact.envelope_digest().as_str(),
            ],
            |row| row.get::<_, i64>(0),
        )
        .optional()?
        .ok_or_else(|| LedgerError::ArtifactReferenceNotFound {
            run_id: run_id.to_string(),
            event_id: event_id.to_string(),
            digest: artifact.envelope_digest().as_str().to_string(),
        })?;
    let current_sequence =
        parse_nonnegative_u64("artifact reference transition sequence", current_sequence)?;
    let next_sequence = current_sequence
        .checked_add(1)
        .ok_or(LedgerError::NumericOverflow)?;
    let changed = transaction.execute(
        "UPDATE session_run_artifact_refs
         SET state = ?1, state_reason = ?2, transition_sequence = ?3, state_changed_at_ms = ?4
         WHERE run_id = ?5 AND event_id = ?6 AND envelope_digest = ?7
           AND state = ?8 AND state_reason IS ?9 AND transition_sequence = ?10",
        params![
            to_state.as_str(),
            reason.map(ArtifactStateReason::as_str),
            i64::try_from(next_sequence).map_err(|_| LedgerError::NumericOverflow)?,
            now,
            run_id.to_string(),
            event_id.to_string(),
            artifact.envelope_digest().as_str(),
            status.reference_state.as_str(),
            status.reference_reason.map(ArtifactStateReason::as_str),
            i64::try_from(current_sequence).map_err(|_| LedgerError::NumericOverflow)?,
        ],
    )?;
    if changed != 1 {
        return Err(LedgerError::ArtifactStateTransitionInvalid {
            event_id: event_id.to_string(),
            digest: artifact.envelope_digest().as_str().to_string(),
            from: status.reference_state.as_str().to_string(),
            to: to_state.as_str().to_string(),
        });
    }
    let next_exact = derive_next_exact_replay_state_tx(transaction, run_id)?;
    let audit_event_id = EventId::new();
    let payload = artifact_state_audit_payload(
        audit_event_id,
        event_id,
        artifact,
        status.reference_state,
        to_state,
        reason,
        next_sequence,
        next_exact,
    )?;
    append_event_tx(
        transaction,
        &RunLedgerConfig::default(),
        EventDraft {
            event_id: audit_event_id,
            run_id,
            kind: SessionRunEventKind::ArtifactStateAudit,
            append_key: None,
            effect_intent: EffectIntent::None,
            artifact_degradation_reason: None,
            identity: None,
            sealed_artifact_ref: None,
            run_provenance: None,
            public_payload: payload,
            created_at_ms: now,
        },
    )?;
    update_exact_replay_latch_tx(transaction, run_id, next_exact, audit_event_id)?;
    transaction.execute(
        "INSERT INTO session_run_artifact_transitions (
            event_id, envelope_digest, run_id, transition_sequence, from_state, to_state,
            state_reason, audit_event_id, created_at_ms
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            event_id.to_string(),
            artifact.envelope_digest().as_str(),
            run_id.to_string(),
            i64::try_from(next_sequence).map_err(|_| LedgerError::NumericOverflow)?,
            status.reference_state.as_str(),
            to_state.as_str(),
            reason.map(ArtifactStateReason::as_str),
            audit_event_id.to_string(),
            now,
        ],
    )?;
    Ok(ArtifactStateTransition {
        run_id,
        event_id,
        artifact: artifact.clone(),
        transition_sequence: next_sequence,
        from_state: status.reference_state,
        to_state,
        reason,
        audit_event_id,
        created_at_ms: now,
    })
}
fn artifact_reference_keys_with_state_tx(
    transaction: &Transaction<'_>,
    digest: &str,
    state: ArtifactReferenceState,
) -> LedgerResult<Vec<(RunId, EventId)>> {
    let mut statement = transaction.prepare(
        "SELECT run_id, event_id
         FROM session_run_artifact_refs
         WHERE envelope_digest = ?1 AND state = ?2
         ORDER BY run_id ASC, event_id ASC",
    )?;
    let raw = statement
        .query_map(params![digest, state.as_str()], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    raw.into_iter()
        .map(|(run_id, event_id)| Ok((parse_run_id(run_id)?, parse_event_id(event_id)?)))
        .collect()
}

fn artifact_live_reference_keys_tx(
    transaction: &Transaction<'_>,
    digest: &str,
) -> LedgerResult<Vec<(RunId, EventId)>> {
    let mut statement = transaction.prepare(
        "SELECT run_id, event_id
         FROM session_run_artifact_refs
         WHERE envelope_digest = ?1 AND state IN ('unverified', 'available')
         ORDER BY run_id ASC, event_id ASC",
    )?;
    let raw = statement
        .query_map([digest], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    raw.into_iter()
        .map(|(run_id, event_id)| Ok((parse_run_id(run_id)?, parse_event_id(event_id)?)))
        .collect()
}

fn protected_reference_count_tx(transaction: &Transaction<'_>, digest: &str) -> LedgerResult<u64> {
    let count: i64 = transaction.query_row(
        "SELECT COUNT(*) FROM session_run_artifact_refs
         WHERE envelope_digest = ?1 AND state IN ('unverified', 'available', 'retiring')",
        [digest],
        |row| row.get(0),
    )?;
    parse_nonnegative_u64("protected artifact reference count", count)
}

fn load_existing_artifact_transition(
    transaction: &Transaction<'_>,
    run_id: RunId,
    event_id: EventId,
    artifact: &SealedArtifactRef,
    to_state: ArtifactReferenceState,
    reason: Option<ArtifactStateReason>,
) -> LedgerResult<Option<ArtifactStateTransition>> {
    let raw = transaction
        .query_row(
            "SELECT transition_sequence, from_state, to_state, state_reason, audit_event_id,
                    created_at_ms
             FROM session_run_artifact_transitions
             WHERE run_id = ?1 AND event_id = ?2 AND envelope_digest = ?3
               AND to_state = ?4 AND state_reason IS ?5
             ORDER BY transition_sequence DESC
             LIMIT 1",
            params![
                run_id.to_string(),
                event_id.to_string(),
                artifact.envelope_digest().as_str(),
                to_state.as_str(),
                reason.map(ArtifactStateReason::as_str),
            ],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, i64>(5)?,
                ))
            },
        )
        .optional()?;
    raw.map(|raw| {
        let stored_to = ArtifactReferenceState::from_db(&raw.2)?;
        let stored_reason = raw
            .3
            .as_deref()
            .map(ArtifactStateReason::from_db)
            .transpose()?;
        if stored_to != to_state || stored_reason != reason {
            return Err(LedgerError::IntegrityViolation(
                "artifact transition idempotency lookup returned incompatible state".to_string(),
            ));
        }
        Ok(ArtifactStateTransition {
            run_id,
            event_id,
            artifact: artifact.clone(),
            transition_sequence: parse_nonnegative_u64(
                "artifact reference transition sequence",
                raw.0,
            )?,
            from_state: ArtifactReferenceState::from_db(&raw.1)?,
            to_state: stored_to,
            reason: stored_reason,
            audit_event_id: parse_event_id(raw.4)?,
            created_at_ms: raw.5,
        })
    })
    .transpose()
}

fn set_physical_artifact_state_tx(
    transaction: &Transaction<'_>,
    artifact: &SealedArtifactRef,
    to_state: ArtifactPhysicalState,
    reason: Option<ArtifactStateReason>,
    now: i64,
) -> LedgerResult<ArtifactPhysicalState> {
    validate_physical_state_reason(to_state, reason)?;
    let (_, current) = load_physical_artifact(transaction, artifact.envelope_digest().as_str())?;
    if current.artifact != *artifact {
        return Err(LedgerError::ArtifactReferenceAliasConflict {
            digest: artifact.envelope_digest().as_str().to_string(),
        });
    }
    if current.state == to_state {
        if current.reason != reason {
            return Err(LedgerError::ArtifactStateTransitionInvalid {
                event_id: "<physical>".to_string(),
                digest: artifact.envelope_digest().as_str().to_string(),
                from: current.state.as_str().to_string(),
                to: to_state.as_str().to_string(),
            });
        }
        return Ok(to_state);
    }
    if !artifact_physical_transition_allowed(current.state, to_state) {
        return Err(LedgerError::ArtifactStateTransitionInvalid {
            event_id: "<physical>".to_string(),
            digest: artifact.envelope_digest().as_str().to_string(),
            from: current.state.as_str().to_string(),
            to: to_state.as_str().to_string(),
        });
    }
    let next_sequence = current
        .transition_sequence
        .checked_add(1)
        .ok_or(LedgerError::NumericOverflow)?;
    let tombstoned_at_ms = match to_state {
        ArtifactPhysicalState::Retiring => Some(now),
        ArtifactPhysicalState::Retired => current.tombstoned_at_ms,
        _ => None,
    };
    let retired_at_ms = match to_state {
        ArtifactPhysicalState::Retired => Some(now),
        _ => None,
    };
    let changed = transaction.execute(
        "UPDATE session_run_artifact_objects
         SET state = ?1, state_reason = ?2, transition_sequence = ?3,
             tombstoned_at_ms = ?4, retired_at_ms = ?5
         WHERE envelope_digest = ?6 AND state = ?7 AND state_reason IS ?8
           AND transition_sequence = ?9",
        params![
            to_state.as_str(),
            reason.map(ArtifactStateReason::as_str),
            i64::try_from(next_sequence).map_err(|_| LedgerError::NumericOverflow)?,
            tombstoned_at_ms,
            retired_at_ms,
            artifact.envelope_digest().as_str(),
            current.state.as_str(),
            current.reason.map(ArtifactStateReason::as_str),
            i64::try_from(current.transition_sequence).map_err(|_| LedgerError::NumericOverflow)?,
        ],
    )?;
    if changed != 1 {
        return Err(LedgerError::ArtifactStateTransitionInvalid {
            event_id: "<physical>".to_string(),
            digest: artifact.envelope_digest().as_str().to_string(),
            from: current.state.as_str().to_string(),
            to: to_state.as_str().to_string(),
        });
    }
    Ok(to_state)
}

fn load_exact_replay_status_tx(
    transaction: &Transaction<'_>,
    run_id: RunId,
) -> LedgerResult<ExactReplayStatus> {
    let raw = transaction
        .query_row(
            "SELECT exact_replay, state, state_reason, transition_sequence, latched_at_ms,
                    latched_by_event_id
             FROM session_run_exact_replay WHERE run_id = ?1",
            [run_id.to_string()],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, Option<i64>>(4)?,
                    row.get::<_, Option<String>>(5)?,
                ))
            },
        )
        .optional()?
        .ok_or(LedgerError::RunNotFound {
            run_id: run_id.to_string(),
        })?;
    let state = ExactReplayState::from_db(&raw.1)?;
    let exact_replay = parse_sql_bool(raw.0, "exact replay")?;
    let reason = raw
        .2
        .as_deref()
        .map(ArtifactStateReason::from_db)
        .transpose()?;
    validate_exact_replay_state_reason(state, reason)?;
    if exact_replay != state.exact_replay() {
        return Err(LedgerError::InvalidStoredState {
            kind: "exact replay latch",
            value: raw.1,
        });
    }
    let transition_sequence =
        parse_nonnegative_u64("exact replay transition sequence", raw.3)?;
    let latched_by_event_id = raw.5.map(parse_event_id).transpose()?;
    match (transition_sequence, raw.4, latched_by_event_id) {
        (0, None, None) => {}
        (0, _, _) => {
            return Err(LedgerError::InvalidStoredState {
                kind: "exact replay latch",
                value: "zero transition sequence has latch fields".to_string(),
            });
        }
        (_, Some(latched_at_ms), Some(event_id)) => {
            let latch_event: Option<(String, i64)> = transaction
                .query_row(
                    "SELECT run_id, created_at_ms FROM session_run_events WHERE event_id = ?1",
                    [event_id.to_string()],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?;
            let Some((event_run_id, event_created_at_ms)) = latch_event else {
                return Err(LedgerError::InvalidStoredState {
                    kind: "exact replay latch",
                    value: "latched event belongs to another run or is absent".to_string(),
                });
            };
            if event_run_id != run_id.to_string() {
                return Err(LedgerError::InvalidStoredState {
                    kind: "exact replay latch",
                    value: "latched event belongs to another run or is absent".to_string(),
                });
            }
            if event_created_at_ms != latched_at_ms {
                return Err(LedgerError::InvalidStoredState {
                    kind: "exact replay latch",
                    value: "latched timestamp does not match its event".to_string(),
                });
            }
        }
        (_, _, _) => {
            return Err(LedgerError::InvalidStoredState {
                kind: "exact replay latch",
                value: "nonzero transition sequence has partial latch fields".to_string(),
            });
        }
    }
    Ok(ExactReplayStatus {
        state,
        reason,
        exact_replay,
        transition_sequence,
        latched_at_ms: raw.4,
        latched_by_event_id,
    })
}

fn derive_next_exact_replay_state_tx(
    transaction: &Transaction<'_>,
    run_id: RunId,
) -> LedgerResult<ExactReplayState> {
    let current = load_exact_replay_status_tx(transaction, run_id)?;
    if !matches!(
        current.state,
        ExactReplayState::Unverified | ExactReplayState::Available
    ) {
        return Ok(current.state);
    }
    let degraded_fidelity: Option<String> = transaction
        .query_row(
            "SELECT replay_fidelity FROM session_run_replay_obligations
             WHERE run_id = ?1 AND replay_fidelity IN ('metadata_only', 'unavailable')
             ORDER BY source_event_id ASC, operation_kind ASC, phase ASC LIMIT 1",
            [run_id.to_string()],
            |row| row.get(0),
        )
        .optional()?;
    if degraded_fidelity.is_some() {
        return Ok(ExactReplayState::UnavailableSanitized);
    }
    let pending: Option<i64> = transaction
        .query_row(
            "SELECT 1 FROM session_run_replay_obligations
             WHERE run_id = ?1 AND replay_fidelity = 'pending' LIMIT 1",
            [run_id.to_string()],
            |row| row.get(0),
        )
        .optional()?;
    if pending.is_some() {
        return Ok(ExactReplayState::Unverified);
    }
    let state = transaction
        .query_row(
            "SELECT state FROM session_run_artifact_refs
             WHERE run_id = ?1 AND state <> 'available'
             ORDER BY CASE state
                WHEN 'retired' THEN 0
                WHEN 'retiring' THEN 1
                WHEN 'unavailable_missing' THEN 2
                WHEN 'unavailable_corrupt' THEN 3
                WHEN 'unavailable_key_revoked' THEN 4
                WHEN 'unverified' THEN 5
                ELSE 6
             END
             LIMIT 1",
            [run_id.to_string()],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    match state {
        Some(state) => ExactReplayState::from_db(&state),
        None => Ok(ExactReplayState::Available),
    }
}

fn update_exact_replay_latch_tx(
    transaction: &Transaction<'_>,
    run_id: RunId,
    to_state: ExactReplayState,
    audit_event_id: EventId,
) -> LedgerResult<ExactReplayStatus> {
    let current = load_exact_replay_status_tx(transaction, run_id)?;
    if current.state == to_state {
        return Ok(current);
    }
    if !exact_replay_transition_allowed(current.state, to_state) {
        return Err(LedgerError::ArtifactStateTransitionInvalid {
            event_id: format!("run:{run_id}"),
            digest: "<exact-replay>".to_string(),
            from: current.state.as_str().to_string(),
            to: to_state.as_str().to_string(),
        });
    }
    let latched_at_ms = transaction
        .query_row(
            "SELECT created_at_ms FROM session_run_events
             WHERE event_id = ?1 AND run_id = ?2",
            params![audit_event_id.to_string(), run_id.to_string()],
            |row| row.get::<_, i64>(0),
        )
        .optional()?
        .ok_or_else(|| {
            LedgerError::IntegrityViolation(
                "exact replay latch event must belong to the run".to_string(),
            )
        })?;
    let next_sequence = current
        .transition_sequence
        .checked_add(1)
        .ok_or(LedgerError::NumericOverflow)?;
    let reason = exact_replay_reason(to_state);
    let changed = transaction.execute(
        "UPDATE session_run_exact_replay
         SET exact_replay = ?1, state = ?2, state_reason = ?3, transition_sequence = ?4,
             latched_at_ms = ?5, latched_by_event_id = ?6
         WHERE run_id = ?7 AND state = ?8 AND transition_sequence = ?9",
        params![
            i64::from(to_state.exact_replay()),
            to_state.as_str(),
            reason.map(ArtifactStateReason::as_str),
            i64::try_from(next_sequence).map_err(|_| LedgerError::NumericOverflow)?,
            latched_at_ms,
            audit_event_id.to_string(),
            run_id.to_string(),
            current.state.as_str(),
            i64::try_from(current.transition_sequence).map_err(|_| LedgerError::NumericOverflow)?,
        ],
    )?;
    if changed != 1 {
        return Err(LedgerError::ArtifactStateTransitionInvalid {
            event_id: format!("run:{run_id}"),
            digest: "<exact-replay>".to_string(),
            from: current.state.as_str().to_string(),
            to: to_state.as_str().to_string(),
        });
    }
    Ok(ExactReplayStatus {
        state: to_state,
        reason,
        exact_replay: to_state.exact_replay(),
        transition_sequence: next_sequence,
        latched_at_ms: Some(latched_at_ms),
        latched_by_event_id: Some(audit_event_id),
    })
}
fn physical_state_for_reason(reason: ArtifactStateReason) -> LedgerResult<ArtifactPhysicalState> {
    match reason {
        ArtifactStateReason::Missing => Ok(ArtifactPhysicalState::UnavailableMissing),
        ArtifactStateReason::Corrupt => Ok(ArtifactPhysicalState::UnavailableCorrupt),
        ArtifactStateReason::KeyRevoked => Ok(ArtifactPhysicalState::UnavailableKeyRevoked),
        ArtifactStateReason::RetentionDeleted => Ok(ArtifactPhysicalState::Retiring),
        ArtifactStateReason::Sanitized => Err(LedgerError::IntegrityViolation(
            "sanitized replay fidelity is a run-level latch reason, not an artifact state"
                .to_string(),
        )),
    }
}

fn reference_state_for_reason(
    reason: ArtifactStateReason,
) -> LedgerResult<ArtifactReferenceState> {
    match reason {
        ArtifactStateReason::Missing => Ok(ArtifactReferenceState::UnavailableMissing),
        ArtifactStateReason::Corrupt => Ok(ArtifactReferenceState::UnavailableCorrupt),
        ArtifactStateReason::KeyRevoked => Ok(ArtifactReferenceState::UnavailableKeyRevoked),
        ArtifactStateReason::RetentionDeleted => Ok(ArtifactReferenceState::Retiring),
        ArtifactStateReason::Sanitized => Err(LedgerError::IntegrityViolation(
            "sanitized replay fidelity is a run-level latch reason, not an artifact state"
                .to_string(),
        )),
    }
}

fn exact_replay_reason(state: ExactReplayState) -> Option<ArtifactStateReason> {
    match state {
        ExactReplayState::UnavailableMissing => Some(ArtifactStateReason::Missing),
        ExactReplayState::UnavailableCorrupt => Some(ArtifactStateReason::Corrupt),
        ExactReplayState::UnavailableKeyRevoked => Some(ArtifactStateReason::KeyRevoked),
        ExactReplayState::UnavailableSanitized => Some(ArtifactStateReason::Sanitized),
        ExactReplayState::Retiring | ExactReplayState::Retired => {
            Some(ArtifactStateReason::RetentionDeleted)
        }
        ExactReplayState::Unverified | ExactReplayState::Available => None,
    }
}

fn expected_reference_reason(state: ArtifactReferenceState) -> Option<ArtifactStateReason> {
    match state {
        ArtifactReferenceState::UnavailableMissing => Some(ArtifactStateReason::Missing),
        ArtifactReferenceState::UnavailableCorrupt => Some(ArtifactStateReason::Corrupt),
        ArtifactReferenceState::UnavailableKeyRevoked => Some(ArtifactStateReason::KeyRevoked),
        ArtifactReferenceState::Retiring | ArtifactReferenceState::Retired => {
            Some(ArtifactStateReason::RetentionDeleted)
        }
        ArtifactReferenceState::Unverified | ArtifactReferenceState::Available => None,
    }
}

fn expected_physical_reason(state: ArtifactPhysicalState) -> Option<ArtifactStateReason> {
    match state {
        ArtifactPhysicalState::UnavailableMissing => Some(ArtifactStateReason::Missing),
        ArtifactPhysicalState::UnavailableCorrupt => Some(ArtifactStateReason::Corrupt),
        ArtifactPhysicalState::UnavailableKeyRevoked => Some(ArtifactStateReason::KeyRevoked),
        ArtifactPhysicalState::Retiring | ArtifactPhysicalState::Retired => {
            Some(ArtifactStateReason::RetentionDeleted)
        }
        ArtifactPhysicalState::Unverified | ArtifactPhysicalState::Available => None,
    }
}

fn validate_state_reason(
    state: ArtifactReferenceState,
    reason: Option<ArtifactStateReason>,
) -> LedgerResult<()> {
    if expected_reference_reason(state) != reason {
        return Err(LedgerError::IntegrityViolation(
            "artifact reference state and reason are not a closed pair".to_string(),
        ));
    }
    Ok(())
}

fn validate_physical_state_reason(
    state: ArtifactPhysicalState,
    reason: Option<ArtifactStateReason>,
) -> LedgerResult<()> {
    if expected_physical_reason(state) != reason {
        return Err(LedgerError::IntegrityViolation(
            "artifact physical state and reason are not a closed pair".to_string(),
        ));
    }
    Ok(())
}

fn validate_exact_replay_state_reason(
    state: ExactReplayState,
    reason: Option<ArtifactStateReason>,
) -> LedgerResult<()> {
    if exact_replay_reason(state) != reason {
        return Err(LedgerError::InvalidStoredState {
            kind: "exact replay state reason",
            value: format!("{} / {:?}", state.as_str(), reason),
        });
    }
    Ok(())
}

fn validate_physical_state_record(record: &PhysicalArtifactRecord) -> LedgerResult<()> {
    validate_physical_state_reason(record.state, record.reason)?;
    let valid_times = match record.state {
        ArtifactPhysicalState::Unverified
        | ArtifactPhysicalState::Available
        | ArtifactPhysicalState::UnavailableMissing
        | ArtifactPhysicalState::UnavailableCorrupt
        | ArtifactPhysicalState::UnavailableKeyRevoked => {
            record.tombstoned_at_ms.is_none() && record.retired_at_ms.is_none()
        }
        ArtifactPhysicalState::Retiring => {
            record.tombstoned_at_ms.is_some() && record.retired_at_ms.is_none()
        }
        ArtifactPhysicalState::Retired => {
            record.tombstoned_at_ms.is_some() && record.retired_at_ms.is_some()
        }
    };
    if !valid_times {
        return Err(LedgerError::InvalidStoredState {
            kind: "artifact physical lifecycle timestamps",
            value: record.state.as_str().to_string(),
        });
    }
    Ok(())
}

fn validate_artifact_status(status: &ArtifactReferenceStatus) -> LedgerResult<()> {
    validate_physical_state_reason(status.physical_state, status.physical_reason)?;
    validate_state_reason(status.reference_state, status.reference_reason)?;
    validate_exact_replay_state_reason(status.exact_replay.state, status.exact_replay.reason)?;
    if status.exact_replay.exact_replay != status.exact_replay.state.exact_replay() {
        return Err(LedgerError::InvalidStoredState {
            kind: "exact replay latch",
            value: status.exact_replay.state.as_str().to_string(),
        });
    }
    Ok(())
}

fn artifact_reference_transition_allowed(
    from: ArtifactReferenceState,
    to: ArtifactReferenceState,
) -> bool {
    match from {
        ArtifactReferenceState::Unverified => matches!(
            to,
            ArtifactReferenceState::Available
                | ArtifactReferenceState::UnavailableMissing
                | ArtifactReferenceState::UnavailableCorrupt
                | ArtifactReferenceState::UnavailableKeyRevoked
                | ArtifactReferenceState::Retiring
        ),
        ArtifactReferenceState::Available => matches!(
            to,
            ArtifactReferenceState::UnavailableMissing
                | ArtifactReferenceState::UnavailableCorrupt
                | ArtifactReferenceState::UnavailableKeyRevoked
                | ArtifactReferenceState::Retiring
        ),
        ArtifactReferenceState::UnavailableMissing
        | ArtifactReferenceState::UnavailableCorrupt
        | ArtifactReferenceState::UnavailableKeyRevoked => {
            matches!(to, ArtifactReferenceState::Retiring)
        }
        ArtifactReferenceState::Retiring => matches!(to, ArtifactReferenceState::Retired),
        ArtifactReferenceState::Retired => false,
    }
}

fn artifact_physical_transition_allowed(
    from: ArtifactPhysicalState,
    to: ArtifactPhysicalState,
) -> bool {
    match from {
        ArtifactPhysicalState::Unverified => matches!(
            to,
            ArtifactPhysicalState::Available
                | ArtifactPhysicalState::UnavailableMissing
                | ArtifactPhysicalState::UnavailableCorrupt
                | ArtifactPhysicalState::UnavailableKeyRevoked
                | ArtifactPhysicalState::Retiring
        ),
        ArtifactPhysicalState::Available => matches!(
            to,
            ArtifactPhysicalState::UnavailableMissing
                | ArtifactPhysicalState::UnavailableCorrupt
                | ArtifactPhysicalState::UnavailableKeyRevoked
                | ArtifactPhysicalState::Retiring
        ),
        ArtifactPhysicalState::UnavailableMissing
        | ArtifactPhysicalState::UnavailableCorrupt
        | ArtifactPhysicalState::UnavailableKeyRevoked => {
            matches!(to, ArtifactPhysicalState::Retiring)
        }
        ArtifactPhysicalState::Retiring => matches!(to, ArtifactPhysicalState::Retired),
        ArtifactPhysicalState::Retired => false,
    }
}

fn exact_replay_transition_allowed(from: ExactReplayState, to: ExactReplayState) -> bool {
    match from {
        ExactReplayState::Unverified => !matches!(to, ExactReplayState::Unverified),
        ExactReplayState::Available => !matches!(to, ExactReplayState::Available),
        ExactReplayState::Retiring => matches!(to, ExactReplayState::Retired),
        ExactReplayState::UnavailableMissing
        | ExactReplayState::UnavailableCorrupt
        | ExactReplayState::UnavailableKeyRevoked
        | ExactReplayState::UnavailableSanitized
        | ExactReplayState::Retired => false,
    }
}
fn artifact_state_audit_payload(
    audit_event_id: EventId,
    affected_event_id: EventId,
    artifact: &SealedArtifactRef,
    from_state: ArtifactReferenceState,
    to_state: ArtifactReferenceState,
    reason: Option<ArtifactStateReason>,
    transition_sequence: u64,
    exact_replay_state: ExactReplayState,
) -> LedgerResult<CanonicalJson> {
    CanonicalJson::from_value(&serde_json::json!({
        "schema": "hermes.artifact-state-audit/v1",
        "schema_version": 1,
        "audit_event_id": audit_event_id.to_string(),
        "affected_event_id": affected_event_id.to_string(),
        "artifact": {
            "envelope_digest": artifact.envelope_digest().as_str(),
            "key_id": artifact.key_id().as_str(),
            "schema_version": artifact.schema_version(),
            "encryption_algorithm": artifact.encryption_algorithm(),
            "canonicalization_algorithm": artifact.canonicalization_algorithm(),
            "canonicalization_version": artifact.canonicalization_version(),
            "redactor_version": artifact.redactor_version().as_str(),
        },
        "from_state": from_state.as_str(),
        "to_state": to_state.as_str(),
        "reason": reason.map(ArtifactStateReason::as_str),
        "transition_sequence": transition_sequence,
        "exact_replay_state": exact_replay_state.as_str(),
        "exact_replay": exact_replay_state.exact_replay(),
    }))
}

fn validate_artifact_state_audit_payload(payload: &CanonicalJson) -> LedgerResult<()> {
    let value = payload.as_value()?;
    let object = value.as_object().ok_or_else(|| {
        LedgerError::IntegrityViolation(
            "artifact state audit payload must be an object".to_string(),
        )
    })?;
    if object.get("schema").and_then(Value::as_str) != Some("hermes.artifact-state-audit/v1") {
        return Err(LedgerError::IntegrityViolation(
            "artifact state audit payload has an unknown schema".to_string(),
        ));
    }
    if object.get("schema_version").and_then(Value::as_u64) != Some(1) {
        return Err(LedgerError::IntegrityViolation(
            "artifact state audit payload has an unsupported schema version".to_string(),
        ));
    }
    let audit_event_id = object
        .get("audit_event_id")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            LedgerError::IntegrityViolation(
                "artifact state audit has no audit event identity".to_string(),
            )
        })?;
    parse_event_id(audit_event_id.to_string())?;
    let affected_event_id = object
        .get("affected_event_id")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            LedgerError::IntegrityViolation(
                "artifact state audit has no affected event identity".to_string(),
            )
        })?;
    parse_event_id(affected_event_id.to_string())?;
    let from_state = object
        .get("from_state")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            LedgerError::IntegrityViolation("artifact state audit has no source state".to_string())
        })?;
    ArtifactReferenceState::from_db(from_state)?;
    let to_state = object
        .get("to_state")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            LedgerError::IntegrityViolation("artifact state audit has no target state".to_string())
        })?;
    let to_state = ArtifactReferenceState::from_db(to_state)?;
    let reason = match object.get("reason") {
        Some(Value::String(reason)) => Some(ArtifactStateReason::from_db(reason)?),
        Some(Value::Null) | None => None,
        _ => {
            return Err(LedgerError::IntegrityViolation(
                "artifact state audit reason is malformed".to_string(),
            ));
        }
    };
    validate_state_reason(to_state, reason)?;
    let exact_state = object
        .get("exact_replay_state")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            LedgerError::IntegrityViolation(
                "artifact state audit has no exact replay state".to_string(),
            )
        })?;
    let exact_state = ExactReplayState::from_db(exact_state)?;
    if object.get("exact_replay").and_then(Value::as_bool) != Some(exact_state.exact_replay()) {
        return Err(LedgerError::IntegrityViolation(
            "artifact state audit exact replay flag conflicts with state".to_string(),
        ));
    }
    Ok(())
}

fn parse_sql_bool(value: i64, kind: &'static str) -> LedgerResult<bool> {
    match value {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(LedgerError::InvalidStoredState {
            kind,
            value: value.to_string(),
        }),
    }
}

fn event_from_draft(
    sequence: u64,
    previous_hash: String,
    manifest_hash: &str,
    draft: &EventDraft,
) -> LedgerResult<SessionRunEvent> {
    let mut event = SessionRunEvent {
        event_id: draft.event_id,
        run_id: draft.run_id,
        sequence,
        kind: draft.kind,
        append_key: draft.append_key.clone(),
        effect_intent: draft.effect_intent,
        artifact_degradation_reason: draft.artifact_degradation_reason,
        public_identity: draft
            .identity
            .as_ref()
            .map(|identity| identity.public_identity.clone()),
        private_fingerprint: draft
            .identity
            .as_ref()
            .map(|identity| identity.private_fingerprint.clone()),
        sealed_artifact_ref: draft.sealed_artifact_ref.clone(),
        run_provenance: draft.run_provenance.clone(),
        public_payload: draft.public_payload.clone(),
        manifest_hash: manifest_hash.to_string(),
        previous_hash,
        event_hash: String::new(),
        created_at_ms: draft.created_at_ms,
    };
    event.event_hash = recompute_event_hash(&event)?;
    Ok(event)
}

pub(crate) fn recompute_event_hash(event: &SessionRunEvent) -> LedgerResult<String> {
    #[derive(Serialize)]
    struct EventHashBody<'a> {
        canonicalization_algorithm: &'static str,
        canonicalization_version: u8,
        event_id: String,
        run_id: String,
        event_sequence: u64,
        event_kind: &'static str,
        append_key: Option<&'a str>,
        effect_intent: &'static str,
        artifact_degradation_reason: Option<&'static str>,
        public_identity_jcs: Option<&'a str>,
        private_fingerprint_algorithm: Option<&'static str>,
        private_fingerprint_algorithm_version: Option<u8>,
        private_fingerprint_key_id: Option<&'a str>,
        private_fingerprint_key_version: Option<u32>,
        private_fingerprint: Option<String>,
        sealed_artifact_ref: Option<&'a SealedArtifactRef>,
        run_provenance_jcs: Option<&'a str>,
        public_payload_jcs: &'a str,
        manifest_hash: &'a str,
        previous_hash: &'a str,
        created_at_ms: i64,
    }

    let private_fingerprint_algorithm = event
        .private_fingerprint
        .as_ref()
        .map(|fingerprint| fingerprint.algorithm().as_str());
    let private_fingerprint_algorithm_version = event
        .private_fingerprint
        .as_ref()
        .map(PrivateFingerprint::algorithm_version);
    let private_fingerprint_key_id = event
        .private_fingerprint
        .as_ref()
        .map(|fingerprint| fingerprint.key_id().as_str());
    let private_fingerprint_key_version = event
        .private_fingerprint
        .as_ref()
        .map(PrivateFingerprint::key_version);
    let private_fingerprint = event
        .private_fingerprint
        .as_ref()
        .map(PrivateFingerprint::as_hex);
    let body = EventHashBody {
        canonicalization_algorithm: CANONICALIZATION_ALGORITHM,
        canonicalization_version: CANONICALIZATION_VERSION,
        event_id: event.event_id.to_string(),
        run_id: event.run_id.to_string(),
        event_sequence: event.sequence,
        event_kind: event.kind.as_str(),
        append_key: event.append_key.as_ref().map(AppendKey::as_str),
        effect_intent: event.effect_intent.as_str(),
        artifact_degradation_reason: event
            .artifact_degradation_reason
            .map(ArtifactDegradationReason::as_str),
        public_identity_jcs: event.public_identity.as_ref().map(CanonicalJson::as_str),
        private_fingerprint_algorithm,
        private_fingerprint_algorithm_version,
        private_fingerprint_key_id,
        private_fingerprint_key_version,
        private_fingerprint,
        sealed_artifact_ref: event.sealed_artifact_ref.as_ref(),
        run_provenance_jcs: event.run_provenance.as_ref().map(CanonicalJson::as_str),
        public_payload_jcs: event.public_payload.as_str(),
        manifest_hash: &event.manifest_hash,
        previous_hash: &event.previous_hash,
        created_at_ms: event.created_at_ms,
    };
    let canonical = CanonicalJson::from_value(&body)?;
    Ok(sha256_hex(canonical.as_str().as_bytes()))
}

pub(crate) fn genesis_hash() -> String {
    sha256_hex(CHAIN_GENESIS_DOMAIN)
}

pub(crate) fn derive_nonresumable_idempotency_key(
    event_id: EventId,
) -> LedgerResult<NonResumableIdempotencyKey> {
    let mut hasher = Sha256::new();
    hasher.update(NONRESUMABLE_EFFECT_IDEMPOTENCY_DOMAIN);
    hasher.update(event_id.as_uuid().as_bytes());
    NonResumableIdempotencyKey::from_sha256_hex(hex::encode(hasher.finalize()))
}

pub(crate) fn derive_nonresumable_claim_binding(token: &LeaseToken) -> LedgerResult<Sha256Digest> {
    let mut hasher = Sha256::new();
    hasher.update(NONRESUMABLE_CLAIM_BINDING_DOMAIN);
    hasher.update(token.as_bytes());
    Sha256Digest::new(hex::encode(hasher.finalize()))
}

pub(crate) fn derive_nonresumable_result_append_key(
    event_id: EventId,
    claim_binding: &Sha256Digest,
) -> LedgerResult<AppendKey> {
    let mut claim_binding_bytes = [0_u8; 32];
    hex::decode_to_slice(claim_binding.as_str(), &mut claim_binding_bytes).map_err(|_| {
        LedgerError::IntegrityViolation(
            "non-resumable claim binding is not a SHA-256 digest".to_string(),
        )
    })?;
    let mut hasher = Sha256::new();
    hasher.update(NONRESUMABLE_RESULT_APPEND_KEY_DOMAIN);
    hasher.update(event_id.as_uuid().as_bytes());
    hasher.update(claim_binding_bytes);
    AppendKey::new(hex::encode(hasher.finalize()))
}

fn bound_run_start_payload(
    mode: ResolvedExecutionMode,
    owner_id: &RunOwnerId,
    started_at_ms: i64,
    payload: CanonicalJson,
) -> LedgerResult<CanonicalJson> {
    CanonicalJson::from_value(&json!({
        "schema": RUN_START_BINDING_SCHEMA,
        "schema_version": RUN_EVENT_BINDING_SCHEMA_VERSION,
        "execution_mode": mode,
        "owner_id": owner_id.as_str(),
        "started_at_ms": started_at_ms,
        "payload": payload.as_value()?,
    }))
}

fn bound_run_exit_payload(
    terminal_reason: TerminalReason,
    payload: CanonicalJson,
) -> LedgerResult<CanonicalJson> {
    CanonicalJson::from_value(&json!({
        "schema": RUN_EXIT_BINDING_SCHEMA,
        "schema_version": RUN_EVENT_BINDING_SCHEMA_VERSION,
        "terminal_reason": terminal_reason.as_str(),
        "payload": payload.as_value()?,
    }))
}

fn manual_artifact_degradation_payload(
    degradation: &TerminalArtifactDegradation,
) -> LedgerResult<CanonicalJson> {
    CanonicalJson::from_value(&json!({
        "artifact_degradation_reason": degradation.reason.as_str(),
        "public_context": degradation.public_context.as_value()?,
    }))
}

fn terminal_artifact_degradation(
    source_append_key: AppendKey,
    reason: ArtifactDegradationReason,
    effect_intent: EffectIntent,
    source_public_payload: CanonicalJson,
) -> LedgerResult<TerminalArtifactDegradation> {
    let audit_key = derived_manual_degradation_audit_key(&source_append_key)?;
    let public_context = CanonicalJson::from_value(&json!({
        "schema": TERMINAL_ARTIFACT_DEGRADATION_SCHEMA,
        "schema_version": RUN_EVENT_BINDING_SCHEMA_VERSION,
        "audit_append_key": audit_key.as_str(),
        "source_append_key": source_append_key.as_str(),
        "source_effect_intent": effect_intent.as_str(),
        "source_public_payload": source_public_payload.as_value()?,
    }))?;
    Ok(TerminalArtifactDegradation {
        audit_key,
        reason,
        public_context,
    })
}

fn derived_manual_degradation_audit_key(source: &AppendKey) -> LedgerResult<AppendKey> {
    let mut hasher = Sha256::new();
    hasher.update(b"hermes.session-run.manual-degradation-key/v1");
    hasher.update([0]);
    hasher.update(source.as_str().as_bytes());
    AppendKey::new(format!(
        "manual-degradation-{}",
        hex::encode(hasher.finalize())
    ))
}

fn validate_execution_certification(
    mode: ResolvedExecutionMode,
    provenance: &RunProvenance,
    certification: &RunExecutionCertification,
) -> LedgerResult<()> {
    match (mode.mode, certification) {
        (ExecutionMode::Manual, RunExecutionCertification::ManualHost) => {
            if matches!(
                provenance.sandbox_digest(),
                ProvenanceDigest::Absent(DigestAbsence::NotApplicable)
            ) {
                Ok(())
            } else {
                Err(LedgerError::InvalidExecutionCertification {
                    reason: "manual mode must record sandbox digest as not_applicable",
                })
            }
        }
        (
            ExecutionMode::Manual,
            RunExecutionCertification::CertifiedBackendConfigured {
                mode: certified_mode,
                ..
            },
        ) => Err(LedgerError::ExecutionCertificationModeMismatch {
            expected: ExecutionMode::Manual.as_str(),
            received: certified_mode.mode.as_str(),
        }),
        (closed_mode, RunExecutionCertification::ManualHost) => {
            Err(LedgerError::MissingExecutionCertification {
                mode: closed_mode.as_str(),
            })
        }
        (
            closed_mode,
            RunExecutionCertification::CertifiedBackendConfigured {
                mode: certified_mode,
                backend_kind,
                scope,
                policy_version,
                configuration_digest,
            },
        ) => {
            if *certified_mode != mode {
                return Err(LedgerError::ExecutionCertificationModeMismatch {
                    expected: mode.mode.as_str(),
                    received: certified_mode.mode.as_str(),
                });
            }
            if *backend_kind != VerifiedIsolatedBackendKind::Docker
                || *scope != VerifiedIsolatedBackendScope::ClosedExecution
                || *policy_version != CLOSED_EXECUTION_POLICY_VERSION
            {
                return Err(LedgerError::InvalidExecutionCertification {
                    reason: "closed mode requires the current Docker closed-execution policy",
                });
            }
            match provenance.sandbox_digest() {
                ProvenanceDigest::Present(digest) if digest == configuration_digest => Ok(()),
                _ => Err(LedgerError::InvalidExecutionCertification {
                    reason: "closed mode sandbox digest must match certified backend configuration",
                }),
            }
        }
    }
}

