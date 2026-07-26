fn ensure_mode_matches(
    connection: &rusqlite::Connection,
    run_id: RunId,
    supplied: ResolvedExecutionMode,
) -> LedgerResult<()> {
    let (mode, source): (String, String) = connection
        .query_row(
            "SELECT execution_mode, execution_mode_source FROM session_runs WHERE run_id = ?1",
            [run_id.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(|error| match error {
            rusqlite::Error::QueryReturnedNoRows => LedgerError::RunNotFound {
                run_id: run_id.to_string(),
            },
            other => LedgerError::Sqlite(other),
        })?;
    if execution_mode_from_db(&mode, &source)? != supplied {
        return Err(LedgerError::ExecutionModeMismatch {
            run_id: run_id.to_string(),
        });
    }
    Ok(())
}

fn ensure_current_unexpired_run_owner_lease(
    connection: &rusqlite::Connection,
    lease: &RunOwnerLease,
    now: i64,
) -> LedgerResult<bool> {
    let raw: Option<(String, i64, Vec<u8>, i64, Option<String>)> = connection
        .query_row(
            "SELECT owner_id, owner_lease_generation, owner_lease_token,
                    owner_lease_expires_at_ms, exit_event_id
             FROM session_runs WHERE run_id = ?1",
            [lease.run_id.to_string()],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .optional()?;
    let (owner_id, lease_generation, token, lease_expires_at_ms, exit_event_id) =
        raw.ok_or_else(|| LedgerError::RunNotFound {
            run_id: lease.run_id.to_string(),
        })?;
    if RunOwnerId::new(owner_id)? != lease.owner_id
        || parse_positive_u64("run owner lease generation", lease_generation)?
            != lease.lease_generation
        || LeaseToken::from_blob(token)? != lease.token
        || lease_expires_at_ms <= now
    {
        return Err(LedgerError::StaleRunOwnerLease {
            run_id: lease.run_id.to_string(),
        });
    }
    Ok(exit_event_id.is_none())
}

fn branch_replay_child_lineage_error(
    source: &BranchReplaySourceSnapshot,
    reason: &'static str,
) -> LedgerError {
    LedgerError::BranchReplayChildLineage {
        source_run_id: source.source_run_id().to_string(),
        reason,
    }
}

fn branch_replay_source_mismatch(
    source: &BranchReplaySourceSnapshot,
    reason: &'static str,
) -> LedgerError {
    LedgerError::BranchReplaySourceMismatch {
        source_run_id: source.source_run_id().to_string(),
        reason,
    }
}

fn validate_branch_replay_child_lineage(
    source: &BranchReplaySourceSnapshot,
    start: &RunStart,
) -> LedgerResult<()> {
    let provenance = &start.provenance;
    if start.run_id == source.source_run_id() {
        return Err(branch_replay_child_lineage_error(
            source,
            "child run id must differ from the source run id",
        ));
    }
    if provenance.parent_run_id() != Some(source.source_run_id()) {
        return Err(branch_replay_child_lineage_error(
            source,
            "child parent run id does not bind the verified source run",
        ));
    }
    if provenance.session_id() != source.source_session_id() {
        return Err(branch_replay_child_lineage_error(
            source,
            "child session id does not preserve source lineage",
        ));
    }
    if provenance.tenant_id() != source.source_tenant_id() {
        return Err(branch_replay_child_lineage_error(
            source,
            "child tenant differs from the verified source tenant",
        ));
    }
    if provenance.branch_id().as_str() == "main" {
        return Err(branch_replay_child_lineage_error(
            source,
            "branch replay child must use a non-main branch id",
        ));
    }
    if provenance.branch_id() == source.source_branch_id() {
        return Err(branch_replay_child_lineage_error(
            source,
            "branch replay child must use a fresh branch id",
        ));
    }
    Ok(())
}

fn ensure_branch_replay_source_snapshot_tx(
    transaction: &Transaction<'_>,
    source: &BranchReplaySourceSnapshot,
) -> LedgerResult<()> {
    let raw: Option<(
        String,
        String,
        String,
        Option<String>,
        i64,
        String,
        String,
        i64,
    )> = transaction
        .query_row(
            "SELECT s.session_id, s.tenant_id, s.branch_id, s.exit_event_id,
                    s.next_event_sequence, s.head_hash,
                    x.state, x.transition_sequence
             FROM session_runs AS s
             JOIN session_run_exact_replay AS x ON x.run_id = s.run_id
             WHERE s.run_id = ?1",
            [source.source_run_id().to_string()],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                ))
            },
        )
        .optional()?;
    let Some((
        session_id,
        tenant_id,
        branch_id,
        exit_event_id,
        next_event_sequence,
        head_hash,
        exact_replay_state,
        exact_replay_transition_sequence,
    )) = raw
    else {
        return Err(branch_replay_source_mismatch(source, "source run no longer exists"));
    };
    if exit_event_id.is_none() {
        return Err(branch_replay_source_mismatch(
            source,
            "source run is not terminal",
        ));
    }
    if session_id != source.source_session_id().as_str()
        || tenant_id != source.source_tenant_id().as_str()
        || branch_id != source.source_branch_id().as_str()
    {
        return Err(branch_replay_source_mismatch(
            source,
            "source lineage no longer matches the verified export",
        ));
    }
    if parse_nonnegative_u64("branch replay source head sequence", next_event_sequence)?
        != source.source_head_sequence()
        || head_hash != source.source_head_hash().as_str()
        || exact_replay_state != source.source_exact_replay_state().as_str()
        || parse_nonnegative_u64(
            "branch replay source exact-replay transition sequence",
            exact_replay_transition_sequence,
        )? != source.source_exact_replay_transition_sequence()
    {
        return Err(branch_replay_source_mismatch(
            source,
            "source head or exact-replay state changed after verification",
        ));
    }

    let events = load_events_for_run(transaction, source.source_run_id())?;
    verify_event_chain_for_run(transaction, source.source_run_id(), &events)?;
    let head = events.last().ok_or_else(|| {
        branch_replay_source_mismatch(source, "source run has no committed events")
    })?;
    if head.event_id != source.source_head_event_id()
        || head.sequence != source.source_head_sequence()
        || head.event_hash != source.source_head_hash().as_str()
    {
        return Err(branch_replay_source_mismatch(
            source,
            "source head event no longer matches the verified export",
        ));
    }
    let boundary = events
        .iter()
        .find(|event| event.event_id == source.boundary_event_id())
        .ok_or_else(|| {
            branch_replay_source_mismatch(source, "source boundary event no longer exists")
        })?;
    if boundary.sequence != source.boundary_event_sequence()
        || boundary.event_hash != source.boundary_event_hash().as_str()
    {
        return Err(branch_replay_source_mismatch(
            source,
            "source boundary event changed after verification",
        ));
    }

    for event in events
        .iter()
        .filter(|event| event.sequence <= source.boundary_event_sequence())
    {
        let Some(artifact) = event.sealed_artifact_ref.as_ref() else {
            continue;
        };
        if !source.artifacts().iter().any(|binding| {
            binding.source_event_id() == event.event_id && binding.artifact() == artifact
        }) {
            return Err(branch_replay_source_mismatch(
                source,
                "verified source artifact set is incomplete at the replay boundary",
            ));
        }
    }
    for binding in source.artifacts() {
        let event = events
            .iter()
            .find(|event| event.event_id == binding.source_event_id())
            .ok_or_else(|| {
                branch_replay_source_mismatch(
                    source,
                    "verified source artifact event no longer exists",
                )
            })?;
        if event.sequence > source.boundary_event_sequence()
            || event.sealed_artifact_ref.as_ref() != Some(binding.artifact())
        {
            return Err(branch_replay_source_mismatch(
                source,
                "verified source artifact reference changed after verification",
            ));
        }
        let status = match load_artifact_reference_status(
            transaction,
            source.source_run_id(),
            binding.source_event_id(),
            binding.artifact(),
        ) {
            Ok(status) => status,
            Err(LedgerError::ArtifactReferenceNotFound { .. })
            | Err(LedgerError::SealedArtifactReference(_)) => {
                return Err(branch_replay_source_mismatch(
                    source,
                    "verified source artifact reference is no longer available",
                ));
            }
            Err(error) => return Err(error),
        };
        if status.physical_state != binding.physical_state()
            || status.reference_state != binding.reference_state()
        {
            return Err(branch_replay_source_mismatch(
                source,
                "source artifact transition changed after verification",
            ));
        }
    }

    let incomplete_obligation: Option<i64> = transaction
        .query_row(
            "SELECT 1
             FROM session_run_replay_obligations AS obligation
             JOIN session_run_events AS source_event
               ON source_event.event_id = obligation.source_event_id
             LEFT JOIN session_run_events AS resolved_event
               ON resolved_event.event_id = obligation.resolved_event_id
             WHERE obligation.run_id = ?1
               AND source_event.event_sequence <= ?2
               AND (
                    obligation.replay_fidelity = 'pending'
                    OR obligation.resolved_event_id IS NULL
                    OR resolved_event.event_sequence > ?2
               )
             LIMIT 1",
            params![
                source.source_run_id().to_string(),
                i64::try_from(source.boundary_event_sequence())
                    .map_err(|_| LedgerError::NumericOverflow)?,
            ],
            |row| row.get(0),
        )
        .optional()?;
    if incomplete_obligation.is_some() {
        return Err(branch_replay_source_mismatch(
            source,
            "source replay obligation crosses or remains pending at the boundary",
        ));
    }

    let pending_outbox: Option<i64> = transaction
        .query_row(
            "SELECT 1
             FROM session_run_outbox AS outbox
             JOIN session_run_events AS source_event
               ON source_event.event_id = outbox.event_id
             WHERE outbox.run_id = ?1
               AND source_event.event_sequence <= ?2
               AND outbox.state NOT IN ('acknowledged', 'dead_lettered')
             LIMIT 1",
            params![
                source.source_run_id().to_string(),
                i64::try_from(source.boundary_event_sequence())
                    .map_err(|_| LedgerError::NumericOverflow)?,
            ],
            |row| row.get(0),
        )
        .optional()?;
    if pending_outbox.is_some() {
        return Err(branch_replay_source_mismatch(
            source,
            "source outbox remains pending at the boundary",
        ));
    }
    Ok(())
}

fn ensure_run_exists(connection: &rusqlite::Connection, run_id: RunId) -> LedgerResult<()> {
    let exists: Option<i64> = connection
        .query_row(
            "SELECT 1 FROM session_runs WHERE run_id = ?1",
            [run_id.to_string()],
            |row| row.get(0),
        )
        .optional()?;
    if exists.is_none() {
        return Err(LedgerError::RunNotFound {
            run_id: run_id.to_string(),
        });
    }
    Ok(())
}

fn ensure_appendable(connection: &rusqlite::Connection, run_id: RunId) -> LedgerResult<()> {
    let state: Option<(Option<String>, Option<String>)> = connection
        .query_row(
            "SELECT exit_event_id, final_outcome FROM session_runs WHERE run_id = ?1",
            [run_id.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    match state {
        None => Err(LedgerError::RunNotFound {
            run_id: run_id.to_string(),
        }),
        Some((Some(_), _)) | Some((None, Some(_))) => Err(LedgerError::RunAlreadyTerminal {
            run_id: run_id.to_string(),
        }),
        Some((None, None)) => Ok(()),
    }
}

fn ensure_active_append_lease(
    reservation: &ReservationRow,
    lease: &AppendLease,
    now: i64,
) -> LedgerResult<()> {
    ensure_append_lease_identity(reservation, lease)?;
    if reservation.committed_event_id.is_some()
        || reservation.terminal_audit_event_id.is_some()
        || reservation.lease_expires_at_ms <= now
    {
        return Err(LedgerError::StaleAppendLease {
            append_key: lease.append_key.to_string(),
        });
    }
    Ok(())
}

fn ensure_append_lease_identity(
    reservation: &ReservationRow,
    lease: &AppendLease,
) -> LedgerResult<()> {
    if reservation.append_key != lease.append_key
        || reservation.run_id != lease.run_id
        || reservation.event_id != lease.event_id
        || reservation.lease_generation != lease.lease_generation
        || reservation.token != lease.token
    {
        return Err(LedgerError::StaleAppendLease {
            append_key: lease.append_key.to_string(),
        });
    }
    Ok(())
}

fn load_run_status(connection: &rusqlite::Connection, run_id: RunId) -> LedgerResult<RunStatus> {
    let raw: Option<RawRunStatus> = connection
        .query_row(
            "SELECT s.exit_event_id, s.exit_terminal_reason, s.candidate_outcome, s.final_outcome,
                    s.required_dead_lettered, s.pending_required_delivery,
                    x.exact_replay, x.state, x.state_reason, x.transition_sequence,
                    x.latched_at_ms, x.latched_by_event_id
             FROM session_runs AS s
             JOIN session_run_exact_replay AS x ON x.run_id = s.run_id
             WHERE s.run_id = ?1",
            [run_id.to_string()],
            raw_run_status,
        )
        .optional()?;
    let raw = raw.ok_or_else(|| LedgerError::RunNotFound {
        run_id: run_id.to_string(),
    })?;
    RunStatus::try_from((run_id, raw))
}

fn load_run_header(connection: &rusqlite::Connection, run_id: RunId) -> LedgerResult<RunHeader> {
    let raw: Option<RawRunHeader> = connection
        .query_row(
            "SELECT start_event_id, execution_mode, execution_mode_source, started_at_ms,
                    owner_id, owner_lease_generation, owner_lease_expires_at_ms,
                    provenance_canonicalization_algorithm, provenance_canonicalization_version,
                    session_id, parent_run_id, branch_id, tenant_id, action_id, objective_id,
                    task_id, model_id, provider_id, config_digest,
                    prompt_digest, prompt_digest_absence, input_digest, input_digest_absence,
                    output_digest, output_digest_absence, tool_digest, tool_digest_absence,
                    skill_digest, skill_digest_absence, policy_digest, policy_digest_absence,
                    sandbox_digest, sandbox_digest_absence
             FROM session_runs WHERE run_id = ?1",
            [run_id.to_string()],
            raw_run_header,
        )
        .optional()?;
    let raw = raw.ok_or_else(|| LedgerError::RunNotFound {
        run_id: run_id.to_string(),
    })?;
    RunHeader::try_from((run_id, raw))
}

fn load_events_for_run(
    connection: &rusqlite::Connection,
    run_id: RunId,
) -> LedgerResult<Vec<SessionRunEvent>> {
    ensure_run_exists(connection, run_id)?;
    let mut statement = connection.prepare(
        "SELECT event_id, run_id, event_sequence, event_kind, append_key, effect_intent,
                artifact_degradation_reason, public_identity_jcs,
                private_fingerprint_algorithm,
                private_fingerprint_algorithm_version, private_fingerprint_key_id,
                private_fingerprint_key_version, private_fingerprint,
                sealed_artifact_envelope_digest, sealed_artifact_key_id,
                sealed_artifact_schema_version, sealed_artifact_encryption_algorithm,
                sealed_artifact_canonicalization_algorithm,
                sealed_artifact_canonicalization_version, sealed_artifact_redactor_version,
                run_provenance_jcs, public_payload_jcs, manifest_hash, previous_hash,
                event_hash, created_at_ms
         FROM session_run_events WHERE run_id = ?1 ORDER BY event_sequence ASC",
    )?;
    let rows = statement
        .query_map([run_id.to_string()], raw_event)?
        .collect::<Result<Vec<_>, _>>()?;
    rows.into_iter().map(SessionRunEvent::try_from).collect()
}

fn verify_event_chain_for_run(
    connection: &rusqlite::Connection,
    run_id: RunId,
    events: &[SessionRunEvent],
) -> LedgerResult<()> {
    let head_hash: String = connection.query_row(
        "SELECT head_hash FROM session_runs WHERE run_id = ?1",
        [run_id.to_string()],
        |row| row.get(0),
    )?;
    let mut previous_hash = genesis_hash();
    for (index, event) in events.iter().enumerate() {
        let expected_sequence = u64::try_from(index)
            .map_err(|_| LedgerError::NumericOverflow)?
            .checked_add(1)
            .ok_or(LedgerError::NumericOverflow)?;
        if event.run_id != run_id
            || event.sequence != expected_sequence
            || event.previous_hash != previous_hash
            || recompute_event_hash(event)? != event.event_hash
        {
            return Err(LedgerError::IntegrityViolation(
                "run event chain does not match its authoritative head".to_string(),
            ));
        }
        previous_hash.clone_from(&event.event_hash);
    }
    if previous_hash != head_hash {
        return Err(LedgerError::IntegrityViolation(
            "run event chain head does not match session_runs".to_string(),
        ));
    }
    Ok(())
}

fn load_manifest_snapshots_for_run(
    connection: &rusqlite::Connection,
    run_id: RunId,
) -> LedgerResult<Vec<LedgerManifestSnapshot>> {
    let mut statement = connection.prepare(
        "SELECT m.event_id, m.manifest_jcs, m.manifest_hash
         FROM session_run_manifests m
         JOIN session_run_events e ON e.event_id = m.event_id
         WHERE e.run_id = ?1
         ORDER BY e.event_sequence ASC",
    )?;
    let rows = statement
        .query_map([run_id.to_string()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    rows.into_iter()
        .map(|(event_id, canonical_json, hash)| {
            Ok(LedgerManifestSnapshot {
                event_id: parse_event_id(event_id)?,
                canonical_json: CanonicalJson::from_jcs_text(canonical_json)?,
                hash: validated_digest_string("manifest hash", hash)?,
            })
        })
        .collect()
}

fn load_outbox_snapshots_for_run(
    connection: &rusqlite::Connection,
    run_id: RunId,
) -> LedgerResult<Vec<LedgerOutboxSnapshot>> {
    let mut statement = connection.prepare(
        "SELECT o.outbox_id, o.run_id, o.event_id, o.destination, o.required,
                o.idempotency_key, o.public_payload_jcs,
                o.sealed_artifact_envelope_digest, o.sealed_artifact_key_id,
                o.sealed_artifact_schema_version, o.sealed_artifact_encryption_algorithm,
                o.sealed_artifact_canonicalization_algorithm,
                o.sealed_artifact_canonicalization_version, o.sealed_artifact_redactor_version,
                o.state, o.attempt_count, o.max_attempts, o.lease_generation,
                o.delivery_receipt, o.delivery_error, o.manifest_order
         FROM session_run_outbox o
         JOIN session_run_events e ON e.event_id = o.event_id
         WHERE o.run_id = ?1
         ORDER BY e.event_sequence ASC, o.manifest_order ASC",
    )?;
    let rows = statement
        .query_map([run_id.to_string()], |row| {
            Ok((raw_outbox_record(row)?, row.get::<_, i64>(20)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    rows.into_iter()
        .map(|(raw, manifest_order)| {
            Ok(LedgerOutboxSnapshot {
                record: OutboxRecord::try_from(raw)?,
                manifest_order: parse_nonnegative_u32("outbox manifest order", manifest_order)?,
            })
        })
        .collect()
}

fn load_nonresumable_effect_snapshots_for_run(
    connection: &rusqlite::Connection,
    run_id: RunId,
) -> LedgerResult<Vec<NonResumableEffectSnapshot>> {
    let event_ids = {
        let mut statement = connection.prepare(
            "SELECT n.event_id
             FROM session_run_nonresumable_effects n
             JOIN session_run_events e ON e.event_id = n.event_id
             WHERE n.run_id = ?1
             ORDER BY e.event_sequence ASC",
        )?;
        let rows = statement
            .query_map([run_id.to_string()], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        rows
    };
    event_ids
        .into_iter()
        .map(|raw_event_id| {
            let event_id = parse_event_id(raw_event_id)?;
            let row = load_nonresumable_effect(connection, event_id)?.ok_or_else(|| {
                LedgerError::NonResumableEffectNotFound {
                    event_id: event_id.to_string(),
                }
            })?;
            let claim_binding = row
                .claim_token
                .as_ref()
                .map(derive_nonresumable_claim_binding)
                .transpose()?;
            Ok(NonResumableEffectSnapshot {
                record: row.record,
                claim_binding,
            })
        })
        .collect()
}
fn load_replay_obligations_for_run(
    connection: &Connection,
    run_id: RunId,
) -> LedgerResult<Vec<ReplayObligationSnapshot>> {
    ensure_run_exists(connection, run_id)?;
    let mut statement = connection.prepare(
        "SELECT o.source_event_id, source.event_sequence, o.operation_kind, o.phase,
                o.operation_instance_id, o.replay_fidelity, o.outcome, o.resolved_event_id,
                resolved.sealed_artifact_envelope_digest,
                resolved.sealed_artifact_key_id,
                resolved.sealed_artifact_schema_version,
                resolved.sealed_artifact_encryption_algorithm,
                resolved.sealed_artifact_canonicalization_algorithm,
                resolved.sealed_artifact_canonicalization_version,
                resolved.sealed_artifact_redactor_version
         FROM session_run_replay_obligations o
         JOIN session_run_events source ON source.event_id = o.source_event_id
         LEFT JOIN session_run_events resolved ON resolved.event_id = o.resolved_event_id
         WHERE o.run_id = ?1
         ORDER BY source.event_sequence ASC, o.operation_kind ASC, o.phase ASC,
                  o.operation_instance_id ASC",
    )?;
    let rows = statement
        .query_map([run_id.to_string()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, Option<String>>(7)?,
                row.get::<_, Option<String>>(8)?,
                row.get::<_, Option<String>>(9)?,
                row.get::<_, Option<i64>>(10)?,
                row.get::<_, Option<String>>(11)?,
                row.get::<_, Option<String>>(12)?,
                row.get::<_, Option<i64>>(13)?,
                row.get::<_, Option<String>>(14)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    rows.into_iter()
        .map(
            |(
                source_event_id,
                source_event_sequence,
                operation,
                phase,
                operation_instance_id,
                fidelity,
                outcome,
                resolved_event_id,
                envelope_digest,
                key_id,
                schema_version,
                encryption_algorithm,
                canonicalization_algorithm,
                canonicalization_version,
                redactor_version,
            )| {
                let fidelity = ReplayObligationFidelity::from_db(&fidelity)?;
                Ok(ReplayObligationSnapshot {
                    source_event_id: parse_event_id(source_event_id)?,
                    source_event_sequence: parse_nonnegative_u64(
                        "replay obligation source event sequence",
                        source_event_sequence,
                    )?,
                    operation: ReplayOperationKind::from_db(&operation)?,
                    phase: ReplayOperationPhase::from_db(&phase)?,
                    operation_instance_id: parse_replay_operation_instance_id(operation_instance_id)?,
                    required: true,
                    fidelity,
                    outcome: ReplayOperationOutcome::from_db(&outcome)?,
                    resolved_event_id: resolved_event_id.map(parse_event_id).transpose()?,
                    artifact: parse_optional_artifact_ref(
                        envelope_digest,
                        key_id,
                        schema_version,
                        encryption_algorithm,
                        canonicalization_algorithm,
                        canonicalization_version,
                        redactor_version,
                    )?,
                    reason: match fidelity {
                        ReplayObligationFidelity::MetadataOnly
                        | ReplayObligationFidelity::Unavailable => {
                            Some(ArtifactStateReason::Sanitized)
                        }
                        ReplayObligationFidelity::Pending
                        | ReplayObligationFidelity::ExactPrivate => None,
                    },
                })
            },
        )
        .collect()
}

fn load_event_for_append_key(
    connection: &rusqlite::Connection,
    append_key: &AppendKey,
) -> LedgerResult<Option<SessionRunEvent>> {
    let raw_run_id: Option<String> = connection
        .query_row(
            "SELECT run_id FROM session_run_events WHERE append_key = ?1",
            [append_key.as_str()],
            |row| row.get(0),
        )
        .optional()?;
    let Some(raw_run_id) = raw_run_id else {
        return Ok(None);
    };
    let run_id = parse_run_id(raw_run_id)?;
    load_events_for_run(connection, run_id)?
        .into_iter()
        .find(|event| event.append_key.as_ref() == Some(append_key))
        .map(Some)
        .ok_or_else(|| {
            LedgerError::IntegrityViolation(
                "event append key lookup did not resolve to a run event".to_string(),
            )
        })
}

struct TerminalizedNonResumableEffects {
    has_unknown: bool,
    abandoned_count: usize,
}

fn terminalize_nonresumable_effects(
    transaction: &Transaction<'_>,
    run_id: RunId,
    now: i64,
) -> LedgerResult<TerminalizedNonResumableEffects> {
    let pending = {
        let mut statement = transaction.prepare(
            "SELECT event_id, state
             FROM session_run_nonresumable_effects
             WHERE run_id = ?1 AND state IN ('armed', 'executing')
             ORDER BY event_id ASC",
        )?;
        let rows = statement
            .query_map([run_id.to_string()], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        rows
    };
    let mut has_unknown = false;
    let mut abandoned_count = 0_usize;
    for (raw_event_id, raw_state) in pending {
        let event_id = parse_event_id(raw_event_id)?;
        let state = NonResumableEffectState::from_db(&raw_state)?;
        if state == NonResumableEffectState::Armed {
            let changed = transaction.execute(
                "UPDATE session_run_nonresumable_effects
                 SET state = 'abandoned', terminal_at_ms = ?1
                 WHERE event_id = ?2 AND run_id = ?3 AND state = 'armed'",
                params![now, event_id.to_string(), run_id.to_string()],
            )?;
            if changed != 1 {
                return Err(LedgerError::IntegrityViolation(
                    "non-resumable armed effect changed during terminalization".to_string(),
                ));
            }
            abandoned_count = abandoned_count
                .checked_add(1)
                .ok_or(LedgerError::NumericOverflow)?;
            continue;
        }

        let row = load_nonresumable_effect(transaction, event_id)?.ok_or_else(|| {
            LedgerError::NonResumableEffectNotFound {
                event_id: event_id.to_string(),
            }
        })?;
        let token = row.claim_token.ok_or_else(|| {
            LedgerError::IntegrityViolation(
                "executing non-resumable effect lacks a claim token".to_string(),
            )
        })?;
        let claimed_at_ms = row.record.claimed_at_ms.ok_or_else(|| {
            LedgerError::IntegrityViolation(
                "executing non-resumable effect lacks a claim timestamp".to_string(),
            )
        })?;
        let result_payload_digest = row.record.result_payload_digest.clone();
        let claim_binding = derive_nonresumable_claim_binding(&token)?;
        let result_append_key = derive_nonresumable_result_append_key(event_id, &claim_binding)?;
        let lease = NonResumableEffectLease {
            run_id,
            event_id,
            idempotency_key: row.record.idempotency_key,
            claim_binding,
            result_append_key,
            claimed_at_ms,
            token,
        };
        let result_event_id = transaction
            .query_row(
                "SELECT event_id FROM session_run_events
                 WHERE run_id = ?1 AND append_key = ?2",
                params![run_id.to_string(), lease.result_append_key.as_str()],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .map(parse_event_id)
            .transpose()?;
        let (state, result_event_id) = match (result_event_id, result_payload_digest.as_ref()) {
            (Some(result_event_id), Some(result_payload_digest)) => {
                match validate_nonresumable_result_event(
                    transaction,
                    &lease,
                    result_event_id,
                    result_payload_digest,
                ) {
                    Ok(()) => ("completed", Some(result_event_id)),
                    Err(LedgerError::NonResumableEffectResultMismatch { .. }) => {
                        has_unknown = true;
                        ("unknown_non_resumable", None)
                    }
                    Err(error) => return Err(error),
                }
            }
            _ => {
                has_unknown = true;
                ("unknown_non_resumable", None)
            }
        };
        let changed = transaction.execute(
            "UPDATE session_run_nonresumable_effects
             SET state = ?1, result_event_id = ?2, terminal_at_ms = ?3
             WHERE event_id = ?4 AND run_id = ?5 AND state = 'executing'
               AND claim_token = ?6",
            params![
                state,
                result_event_id.map(|value| value.to_string()),
                now,
                event_id.to_string(),
                run_id.to_string(),
                lease.token.to_blob(),
            ],
        )?;
        if changed != 1 {
            return Err(LedgerError::IntegrityViolation(
                "executing non-resumable effect changed during terminalization".to_string(),
            ));
        }
    }
    Ok(TerminalizedNonResumableEffects {
        has_unknown,
        abandoned_count,
    })
}

fn reconcile_run_finalization(transaction: &Transaction<'_>, run_id: RunId) -> LedgerResult<()> {
    let status = load_run_status(transaction, run_id)?;
    if status.required_dead_lettered {
        transaction.execute(
            "UPDATE session_runs
             SET final_outcome = 'dead_lettered', pending_required_delivery = 0
             WHERE run_id = ?1",
            [run_id.to_string()],
        )?;
        return Ok(());
    }

    let Some(candidate_outcome) = status.candidate_outcome else {
        return Ok(());
    };
    let unresolved_required: i64 = transaction.query_row(
        "SELECT COUNT(*) FROM session_run_outbox
         WHERE run_id = ?1 AND required = 1 AND state != 'acknowledged'",
        [run_id.to_string()],
        |row| row.get(0),
    )?;
    if unresolved_required == 0 {
        transaction.execute(
            "UPDATE session_runs
             SET final_outcome = ?1, pending_required_delivery = 0
             WHERE run_id = ?2 AND required_dead_lettered = 0",
            params![candidate_outcome.as_str(), run_id.to_string()],
        )?;
    } else {
        transaction.execute(
            "UPDATE session_runs
             SET pending_required_delivery = 1
             WHERE run_id = ?1 AND final_outcome IS NULL",
            [run_id.to_string()],
        )?;
    }
    Ok(())
}

fn complete_delivery_tx(
    transaction: &Transaction<'_>,
    config: &RunLedgerConfig,
    mode: ResolvedExecutionMode,
    fence: &OutboxFence,
    completion: DeliveryCompletion,
    evidence: Option<&DeliveryEvidence>,
    now: i64,
) -> LedgerResult<DeliveryCompletionReceipt> {
    ensure_mode_matches(transaction, fence.run_id, mode)?;
    let state = match completion {
        DeliveryCompletion::Acknowledged(receipt) => {
            let changed = transaction.execute(
                "UPDATE session_run_outbox
                 SET state = 'acknowledged', lease_token = NULL, lease_expires_at_ms = NULL,
                     delivery_receipt = ?1, delivery_error = NULL, terminal_at_ms = ?2
                 WHERE outbox_id = ?3 AND state = 'leased' AND lease_generation = ?4
                   AND lease_token = ?5",
                params![
                    receipt.as_str(),
                    now,
                    fence.outbox_id.to_string(),
                    i64::try_from(fence.lease_generation)
                        .map_err(|_| LedgerError::NumericOverflow)?,
                    fence.token.to_blob(),
                ],
            )?;
            if changed != 1 {
                return Err(LedgerError::StaleOutboxLease {
                    outbox_id: fence.outbox_id.to_string(),
                });
            }
            append_delivery_result_event(
                transaction,
                config,
                fence,
                OutboxState::Acknowledged,
                None,
                evidence,
                now,
            )?;
            OutboxState::Acknowledged
        }
        DeliveryCompletion::Nacked(reason) => {
            let dead_letter = fence.attempt_count >= fence.max_attempts;
            let next_state = if dead_letter {
                OutboxState::DeadLettered
            } else {
                OutboxState::Ready
            };
            let changed = transaction.execute(
                "UPDATE session_run_outbox
                 SET state = ?1, lease_token = NULL, lease_expires_at_ms = NULL,
                     delivery_receipt = NULL, delivery_error = ?2, terminal_at_ms = ?3
                 WHERE outbox_id = ?4 AND state = 'leased' AND lease_generation = ?5
                   AND lease_token = ?6",
                params![
                    next_state.as_str(),
                    if dead_letter {
                        Some(reason.as_str())
                    } else {
                        None
                    },
                    if dead_letter { Some(now) } else { None },
                    fence.outbox_id.to_string(),
                    i64::try_from(fence.lease_generation)
                        .map_err(|_| LedgerError::NumericOverflow)?,
                    fence.token.to_blob(),
                ],
            )?;
            if changed != 1 {
                return Err(LedgerError::StaleOutboxLease {
                    outbox_id: fence.outbox_id.to_string(),
                });
            }
            append_delivery_result_event(
                transaction,
                config,
                fence,
                next_state,
                Some(reason),
                evidence,
                now,
            )?;
            if dead_letter && fence.required {
                transaction.execute(
                    "UPDATE session_runs SET required_dead_lettered = 1 WHERE run_id = ?1",
                    [fence.run_id.to_string()],
                )?;
            }
            next_state
        }
    };
    reconcile_run_finalization(transaction, fence.run_id)?;
    let run_status = load_run_status(transaction, fence.run_id)?;
    Ok(DeliveryCompletionReceipt {
        outbox_id: fence.outbox_id,
        state,
        run_status,
    })
}

fn dead_letter_expired_outbox_tx(
    transaction: &Transaction<'_>,
    config: &RunLedgerConfig,
    mode: ResolvedExecutionMode,
    fence: &OutboxFence,
    now: i64,
) -> LedgerResult<()> {
    ensure_mode_matches(transaction, fence.run_id, mode)?;
    if fence.attempt_count != fence.max_attempts {
        return Err(LedgerError::IntegrityViolation(
            "only a lease at its exact maximum attempts may terminalize on expiry".to_string(),
        ));
    }
    let changed = transaction.execute(
        "UPDATE session_run_outbox
         SET state = 'dead_lettered', lease_token = NULL, lease_expires_at_ms = NULL,
             delivery_receipt = NULL, delivery_error = 'attempts_exhausted', terminal_at_ms = ?1
         WHERE outbox_id = ?2 AND state = 'leased' AND lease_generation = ?3
           AND lease_token = ?4 AND lease_expires_at_ms <= ?5
           AND attempt_count = max_attempts",
        params![
            now,
            fence.outbox_id.to_string(),
            i64::try_from(fence.lease_generation).map_err(|_| LedgerError::NumericOverflow)?,
            fence.token.to_blob(),
            now,
        ],
    )?;
    if changed != 1 {
        return Err(LedgerError::StaleOutboxLease {
            outbox_id: fence.outbox_id.to_string(),
        });
    }
    append_delivery_result_event(
        transaction,
        config,
        fence,
        OutboxState::DeadLettered,
        Some(DeliveryFailureReason::AttemptsExhausted),
        None,
        now,
    )?;
    if fence.required {
        transaction.execute(
            "UPDATE session_runs SET required_dead_lettered = 1 WHERE run_id = ?1",
            [fence.run_id.to_string()],
        )?;
    }
    reconcile_run_finalization(transaction, fence.run_id)
}

fn append_delivery_result_event(
    transaction: &Transaction<'_>,
    config: &RunLedgerConfig,
    fence: &OutboxFence,
    state: OutboxState,
    error: Option<DeliveryFailureReason>,
    evidence: Option<&DeliveryEvidence>,
    now: i64,
) -> LedgerResult<()> {
    let payload = CanonicalJson::from_value(&json!({
        "attempt_count": fence.attempt_count,
        "delivery_error": error.map(DeliveryFailureReason::as_str),
        "max_attempts": fence.max_attempts,
        "outbox_id": fence.outbox_id.to_string(),
        "schema": DELIVERY_RESULT_BINDING_SCHEMA,
        "state": state.as_str(),
    }))?;
    let committed = append_event_tx(
        transaction,
        config,
        EventDraft {
            event_id: EventId::new(),
            run_id: fence.run_id,
            kind: SessionRunEventKind::DeliveryResult,
            append_key: None,
            effect_intent: EffectIntent::None,
            artifact_degradation_reason: None,
            identity: evidence.map(|value| value.identity.clone()),
            sealed_artifact_ref: evidence.map(|value| value.sealed_artifact_ref.clone()),
            run_provenance: None,
            public_payload: payload,
            created_at_ms: now,
        },
    )?;
    let fidelity = match evidence.map(|value| value.replay_fidelity) {
        Some(ReplayArtifactFidelity::ExactPrivate) => ReplayObligationFidelity::ExactPrivate,
        Some(ReplayArtifactFidelity::MetadataOnly) | None => ReplayObligationFidelity::Unavailable,
    };
    let outcome = if state == OutboxState::Acknowledged {
        ReplayOperationOutcome::Returned
    } else {
        ReplayOperationOutcome::HandlerError
    };
    transaction.execute(
        "INSERT INTO session_run_replay_obligations (
            run_id, source_event_id, operation_kind, phase, operation_instance_id,
            replay_fidelity, outcome, resolved_event_id, created_at_ms, resolved_at_ms
         ) VALUES (?1, ?2, 'delivery', 'result', ?3, ?4, ?5, ?2, ?6, ?6)",
        params![
            fence.run_id.to_string(),
            committed.event_id.to_string(),
            fence.outbox_id.to_string(),
            fidelity.as_str(),
            outcome.as_str(),
            now,
        ],
    )?;
    let next_state = derive_next_exact_replay_state_tx(transaction, fence.run_id)?;
    update_exact_replay_latch_tx(transaction, fence.run_id, next_state, committed.event_id)?;
    Ok(())
}

fn append_receipt_for_key(
    connection: &rusqlite::Connection,
    append_key: &AppendKey,
) -> LedgerResult<AppendReceipt> {
    try_append_receipt_for_key(connection, append_key)?.ok_or_else(|| {
        LedgerError::AppendReservationNotFound {
            append_key: append_key.to_string(),
        }
    })
}

fn try_append_receipt_for_key(
    connection: &rusqlite::Connection,
    append_key: &AppendKey,
) -> LedgerResult<Option<AppendReceipt>> {
    let raw = connection
        .query_row(
            "SELECT event_id, run_id, event_sequence, event_hash,
                    sealed_artifact_envelope_digest, sealed_artifact_key_id,
                    sealed_artifact_schema_version, sealed_artifact_encryption_algorithm,
                    sealed_artifact_canonicalization_algorithm,
                    sealed_artifact_canonicalization_version, sealed_artifact_redactor_version,
                    event_kind, artifact_degradation_reason
             FROM session_run_events WHERE append_key = ?1",
            [append_key.as_str()],
            raw_append_receipt,
        )
        .optional()?;
    raw.map(|raw| append_receipt_from_raw(connection, raw))
        .transpose()
}

fn append_receipt_from_raw(
    connection: &rusqlite::Connection,
    raw: RawAppendReceipt,
) -> LedgerResult<AppendReceipt> {
    let mut statement = connection.prepare(
        "SELECT outbox_id FROM session_run_outbox WHERE event_id = ?1 ORDER BY manifest_order",
    )?;

    let outbox_ids = statement
        .query_map([raw.event_id.clone()], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .map(parse_outbox_id)
        .collect::<LedgerResult<Vec<_>>>()?;
    Ok(AppendReceipt {
        run_id: parse_run_id(raw.run_id)?,
        event_id: parse_event_id(raw.event_id)?,
        event_sequence: parse_nonnegative_u64("event sequence", raw.event_sequence)?,
        event_hash: validated_digest_string("event hash", raw.event_hash)?,
        event_kind: SessionRunEventKind::from_db(&raw.event_kind)?,
        artifact_degradation_reason: raw
            .artifact_degradation_reason
            .map(|reason| ArtifactDegradationReason::from_db(&reason))
            .transpose()?,
        sealed_artifact_ref: parse_optional_artifact_ref(
            raw.sealed_artifact_envelope_digest,
            raw.sealed_artifact_key_id,
            raw.sealed_artifact_schema_version,
            raw.sealed_artifact_encryption_algorithm,
            raw.sealed_artifact_canonicalization_algorithm,
            raw.sealed_artifact_canonicalization_version,
            raw.sealed_artifact_redactor_version,
        )?,
        outbox_ids,
    })
}

fn append_receipt_for_event_id(
    connection: &rusqlite::Connection,
    event_id: EventId,
) -> LedgerResult<AppendReceipt> {
    let raw = connection
        .query_row(
            "SELECT event_id, run_id, event_sequence, event_hash,
                    sealed_artifact_envelope_digest, sealed_artifact_key_id,
                    sealed_artifact_schema_version, sealed_artifact_encryption_algorithm,
                    sealed_artifact_canonicalization_algorithm,
                    sealed_artifact_canonicalization_version, sealed_artifact_redactor_version,
                    event_kind, artifact_degradation_reason
             FROM session_run_events WHERE event_id = ?1",
            [event_id.to_string()],
            raw_append_receipt,
        )
        .map_err(|error| match error {
            rusqlite::Error::QueryReturnedNoRows => LedgerError::IntegrityViolation(format!(
                "terminal audit event {event_id} is missing"
            )),
            other => LedgerError::Sqlite(other),
        })?;
    append_receipt_from_raw(connection, raw)
}

fn ensure_matching_replay_append_contract(
    connection: &rusqlite::Connection,
    append_key: &AppendKey,
    requested: Option<ReplayAppendContract>,
    requested_fidelity: ReplayArtifactFidelity,
) -> LedgerResult<()> {
    let stored: Option<(String, String, Option<String>, String)> = connection
        .query_row(
            "SELECT operation_kind, phase, resolution_source_event_id, requested_fidelity
             FROM session_run_replay_append_contracts WHERE append_key = ?1",
            [append_key.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()?;
    match (stored, requested) {
        (None, None) => Ok(()),
        (Some((operation, phase, source_event_id, fidelity)), Some(contract))
            if ReplayOperationKind::from_db(&operation)? == contract.operation
                && ReplayOperationPhase::from_db(&phase)? == contract.phase
                && source_event_id.clone().map(parse_event_id).transpose()?
                    == contract.resolution_source_event_id
                && ReplayArtifactFidelity::from_db(&fidelity)? == requested_fidelity =>
        {
            Ok(())
        }
        _ => Err(LedgerError::AppendKeyConflict {
            append_key: append_key.to_string(),
        }),
    }
}

fn validate_replay_append_contract(contract: ReplayAppendContract) -> LedgerResult<()> {
    let valid = matches!(
        (contract.operation, contract.phase, contract.resolution_source_event_id.is_some()),
        (ReplayOperationKind::Model, ReplayOperationPhase::Request, false)
            | (ReplayOperationKind::Model, ReplayOperationPhase::Result, true)
            | (ReplayOperationKind::Tool, ReplayOperationPhase::Request, false)
            | (ReplayOperationKind::Tool, ReplayOperationPhase::Result, true)
            | (ReplayOperationKind::Delegation, ReplayOperationPhase::Request, false)
            | (ReplayOperationKind::Delegation, ReplayOperationPhase::Result, true)
            | (ReplayOperationKind::Oauth, ReplayOperationPhase::Request, false)
            | (ReplayOperationKind::Oauth, ReplayOperationPhase::Result, true)
            | (ReplayOperationKind::AgentOutput, ReplayOperationPhase::Result, false)
    );
    if valid {
        Ok(())
    } else {
        Err(LedgerError::IntegrityViolation(
            "invalid replay append operation/phase/source contract".to_string(),
        ))
    }
}

fn resolve_replay_append_contract_tx(
    transaction: &Transaction<'_>,
    reservation: &ReservationRow,
    committed_event_id: EventId,
    now: i64,
) -> LedgerResult<()> {
    let contract: Option<(String, String, Option<String>, String, String, String)> = transaction
        .query_row(
            "SELECT run_id, reserved_event_id, resolution_source_event_id, operation_kind, phase,
                    requested_fidelity
             FROM session_run_replay_append_contracts WHERE append_key = ?1",
            [reservation.append_key.as_str()],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            },
        )
        .optional()?;
    let Some((run_id, reserved_event_id, source_event_id, operation, phase, fidelity)) = contract
    else {
        return Ok(());
    };
    if run_id != reservation.run_id.to_string()
        || reserved_event_id != committed_event_id.to_string()
    {
        return Err(LedgerError::IntegrityViolation(
            "replay append contract does not bind its reserved run event".to_string(),
        ));
    }
    let operation = ReplayOperationKind::from_db(&operation)?;
    let phase = ReplayOperationPhase::from_db(&phase)?;
    let fidelity = ReplayArtifactFidelity::from_db(&fidelity)?;
    let source_event_id = source_event_id.map(parse_event_id).transpose()?;
    let contract = ReplayAppendContract {
        operation,
        phase,
        resolution_source_event_id: source_event_id,
    };
    validate_replay_append_contract(contract)?;
    let identity: Value = serde_json::from_str(reservation.identity.public_identity.as_str())
        .map_err(|error| LedgerError::IntegrityViolation(format!(
            "replay append identity is not valid canonical JSON: {error}"
        )))?;
    let outcome = identity
        .get("outcome")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            LedgerError::IntegrityViolation(
                "replay append identity lacks its closed outcome classification".to_string(),
            )
        })
        .and_then(ReplayOperationOutcome::from_db)?;
    if (phase == ReplayOperationPhase::Request
        && outcome != ReplayOperationOutcome::NotApplicable)
        || (phase == ReplayOperationPhase::Result
            && outcome == ReplayOperationOutcome::NotApplicable)
    {
        return Err(LedgerError::IntegrityViolation(
            "replay append identity outcome does not match its operation phase".to_string(),
        ));
    }
    let expected_identity = match (operation, phase) {
        (ReplayOperationKind::Model, ReplayOperationPhase::Request) => ("request", "llm-request"),
        (ReplayOperationKind::Model, ReplayOperationPhase::Result) => ("result", "llm-response"),
        (ReplayOperationKind::Tool, ReplayOperationPhase::Request) => {
            ("request", "tool-invocation")
        }
        (ReplayOperationKind::Tool, ReplayOperationPhase::Result) => ("result", "tool-invocation"),
        (ReplayOperationKind::Delegation, ReplayOperationPhase::Request) => {
            ("request", "delegation")
        }
        (ReplayOperationKind::Delegation, ReplayOperationPhase::Result) => {
            ("result", "delegation")
        }
        (ReplayOperationKind::Oauth, ReplayOperationPhase::Request) => {
            ("request", "oauth-refresh")
        }
        (ReplayOperationKind::Oauth, ReplayOperationPhase::Result) => {
            ("result", "oauth-refresh")
        }
        (ReplayOperationKind::AgentOutput, ReplayOperationPhase::Result) => {
            ("result", "agent-output")
        }
        _ => unreachable!("validated replay append contract"),
    };
    if identity
        .get("artifact_kind")
        .and_then(Value::as_str)
        != Some(expected_identity.0)
        || identity
            .get("evidence_subject")
            .and_then(Value::as_str)
            != Some(expected_identity.1)
    {
        return Err(LedgerError::IntegrityViolation(
            "replay append identity does not match its typed contract".to_string(),
        ));
    }

    match (phase, source_event_id) {
        (ReplayOperationPhase::Request, None) => {
            transaction.execute(
                "INSERT INTO session_run_replay_obligations (
                    run_id, source_event_id, operation_kind, phase, operation_instance_id,
                    replay_fidelity, outcome, resolved_event_id, created_at_ms, resolved_at_ms
                 ) VALUES (?1, ?2, ?3, 'request', ?2, ?4, 'not_applicable', ?2, ?5, ?5)",
                params![
                    reservation.run_id.to_string(),
                    committed_event_id.to_string(),
                    operation.as_str(),
                    fidelity.as_str(),
                    now,
                ],
            )?;
            transaction.execute(
                "INSERT INTO session_run_replay_obligations (
                    run_id, source_event_id, operation_kind, phase, operation_instance_id,
                    replay_fidelity, outcome, resolved_event_id, created_at_ms, resolved_at_ms
                 ) VALUES (?1, ?2, ?3, 'result', ?2, 'pending', 'unavailable', NULL, ?4, NULL)",
                params![
                    reservation.run_id.to_string(),
                    committed_event_id.to_string(),
                    operation.as_str(),
                    now,
                ],
            )?;
        }
        (ReplayOperationPhase::Result, Some(source_event_id)) => {
            let source_run: Option<String> = transaction
                .query_row(
                    "SELECT run_id FROM session_run_events WHERE event_id = ?1",
                    [source_event_id.to_string()],
                    |row| row.get(0),
                )
                .optional()?;
            let expected_run_id = reservation.run_id.to_string();
            if source_run.as_deref() != Some(expected_run_id.as_str()) {
                return Err(LedgerError::IntegrityViolation(
                    "replay result contract resolves an event from another run".to_string(),
                ));
            }
            let changed = transaction.execute(
                "UPDATE session_run_replay_obligations
                 SET replay_fidelity = ?1, outcome = ?2, resolved_event_id = ?3, resolved_at_ms = ?4
                 WHERE run_id = ?5 AND source_event_id = ?6 AND operation_kind = ?7
                   AND operation_instance_id = ?6
                   AND phase = 'result' AND replay_fidelity = 'pending'
                   AND outcome = 'unavailable' AND resolved_event_id IS NULL AND resolved_at_ms IS NULL",
                params![
                    fidelity.as_str(),
                    outcome.as_str(),
                    committed_event_id.to_string(),
                    now,
                    reservation.run_id.to_string(),
                    source_event_id.to_string(),
                    operation.as_str(),
                ],
            )?;
            if changed != 1 {
                return Err(LedgerError::IntegrityViolation(
                    "replay result contract has no matching pending request obligation".to_string(),
                ));
            }
        }
        (ReplayOperationPhase::Result, None) => {
            transaction.execute(
                "INSERT INTO session_run_replay_obligations (
                    run_id, source_event_id, operation_kind, phase, operation_instance_id,
                    replay_fidelity, outcome, resolved_event_id, created_at_ms, resolved_at_ms
                 ) VALUES (?1, ?2, ?3, 'result', ?2, ?4, ?5, ?2, ?6, ?6)",
                params![
                    reservation.run_id.to_string(),
                    committed_event_id.to_string(),
                    operation.as_str(),
                    fidelity.as_str(),
                    outcome.as_str(),
                    now,
                ],
            )?;
        }
        (ReplayOperationPhase::Request, Some(_)) => {
            return Err(LedgerError::IntegrityViolation(
                "replay request contract cannot resolve a prior event".to_string(),
            ));
        }
    }
    let next_state = derive_next_exact_replay_state_tx(transaction, reservation.run_id)?;
    update_exact_replay_latch_tx(transaction, reservation.run_id, next_state, committed_event_id)?;
    Ok(())
}

/// Close every unresolved result obligation in the same transaction that
/// appends `RunExit`. A crash or cancellation cannot leave a replay-critical
/// request without a typed terminal result.
fn resolve_pending_replay_obligations_at_exit_tx(
    transaction: &Transaction<'_>,
    run_id: RunId,
    terminal_reason: TerminalReason,
    exit_event_id: EventId,
    now: i64,
) -> LedgerResult<()> {
    let outcome = match terminal_reason {
        TerminalReason::Cancellation => ReplayOperationOutcome::Cancelled,
        TerminalReason::Timeout => ReplayOperationOutcome::TimedOut,
        TerminalReason::EffectNonResumableUnknown => {
            ReplayOperationOutcome::EffectNonResumableUnknown
        }
        TerminalReason::Completed
        | TerminalReason::ExecutionFailure
        | TerminalReason::PolicyBlocked
        | TerminalReason::RequiredDeliveryDeadLettered
        | TerminalReason::ProcessDeath => ReplayOperationOutcome::Unavailable,
    };
    let changed = transaction.execute(
        "UPDATE session_run_replay_obligations
         SET replay_fidelity = 'unavailable', outcome = ?1, resolved_event_id = ?2,
             resolved_at_ms = ?3
         WHERE run_id = ?4 AND phase = 'result' AND replay_fidelity = 'pending'
           AND outcome = 'unavailable' AND resolved_event_id IS NULL AND resolved_at_ms IS NULL",
        params![
            outcome.as_str(),
            exit_event_id.to_string(),
            now,
            run_id.to_string(),
        ],
    )?;
    if changed == 0 {
        return Ok(());
    }
    let next_state = derive_next_exact_replay_state_tx(transaction, run_id)?;
    update_exact_replay_latch_tx(transaction, run_id, next_state, exit_event_id)?;
    Ok(())
}

fn resolve_degraded_replay_append_contract_tx(
    transaction: &Transaction<'_>,
    reservation: &ReservationRow,
    audit_event_id: EventId,
    now: i64,
) -> LedgerResult<()> {
    let contract: Option<(Option<String>, String, String)> = transaction
        .query_row(
            "SELECT resolution_source_event_id, operation_kind, phase
             FROM session_run_replay_append_contracts WHERE append_key = ?1",
            [reservation.append_key.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    let Some((source_event_id, operation, phase)) = contract else {
        return Ok(());
    };
    let operation = ReplayOperationKind::from_db(&operation)?;
    let phase = ReplayOperationPhase::from_db(&phase)?;
    let source_event_id = source_event_id.map(parse_event_id).transpose()?;
    match (phase, source_event_id) {
        (ReplayOperationPhase::Request, None) => {
            transaction.execute(
                "INSERT INTO session_run_replay_obligations (
                    run_id, source_event_id, operation_kind, phase, operation_instance_id,
                    replay_fidelity, outcome, resolved_event_id, created_at_ms, resolved_at_ms
                 ) VALUES (?1, ?2, ?3, 'request', ?2, 'unavailable', 'not_applicable', ?2, ?4, ?4)",
                params![
                    reservation.run_id.to_string(),
                    audit_event_id.to_string(),
                    operation.as_str(),
                    now,
                ],
            )?;
            transaction.execute(
                "INSERT INTO session_run_replay_obligations (
                    run_id, source_event_id, operation_kind, phase, operation_instance_id,
                    replay_fidelity, outcome, resolved_event_id, created_at_ms, resolved_at_ms
                 ) VALUES (?1, ?2, ?3, 'result', ?2, 'unavailable', 'unavailable', ?2, ?4, ?4)",
                params![
                    reservation.run_id.to_string(),
                    audit_event_id.to_string(),
                    operation.as_str(),
                    now,
                ],
            )?;
        }
        (ReplayOperationPhase::Result, Some(source_event_id)) => {
            let changed = transaction.execute(
                "UPDATE session_run_replay_obligations
                 SET replay_fidelity = 'unavailable', outcome = 'unavailable',
                     resolved_event_id = ?1, resolved_at_ms = ?2
                 WHERE run_id = ?3 AND source_event_id = ?4 AND operation_kind = ?5
                   AND operation_instance_id = ?4
                   AND phase = 'result' AND replay_fidelity = 'pending'
                   AND outcome = 'unavailable' AND resolved_event_id IS NULL AND resolved_at_ms IS NULL",
                params![
                    audit_event_id.to_string(),
                    now,
                    reservation.run_id.to_string(),
                    source_event_id.to_string(),
                    operation.as_str(),
                ],
            )?;
            if changed != 1 {
                return Err(LedgerError::IntegrityViolation(
                    "degraded replay result has no matching pending request obligation".to_string(),
                ));
            }
        }
        (ReplayOperationPhase::Result, None) => {
            transaction.execute(
                "INSERT INTO session_run_replay_obligations (
                    run_id, source_event_id, operation_kind, phase, operation_instance_id,
                    replay_fidelity, outcome, resolved_event_id, created_at_ms, resolved_at_ms
                 ) VALUES (?1, ?2, ?3, 'result', ?2, 'unavailable', 'unavailable', ?2, ?4, ?4)",
                params![
                    reservation.run_id.to_string(),
                    audit_event_id.to_string(),
                    operation.as_str(),
                    now,
                ],
            )?;
        }
        (ReplayOperationPhase::Request, Some(_)) => {
            return Err(LedgerError::IntegrityViolation(
                "degraded replay request resolves a prior event".to_string(),
            ));
        }
    }
    update_exact_replay_latch_tx(
        transaction,
        reservation.run_id,
        ExactReplayState::UnavailableSanitized,
        audit_event_id,
    )?;
    Ok(())
}

fn ensure_matching_reservation(
    reservation: &ReservationRow,
    request: &AppendRequest,
) -> LedgerResult<()> {
    if reservation.identity.public_identity != request.identity.public_identity {
        return Err(LedgerError::AppendKeyConflict {
            append_key: request.append_key.to_string(),
        });
    }
    if reservation.identity.private_fingerprint != request.identity.private_fingerprint {
        return Err(LedgerError::PrivateFingerprintMismatch {
            append_key: request.append_key.to_string(),
        });
    }
    if reservation.run_id != request.run_id
        || reservation.effect_intent != request.effect_intent
        || reservation.public_payload != request.public_payload
    {
        return Err(LedgerError::AppendKeyConflict {
            append_key: request.append_key.to_string(),
        });
    }
    Ok(())
}

fn load_reservation(
    connection: &rusqlite::Connection,
    append_key: &AppendKey,
) -> LedgerResult<Option<ReservationRow>> {
    let raw: Option<RawReservation> = connection
        .query_row(
            "SELECT append_key, run_id, event_id, frozen_event_at_ms, public_identity_jcs,
                    private_fingerprint_algorithm, private_fingerprint_algorithm_version,
                    private_fingerprint_key_id, private_fingerprint_key_version,
                    private_fingerprint, effect_intent, public_payload_jcs,
                    sealed_artifact_envelope_digest, sealed_artifact_key_id,
                    sealed_artifact_schema_version, sealed_artifact_encryption_algorithm,
                    sealed_artifact_canonicalization_algorithm,
                    sealed_artifact_canonicalization_version, sealed_artifact_redactor_version,
                    lease_generation, lease_token, lease_expires_at_ms, committed_event_id,
                    terminal_audit_event_id
             FROM session_run_append_reservations WHERE append_key = ?1",
            [append_key.as_str()],
            raw_reservation,
        )
        .optional()?;
    raw.map(ReservationRow::try_from).transpose()
}

fn load_expired_exhausted_outbox(
    transaction: &Transaction<'_>,
    now: i64,
    mode: &ResolvedExecutionMode,
) -> LedgerResult<Option<OutboxCandidate>> {
    load_outbox_candidate(
        transaction,
        "o.state = 'leased' AND o.lease_expires_at_ms <= ?1 AND o.attempt_count = o.max_attempts",
        now,
        mode,
    )
}

fn load_claimable_outbox(
    transaction: &Transaction<'_>,
    now: i64,
    mode: &ResolvedExecutionMode,
) -> LedgerResult<Option<OutboxCandidate>> {
    load_outbox_candidate(
        transaction,
        "(o.state = 'ready' OR (o.state = 'leased' AND o.lease_expires_at_ms <= ?1)) AND o.attempt_count < o.max_attempts",
        now,
        mode,
    )
}

fn load_outbox_candidate(
    transaction: &Transaction<'_>,
    predicate: &str,
    now: i64,
    mode: &ResolvedExecutionMode,
) -> LedgerResult<Option<OutboxCandidate>> {
    let query = format!(
        "SELECT o.outbox_id, o.run_id, o.event_id, o.destination, o.required, o.idempotency_key,
                o.public_payload_jcs,
                o.sealed_artifact_envelope_digest, o.sealed_artifact_key_id,
                o.sealed_artifact_schema_version, o.sealed_artifact_encryption_algorithm,
                o.sealed_artifact_canonicalization_algorithm,
                o.sealed_artifact_canonicalization_version, o.sealed_artifact_redactor_version,
                e.sealed_artifact_envelope_digest, e.sealed_artifact_key_id,
                e.sealed_artifact_schema_version, e.sealed_artifact_encryption_algorithm,
                e.sealed_artifact_canonicalization_algorithm,
                e.sealed_artifact_canonicalization_version, e.sealed_artifact_redactor_version,
                o.attempt_count, o.max_attempts, o.lease_generation, o.state,
                o.lease_token, o.lease_expires_at_ms
         FROM session_run_outbox o
         JOIN session_run_events e ON e.event_id = o.event_id
         JOIN session_runs r ON r.run_id = o.run_id
         WHERE {predicate}
           AND r.execution_mode = ?2 AND r.execution_mode_source = ?3
         ORDER BY CASE o.state WHEN 'ready' THEN 0 ELSE 1 END, o.outbox_id ASC
         LIMIT 1"
    );
    let raw: Option<RawOutboxCandidate> = transaction
        .query_row(
            &query,
            params![
                now,
                execution_mode_to_db(mode.mode),
                execution_mode_source_to_db(mode.source),
            ],
            raw_outbox_candidate,
        )
        .optional()?;
    raw.map(OutboxCandidate::try_from).transpose()
}

fn load_fence_for_lease(
    transaction: &Transaction<'_>,
    lease: &OutboxLease,
) -> LedgerResult<OutboxFence> {
    let raw: Option<RawOutboxFence> = transaction
        .query_row(
            "SELECT outbox_id, run_id, event_id, required, attempt_count, max_attempts,
                    lease_generation, lease_token, state
             FROM session_run_outbox WHERE outbox_id = ?1",
            [lease.outbox_id.to_string()],
            raw_outbox_fence,
        )
        .optional()?;
    let raw = raw.ok_or_else(|| LedgerError::StaleOutboxLease {
        outbox_id: lease.outbox_id.to_string(),
    })?;
    let fence = OutboxFence::try_from(raw)?;
    if fence.run_id != lease.run_id
        || fence.event_id != lease.event_id
        || fence.lease_generation != lease.lease_generation
        || fence.token != lease.token
    {
        return Err(LedgerError::StaleOutboxLease {
            outbox_id: lease.outbox_id.to_string(),
        });
    }
    Ok(fence)
}

struct NonResumableEffectRow {
    record: NonResumableEffectRecord,
    claim_token: Option<LeaseToken>,
}

fn load_nonresumable_effect(
    connection: &rusqlite::Connection,
    event_id: EventId,
) -> LedgerResult<Option<NonResumableEffectRow>> {
    let raw = connection
        .query_row(
            "SELECT run_id, state, idempotency_key, claim_token, claimed_at_ms,
                    result_payload_digest, result_event_id, terminal_at_ms
             FROM session_run_nonresumable_effects WHERE event_id = ?1",
            [event_id.to_string()],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<Vec<u8>>>(3)?,
                    row.get::<_, Option<i64>>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, Option<String>>(6)?,
                    row.get::<_, Option<i64>>(7)?,
                ))
            },
        )
        .optional()?;
    let Some((
        run_id,
        state,
        idempotency_key,
        claim_token,
        claimed_at_ms,
        result_payload_digest,
        result_event_id,
        terminal_at_ms,
    )) = raw
    else {
        return Ok(None);
    };
    Ok(Some(NonResumableEffectRow {
        record: NonResumableEffectRecord {
            run_id: parse_run_id(run_id)?,
            event_id,
            state: NonResumableEffectState::from_db(&state)?,
            idempotency_key: NonResumableIdempotencyKey::from_sha256_hex(idempotency_key)?,
            claimed_at_ms,
            result_payload_digest: result_payload_digest.map(Sha256Digest::new).transpose()?,
            result_event_id: result_event_id.map(parse_event_id).transpose()?,
            terminal_at_ms,
        },
        claim_token: claim_token.map(LeaseToken::from_blob).transpose()?,
    }))
}

fn validate_nonresumable_result_event(
    connection: &rusqlite::Connection,
    lease: &NonResumableEffectLease,
    result_event_id: EventId,
    expected_result_digest: &Sha256Digest,
) -> LedgerResult<()> {
    let raw = connection
        .query_row(
            "SELECT result.run_id, result.event_kind, result.append_key,
                    result.effect_intent, result.sealed_artifact_envelope_digest,
                    result.event_sequence, result.public_payload_jcs,
                    source.event_sequence, source.run_id, source.effect_intent
             FROM session_run_events AS result
             JOIN session_run_events AS source ON source.event_id = ?2
             WHERE result.event_id = ?1",
            params![result_event_id.to_string(), lease.event_id.to_string()],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, i64>(7)?,
                    row.get::<_, String>(8)?,
                    row.get::<_, String>(9)?,
                ))
            },
        )
        .optional()?;
    let mismatch = || LedgerError::NonResumableEffectResultMismatch {
        event_id: lease.event_id.to_string(),
        result_event_id: result_event_id.to_string(),
    };
    let Some((
        run_id,
        kind,
        append_key,
        effect_intent,
        envelope_digest,
        result_sequence,
        public_payload_jcs,
        source_sequence,
        source_run_id,
        source_effect_intent,
    )) = raw
    else {
        return Err(mismatch());
    };
    let claim_binding = derive_nonresumable_claim_binding(&lease.token)?;
    let expected_append_key =
        derive_nonresumable_result_append_key(lease.event_id, &claim_binding)?;
    let public_payload = CanonicalJson::from_jcs_text(public_payload_jcs)?;
    let payload_value = public_payload.as_value()?;
    let observed_payload = payload_value
        .as_object()
        .and_then(|object| object.get("payload"))
        .ok_or_else(mismatch)?;
    let expected_payload = CanonicalJson::from_value(&json!({
        "schema": NONRESUMABLE_RESULT_SCHEMA,
        "schema_version": RUN_EVENT_BINDING_SCHEMA_VERSION,
        "source_event_id": lease.event_id,
        "claim_binding": claim_binding,
        "payload": observed_payload,
    }))?;
    let actual_result_digest = Sha256Digest::new(sha256_hex(public_payload.as_str().as_bytes()))?;
    let matches = run_id == lease.run_id.to_string()
        && source_run_id == run_id
        && source_effect_intent == EffectIntent::LocalOneShotV1.as_str()
        && kind == SessionRunEventKind::Append.as_str()
        && append_key.as_deref() == Some(expected_append_key.as_str())
        && effect_intent == EffectIntent::None.as_str()
        && envelope_digest.is_some()
        && result_sequence > source_sequence
        && public_payload == expected_payload
        && actual_result_digest == *expected_result_digest;
    if !matches {
        return Err(mismatch());
    }
    Ok(())
}

fn add_duration(now: i64, duration: Duration) -> LedgerResult<i64> {
    let duration = i64::try_from(duration.as_millis()).map_err(|_| LedgerError::NumericOverflow)?;
    now.checked_add(duration)
        .ok_or(LedgerError::NumericOverflow)
}

