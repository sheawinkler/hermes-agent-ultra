struct ArtifactRefDbValues<'a> {
    envelope_digest: Option<&'a str>,
    key_id: Option<&'a str>,
    schema_version: Option<i64>,
    encryption_algorithm: Option<&'a str>,
    canonicalization_algorithm: Option<&'a str>,
    canonicalization_version: Option<i64>,
    redactor_version: Option<&'a str>,
}

impl<'a> From<Option<&'a SealedArtifactRef>> for ArtifactRefDbValues<'a> {
    fn from(reference: Option<&'a SealedArtifactRef>) -> Self {
        Self {
            envelope_digest: reference.map(|value| value.envelope_digest().as_str()),
            key_id: reference.map(|value| value.key_id().as_str()),
            schema_version: reference.map(|value| i64::from(value.schema_version())),
            encryption_algorithm: reference.map(SealedArtifactRef::encryption_algorithm),
            canonicalization_algorithm: reference
                .map(SealedArtifactRef::canonicalization_algorithm),
            canonicalization_version: reference
                .map(|value| i64::from(value.canonicalization_version())),
            redactor_version: reference.map(|value| value.redactor_version().as_str()),
        }
    }
}

struct ReservationRow {
    append_key: AppendKey,
    run_id: RunId,
    event_id: EventId,
    frozen_event_at_ms: i64,
    identity: AppendIdentity,
    effect_intent: EffectIntent,
    public_payload: CanonicalJson,
    sealed_artifact_ref: Option<SealedArtifactRef>,
    lease_generation: u64,
    token: LeaseToken,
    lease_expires_at_ms: i64,
    committed_event_id: Option<EventId>,
    terminal_audit_event_id: Option<EventId>,
}

struct RawReservation {
    append_key: String,
    run_id: String,
    event_id: String,
    frozen_event_at_ms: i64,
    public_identity_jcs: String,
    private_fingerprint_algorithm: String,
    private_fingerprint_algorithm_version: i64,
    private_fingerprint_key_id: String,
    private_fingerprint_key_version: i64,
    private_fingerprint: String,
    effect_intent: String,
    public_payload_jcs: String,
    sealed_artifact_envelope_digest: Option<String>,
    sealed_artifact_key_id: Option<String>,
    sealed_artifact_schema_version: Option<i64>,
    sealed_artifact_encryption_algorithm: Option<String>,
    sealed_artifact_canonicalization_algorithm: Option<String>,
    sealed_artifact_canonicalization_version: Option<i64>,
    sealed_artifact_redactor_version: Option<String>,
    lease_generation: i64,
    lease_token: Vec<u8>,
    lease_expires_at_ms: i64,
    committed_event_id: Option<String>,
    terminal_audit_event_id: Option<String>,
}

impl TryFrom<RawReservation> for ReservationRow {
    type Error = LedgerError;

    fn try_from(raw: RawReservation) -> Result<Self, Self::Error> {
        Ok(Self {
            append_key: AppendKey::new(raw.append_key)?,
            run_id: parse_run_id(raw.run_id)?,
            event_id: parse_event_id(raw.event_id)?,
            frozen_event_at_ms: raw.frozen_event_at_ms,
            identity: AppendIdentity {
                public_identity: CanonicalJson::from_jcs_text(raw.public_identity_jcs)?,
                private_fingerprint: PrivateFingerprint::from_db(
                    &raw.private_fingerprint_algorithm,
                    raw.private_fingerprint_algorithm_version,
                    raw.private_fingerprint_key_id,
                    raw.private_fingerprint_key_version,
                    raw.private_fingerprint,
                )?,
            },
            effect_intent: EffectIntent::from_db(&raw.effect_intent)?,
            public_payload: CanonicalJson::from_jcs_text(raw.public_payload_jcs)?,
            sealed_artifact_ref: parse_optional_artifact_ref(
                raw.sealed_artifact_envelope_digest,
                raw.sealed_artifact_key_id,
                raw.sealed_artifact_schema_version,
                raw.sealed_artifact_encryption_algorithm,
                raw.sealed_artifact_canonicalization_algorithm,
                raw.sealed_artifact_canonicalization_version,
                raw.sealed_artifact_redactor_version,
            )?,
            lease_generation: parse_positive_u64("append lease generation", raw.lease_generation)?,
            token: LeaseToken::from_blob(raw.lease_token)?,
            lease_expires_at_ms: raw.lease_expires_at_ms,
            committed_event_id: raw.committed_event_id.map(parse_event_id).transpose()?,
            terminal_audit_event_id: raw
                .terminal_audit_event_id
                .map(parse_event_id)
                .transpose()?,
        })
    }
}

struct OutboxCandidate {
    outbox_id: OutboxRowId,
    run_id: RunId,
    event_id: EventId,
    destination: Destination,
    required: bool,
    idempotency_key: DownstreamIdempotencyKey,
    public_payload: CanonicalJson,
    sealed_artifact_ref: SealedArtifactRef,
    attempt_count: u32,
    max_attempts: u32,
    lease_generation: u64,
    state: OutboxState,
    previous_token: Option<LeaseToken>,
    lease_expires_at_ms: Option<i64>,
}

impl OutboxCandidate {
    fn into_existing_fence(self) -> LedgerResult<OutboxFence> {
        if self.state != OutboxState::Leased || self.lease_expires_at_ms.is_none() {
            return Err(LedgerError::IntegrityViolation(
                "expired outbox terminalization candidate is not an active lease".to_string(),
            ));
        }
        Ok(OutboxFence {
            outbox_id: self.outbox_id,
            run_id: self.run_id,
            event_id: self.event_id,
            required: self.required,
            attempt_count: self.attempt_count,
            max_attempts: self.max_attempts,
            lease_generation: self.lease_generation,
            token: self.previous_token.ok_or_else(|| {
                LedgerError::IntegrityViolation(
                    "expired outbox terminalization candidate has no lease token".to_string(),
                )
            })?,
        })
    }
}

struct RawOutboxCandidate {
    outbox_id: String,
    run_id: String,
    event_id: String,
    destination: String,
    required: i64,
    idempotency_key: String,
    public_payload: String,
    sealed_artifact_envelope_digest: String,
    sealed_artifact_key_id: String,
    sealed_artifact_schema_version: i64,
    sealed_artifact_encryption_algorithm: String,
    sealed_artifact_canonicalization_algorithm: String,
    sealed_artifact_canonicalization_version: i64,
    sealed_artifact_redactor_version: String,
    event_sealed_artifact_envelope_digest: Option<String>,
    event_sealed_artifact_key_id: Option<String>,
    event_sealed_artifact_schema_version: Option<i64>,
    event_sealed_artifact_encryption_algorithm: Option<String>,
    event_sealed_artifact_canonicalization_algorithm: Option<String>,
    event_sealed_artifact_canonicalization_version: Option<i64>,
    event_sealed_artifact_redactor_version: Option<String>,
    attempt_count: i64,
    max_attempts: i64,
    lease_generation: i64,
    state: String,
    lease_token: Option<Vec<u8>>,
    lease_expires_at_ms: Option<i64>,
}

impl TryFrom<RawOutboxCandidate> for OutboxCandidate {
    type Error = LedgerError;

    fn try_from(raw: RawOutboxCandidate) -> Result<Self, Self::Error> {
        let sealed_artifact_ref = SealedArtifactRef::from_db(
            raw.sealed_artifact_envelope_digest,
            raw.sealed_artifact_key_id,
            raw.sealed_artifact_schema_version,
            raw.sealed_artifact_encryption_algorithm,
            raw.sealed_artifact_canonicalization_algorithm,
            raw.sealed_artifact_canonicalization_version,
            raw.sealed_artifact_redactor_version,
        )?;
        let event_ref = parse_optional_artifact_ref(
            raw.event_sealed_artifact_envelope_digest,
            raw.event_sealed_artifact_key_id,
            raw.event_sealed_artifact_schema_version,
            raw.event_sealed_artifact_encryption_algorithm,
            raw.event_sealed_artifact_canonicalization_algorithm,
            raw.event_sealed_artifact_canonicalization_version,
            raw.event_sealed_artifact_redactor_version,
        )?;
        if event_ref.as_ref() != Some(&sealed_artifact_ref) {
            return Err(LedgerError::IntegrityViolation(
                "outbox sealed artifact reference differs from originating event".to_string(),
            ));
        }
        let state = OutboxState::from_db(&raw.state)?;
        let token = raw.lease_token.map(LeaseToken::from_blob).transpose()?;
        if (state == OutboxState::Leased) != (token.is_some() && raw.lease_expires_at_ms.is_some())
        {
            return Err(LedgerError::InvalidStoredState {
                kind: "outbox lease fields",
                value: "state and lease fields disagree".to_string(),
            });
        }
        Ok(Self {
            outbox_id: parse_outbox_id(raw.outbox_id)?,
            run_id: parse_run_id(raw.run_id)?,
            event_id: parse_event_id(raw.event_id)?,
            destination: Destination::from_db(&raw.destination)?,
            required: parse_bool("outbox required", raw.required)?,
            idempotency_key: DownstreamIdempotencyKey::from_sha256_hex(raw.idempotency_key)?,
            public_payload: CanonicalJson::from_jcs_text(raw.public_payload)?,
            sealed_artifact_ref,
            attempt_count: parse_nonnegative_u32("outbox attempt count", raw.attempt_count)?,
            max_attempts: parse_positive_u32("outbox maximum attempts", raw.max_attempts)?,
            lease_generation: parse_nonnegative_u64(
                "outbox lease generation",
                raw.lease_generation,
            )?,
            state,
            previous_token: token,
            lease_expires_at_ms: raw.lease_expires_at_ms,
        })
    }
}

struct OutboxFence {
    outbox_id: OutboxRowId,
    run_id: RunId,
    event_id: EventId,
    required: bool,
    attempt_count: u32,
    max_attempts: u32,
    lease_generation: u64,
    token: LeaseToken,
}

struct RawOutboxFence {
    outbox_id: String,
    run_id: String,
    event_id: String,
    required: i64,
    attempt_count: i64,
    max_attempts: i64,
    lease_generation: i64,
    lease_token: Option<Vec<u8>>,
    state: String,
}

impl TryFrom<RawOutboxFence> for OutboxFence {
    type Error = LedgerError;

    fn try_from(raw: RawOutboxFence) -> Result<Self, Self::Error> {
        if raw.state != OutboxState::Leased.as_str() {
            return Err(LedgerError::StaleOutboxLease {
                outbox_id: raw.outbox_id,
            });
        }
        let outbox_id = parse_outbox_id(raw.outbox_id.clone())?;
        let token = raw
            .lease_token
            .ok_or_else(|| LedgerError::StaleOutboxLease {
                outbox_id: raw.outbox_id,
            })?;
        Ok(Self {
            outbox_id,
            run_id: parse_run_id(raw.run_id)?,
            event_id: parse_event_id(raw.event_id)?,
            required: parse_bool("outbox required", raw.required)?,
            attempt_count: parse_nonnegative_u32("outbox attempt count", raw.attempt_count)?,
            max_attempts: parse_positive_u32("outbox maximum attempts", raw.max_attempts)?,
            lease_generation: parse_nonnegative_u64(
                "outbox lease generation",
                raw.lease_generation,
            )?,
            token: LeaseToken::from_blob(token)?,
        })
    }
}

struct RawRunStatus {
    exit_event_id: Option<String>,
    exit_terminal_reason: Option<String>,
    candidate_outcome: Option<String>,
    final_outcome: Option<String>,
    required_dead_lettered: i64,
    pending_required_delivery: i64,
    exact_replay: i64,
    exact_replay_state: String,
    exact_replay_reason: Option<String>,
    exact_replay_transition_sequence: i64,
    exact_replay_latched_at_ms: Option<i64>,
    exact_replay_latched_by_event_id: Option<String>,
}

impl TryFrom<(RunId, RawRunStatus)> for RunStatus {
    type Error = LedgerError;

    fn try_from((run_id, raw): (RunId, RawRunStatus)) -> Result<Self, Self::Error> {
        Ok(Self {
            run_id,
            exit_event_id: raw.exit_event_id.map(parse_event_id).transpose()?,
            exit_terminal_reason: raw
                .exit_terminal_reason
                .map(|value| TerminalReason::from_db(&value))
                .transpose()?,
            candidate_outcome: raw
                .candidate_outcome
                .map(|value| RunOutcome::from_db(&value))
                .transpose()?,
            final_outcome: raw
                .final_outcome
                .map(|value| RunOutcome::from_db(&value))
                .transpose()?,
            required_dead_lettered: parse_bool(
                "required dead letter latch",
                raw.required_dead_lettered,
            )?,
            pending_required_delivery: parse_bool(
                "pending required delivery",
                raw.pending_required_delivery,
            )?,
            exact_replay: {
                let state = ExactReplayState::from_db(&raw.exact_replay_state)?;
                let reason = raw
                    .exact_replay_reason
                    .as_deref()
                    .map(ArtifactStateReason::from_db)
                    .transpose()?;
                validate_exact_replay_state_reason(state, reason)?;
                let exact_replay = parse_sql_bool(raw.exact_replay, "exact replay")?;
                if exact_replay != state.exact_replay() {
                    return Err(LedgerError::InvalidStoredState {
                        kind: "exact replay latch",
                        value: raw.exact_replay_state,
                    });
                }
                ExactReplayStatus {
                    state,
                    reason,
                    exact_replay,
                    transition_sequence: parse_nonnegative_u64(
                        "exact replay transition sequence",
                        raw.exact_replay_transition_sequence,
                    )?,
                    latched_at_ms: raw.exact_replay_latched_at_ms,
                    latched_by_event_id: raw
                        .exact_replay_latched_by_event_id
                        .map(parse_event_id)
                        .transpose()?,
                }
            },
        })
    }
}

struct RawRunHeader {
    start_event_id: String,
    execution_mode: String,
    execution_mode_source: String,
    started_at_ms: i64,
    owner_id: String,
    owner_lease_generation: i64,
    owner_lease_expires_at_ms: i64,
    provenance_canonicalization_algorithm: String,
    provenance_canonicalization_version: i64,
    session_id: String,
    parent_run_id: Option<String>,
    branch_id: String,
    tenant_id: String,
    action_id: String,
    objective_id: String,
    task_id: String,
    model_id: String,
    provider_id: String,
    config_digest: String,
    prompt_digest: Option<String>,
    prompt_digest_absence: Option<String>,
    input_digest: Option<String>,
    input_digest_absence: Option<String>,
    output_digest: Option<String>,
    output_digest_absence: Option<String>,
    tool_digest: Option<String>,
    tool_digest_absence: Option<String>,
    skill_digest: Option<String>,
    skill_digest_absence: Option<String>,
    policy_digest: Option<String>,
    policy_digest_absence: Option<String>,
    sandbox_digest: Option<String>,
    sandbox_digest_absence: Option<String>,
}

impl TryFrom<(RunId, RawRunHeader)> for RunHeader {
    type Error = LedgerError;

    fn try_from((run_id, raw): (RunId, RawRunHeader)) -> Result<Self, Self::Error> {
        Ok(Self {
            run_id,
            start_event_id: parse_event_id(raw.start_event_id)?,
            execution_mode: execution_mode_from_db(
                &raw.execution_mode,
                &raw.execution_mode_source,
            )?,
            started_at_ms: raw.started_at_ms,
            owner_id: RunOwnerId::new(raw.owner_id)?,
            owner_lease_generation: parse_positive_u64(
                "run owner lease generation",
                raw.owner_lease_generation,
            )?,
            owner_lease_expires_at_ms: raw.owner_lease_expires_at_ms,
            provenance: RunProvenance::from_db(
                raw.provenance_canonicalization_algorithm,
                raw.provenance_canonicalization_version,
                raw.session_id,
                raw.parent_run_id,
                raw.branch_id,
                raw.tenant_id,
                raw.action_id,
                raw.objective_id,
                raw.task_id,
                raw.model_id,
                raw.provider_id,
                raw.config_digest,
                raw.prompt_digest,
                raw.prompt_digest_absence,
                raw.input_digest,
                raw.input_digest_absence,
                raw.output_digest,
                raw.output_digest_absence,
                raw.tool_digest,
                raw.tool_digest_absence,
                raw.skill_digest,
                raw.skill_digest_absence,
                raw.policy_digest,
                raw.policy_digest_absence,
                raw.sandbox_digest,
                raw.sandbox_digest_absence,
            )?,
        })
    }
}

struct RawEvent {
    event_id: String,
    run_id: String,
    sequence: i64,
    kind: String,
    append_key: Option<String>,
    effect_intent: String,
    artifact_degradation_reason: Option<String>,
    public_identity: Option<String>,
    fingerprint_algorithm: Option<String>,
    fingerprint_algorithm_version: Option<i64>,
    fingerprint_key_id: Option<String>,
    fingerprint_key_version: Option<i64>,
    fingerprint: Option<String>,
    sealed_artifact_envelope_digest: Option<String>,
    sealed_artifact_key_id: Option<String>,
    sealed_artifact_schema_version: Option<i64>,
    sealed_artifact_encryption_algorithm: Option<String>,
    sealed_artifact_canonicalization_algorithm: Option<String>,
    sealed_artifact_canonicalization_version: Option<i64>,
    sealed_artifact_redactor_version: Option<String>,
    run_provenance: Option<String>,
    public_payload: String,
    manifest_hash: String,
    previous_hash: String,
    event_hash: String,
    created_at_ms: i64,
}

impl TryFrom<RawEvent> for SessionRunEvent {
    type Error = LedgerError;

    fn try_from(raw: RawEvent) -> Result<Self, Self::Error> {
        let private_fingerprint = match (
            raw.fingerprint_algorithm,
            raw.fingerprint_algorithm_version,
            raw.fingerprint_key_id,
            raw.fingerprint_key_version,
            raw.fingerprint,
        ) {
            (None, None, None, None, None) => None,
            (
                Some(algorithm),
                Some(algorithm_version),
                Some(key_id),
                Some(key_version),
                Some(fingerprint),
            ) => Some(PrivateFingerprint::from_db(
                &algorithm,
                algorithm_version,
                key_id,
                key_version,
                fingerprint,
            )?),
            _ => {
                return Err(LedgerError::InvalidStoredState {
                    kind: "event private fingerprint metadata",
                    value: "partially populated".to_string(),
                });
            }
        };
        let public_identity = raw
            .public_identity
            .map(CanonicalJson::from_jcs_text)
            .transpose()?;
        if public_identity.is_some() != private_fingerprint.is_some() {
            return Err(LedgerError::InvalidStoredState {
                kind: "event append identity",
                value: "public identity and fingerprint must be present together".to_string(),
            });
        }
        let append_key = raw.append_key.map(AppendKey::new).transpose()?;
        let effect_intent = EffectIntent::from_db(&raw.effect_intent)?;
        let artifact_degradation_reason = raw
            .artifact_degradation_reason
            .map(|value| ArtifactDegradationReason::from_db(&value))
            .transpose()?;
        let sealed_artifact_ref = parse_optional_artifact_ref(
            raw.sealed_artifact_envelope_digest,
            raw.sealed_artifact_key_id,
            raw.sealed_artifact_schema_version,
            raw.sealed_artifact_encryption_algorithm,
            raw.sealed_artifact_canonicalization_algorithm,
            raw.sealed_artifact_canonicalization_version,
            raw.sealed_artifact_redactor_version,
        )?;
        let public_payload = CanonicalJson::from_jcs_text(raw.public_payload)?;
        let kind = SessionRunEventKind::from_db(&raw.kind)?;
        let valid_kind_state = match kind {
            SessionRunEventKind::Append => {
                append_key.is_some()
                    && public_identity.is_some()
                    && artifact_degradation_reason.is_none()
            }
            SessionRunEventKind::ManualAppendDegradedAudit => {
                append_key.is_some()
                    && public_identity.is_none()
                    && private_fingerprint.is_none()
                    && effect_intent == EffectIntent::None
                    && artifact_degradation_reason.is_some()
                    && sealed_artifact_ref.is_none()
            }
            SessionRunEventKind::DeliveryResult => {
                append_key.is_none()
                    && effect_intent == EffectIntent::None
                    && artifact_degradation_reason.is_none()
                    && ((public_identity.is_none()
                        && private_fingerprint.is_none()
                        && sealed_artifact_ref.is_none())
                        || (public_identity.is_some()
                            && private_fingerprint.is_some()
                            && sealed_artifact_ref.is_some()))
            }
            SessionRunEventKind::RunStart => {
                append_key.is_none()
                    && artifact_degradation_reason.is_none()
                    && ((public_identity.is_none()
                        && private_fingerprint.is_none()
                        && sealed_artifact_ref.is_none())
                        || (public_identity.is_some()
                            && private_fingerprint.is_some()
                            && sealed_artifact_ref.is_some()))
            }
            SessionRunEventKind::RunExit => {
                append_key.is_none()
                    && public_identity.is_none()
                    && private_fingerprint.is_none()
                    && artifact_degradation_reason.is_none()
                    && sealed_artifact_ref.is_none()
            }
            SessionRunEventKind::ArtifactStateAudit => {
                append_key.is_none()
                    && public_identity.is_none()
                    && private_fingerprint.is_none()
                    && effect_intent == EffectIntent::None
                    && artifact_degradation_reason.is_none()
                    && sealed_artifact_ref.is_none()
                    && validate_artifact_state_audit_payload(&public_payload).is_ok()
            }
        };
        if !valid_kind_state {
            return Err(LedgerError::InvalidStoredState {
                kind: "event kind fields",
                value: kind.as_str().to_string(),
            });
        }
        if kind == SessionRunEventKind::DeliveryResult {
            validate_delivery_result_storage_shape(&public_payload, public_identity.is_some())?;
        }
        let run_provenance = raw
            .run_provenance
            .map(CanonicalJson::from_jcs_text)
            .transpose()?;
        if (kind == SessionRunEventKind::RunStart) != run_provenance.is_some() {
            return Err(LedgerError::InvalidStoredState {
                kind: "event run provenance",
                value: "only run_start may contain provenance".to_string(),
            });
        }
        Ok(Self {
            event_id: parse_event_id(raw.event_id)?,
            run_id: parse_run_id(raw.run_id)?,
            sequence: parse_positive_u64("event sequence", raw.sequence)?,
            kind,
            append_key,
            effect_intent,
            artifact_degradation_reason,
            public_identity,
            private_fingerprint,
            sealed_artifact_ref,
            run_provenance,
            public_payload,
            manifest_hash: validated_digest_string("manifest hash", raw.manifest_hash)?,
            previous_hash: validated_digest_string("previous hash", raw.previous_hash)?,
            event_hash: validated_digest_string("event hash", raw.event_hash)?,
            created_at_ms: raw.created_at_ms,
        })
    }
}

struct RawOutboxRecord {
    outbox_id: String,
    run_id: String,
    event_id: String,
    destination: String,
    required: i64,
    idempotency_key: String,
    public_payload: String,
    sealed_artifact_envelope_digest: String,
    sealed_artifact_key_id: String,
    sealed_artifact_schema_version: i64,
    sealed_artifact_encryption_algorithm: String,
    sealed_artifact_canonicalization_algorithm: String,
    sealed_artifact_canonicalization_version: i64,
    sealed_artifact_redactor_version: String,
    state: String,
    attempt_count: i64,
    max_attempts: i64,
    lease_generation: i64,
    delivery_receipt: Option<String>,
    delivery_error: Option<String>,
}

impl TryFrom<RawOutboxRecord> for OutboxRecord {
    type Error = LedgerError;

    fn try_from(raw: RawOutboxRecord) -> Result<Self, Self::Error> {
        Ok(Self {
            outbox_id: parse_outbox_id(raw.outbox_id)?,
            run_id: parse_run_id(raw.run_id)?,
            event_id: parse_event_id(raw.event_id)?,
            destination: Destination::from_db(&raw.destination)?,
            idempotency_key: DownstreamIdempotencyKey::from_sha256_hex(raw.idempotency_key)?,
            required: parse_bool("outbox required", raw.required)?,
            public_payload: CanonicalJson::from_jcs_text(raw.public_payload)?,
            sealed_artifact_ref: SealedArtifactRef::from_db(
                raw.sealed_artifact_envelope_digest,
                raw.sealed_artifact_key_id,
                raw.sealed_artifact_schema_version,
                raw.sealed_artifact_encryption_algorithm,
                raw.sealed_artifact_canonicalization_algorithm,
                raw.sealed_artifact_canonicalization_version,
                raw.sealed_artifact_redactor_version,
            )?,
            state: OutboxState::from_db(&raw.state)?,
            attempt_count: parse_nonnegative_u32("outbox attempt count", raw.attempt_count)?,
            max_attempts: parse_positive_u32("outbox maximum attempts", raw.max_attempts)?,
            lease_generation: parse_nonnegative_u64(
                "outbox lease generation",
                raw.lease_generation,
            )?,
            delivery_receipt: raw
                .delivery_receipt
                .map(DurableDeliveryReceipt::new)
                .transpose()?,
            delivery_error: raw
                .delivery_error
                .map(|value| DeliveryFailureReason::from_db(&value))
                .transpose()?,
        })
    }
}

struct RawAppendReceipt {
    event_id: String,
    run_id: String,
    event_sequence: i64,
    event_hash: String,
    sealed_artifact_envelope_digest: Option<String>,
    sealed_artifact_key_id: Option<String>,
    sealed_artifact_schema_version: Option<i64>,
    sealed_artifact_encryption_algorithm: Option<String>,
    sealed_artifact_canonicalization_algorithm: Option<String>,
    sealed_artifact_canonicalization_version: Option<i64>,
    sealed_artifact_redactor_version: Option<String>,
    event_kind: String,
    artifact_degradation_reason: Option<String>,
}

fn raw_reservation(row: &Row<'_>) -> rusqlite::Result<RawReservation> {
    Ok(RawReservation {
        append_key: row.get(0)?,
        run_id: row.get(1)?,
        event_id: row.get(2)?,
        frozen_event_at_ms: row.get(3)?,
        public_identity_jcs: row.get(4)?,
        private_fingerprint_algorithm: row.get(5)?,
        private_fingerprint_algorithm_version: row.get(6)?,
        private_fingerprint_key_id: row.get(7)?,
        private_fingerprint_key_version: row.get(8)?,
        private_fingerprint: row.get(9)?,
        effect_intent: row.get(10)?,
        public_payload_jcs: row.get(11)?,
        sealed_artifact_envelope_digest: row.get(12)?,
        sealed_artifact_key_id: row.get(13)?,
        sealed_artifact_schema_version: row.get(14)?,
        sealed_artifact_encryption_algorithm: row.get(15)?,
        sealed_artifact_canonicalization_algorithm: row.get(16)?,
        sealed_artifact_canonicalization_version: row.get(17)?,
        sealed_artifact_redactor_version: row.get(18)?,
        lease_generation: row.get(19)?,
        lease_token: row.get(20)?,
        lease_expires_at_ms: row.get(21)?,
        committed_event_id: row.get(22)?,
        terminal_audit_event_id: row.get(23)?,
    })
}

fn raw_outbox_candidate(row: &Row<'_>) -> rusqlite::Result<RawOutboxCandidate> {
    Ok(RawOutboxCandidate {
        outbox_id: row.get(0)?,
        run_id: row.get(1)?,
        event_id: row.get(2)?,
        destination: row.get(3)?,
        required: row.get(4)?,
        idempotency_key: row.get(5)?,
        public_payload: row.get(6)?,
        sealed_artifact_envelope_digest: row.get(7)?,
        sealed_artifact_key_id: row.get(8)?,
        sealed_artifact_schema_version: row.get(9)?,
        sealed_artifact_encryption_algorithm: row.get(10)?,
        sealed_artifact_canonicalization_algorithm: row.get(11)?,
        sealed_artifact_canonicalization_version: row.get(12)?,
        sealed_artifact_redactor_version: row.get(13)?,
        event_sealed_artifact_envelope_digest: row.get(14)?,
        event_sealed_artifact_key_id: row.get(15)?,
        event_sealed_artifact_schema_version: row.get(16)?,
        event_sealed_artifact_encryption_algorithm: row.get(17)?,
        event_sealed_artifact_canonicalization_algorithm: row.get(18)?,
        event_sealed_artifact_canonicalization_version: row.get(19)?,
        event_sealed_artifact_redactor_version: row.get(20)?,
        attempt_count: row.get(21)?,
        max_attempts: row.get(22)?,
        lease_generation: row.get(23)?,
        state: row.get(24)?,
        lease_token: row.get(25)?,
        lease_expires_at_ms: row.get(26)?,
    })
}

fn raw_outbox_fence(row: &Row<'_>) -> rusqlite::Result<RawOutboxFence> {
    Ok(RawOutboxFence {
        outbox_id: row.get(0)?,
        run_id: row.get(1)?,
        event_id: row.get(2)?,
        required: row.get(3)?,
        attempt_count: row.get(4)?,
        max_attempts: row.get(5)?,
        lease_generation: row.get(6)?,
        lease_token: row.get(7)?,
        state: row.get(8)?,
    })
}

fn raw_run_status(row: &Row<'_>) -> rusqlite::Result<RawRunStatus> {
    Ok(RawRunStatus {
        exit_event_id: row.get(0)?,
        exit_terminal_reason: row.get(1)?,
        candidate_outcome: row.get(2)?,
        final_outcome: row.get(3)?,
        required_dead_lettered: row.get(4)?,
        pending_required_delivery: row.get(5)?,
        exact_replay: row.get(6)?,
        exact_replay_state: row.get(7)?,
        exact_replay_reason: row.get(8)?,
        exact_replay_transition_sequence: row.get(9)?,
        exact_replay_latched_at_ms: row.get(10)?,
        exact_replay_latched_by_event_id: row.get(11)?,
    })
}

fn raw_run_header(row: &Row<'_>) -> rusqlite::Result<RawRunHeader> {
    Ok(RawRunHeader {
        start_event_id: row.get(0)?,
        execution_mode: row.get(1)?,
        execution_mode_source: row.get(2)?,
        started_at_ms: row.get(3)?,
        owner_id: row.get(4)?,
        owner_lease_generation: row.get(5)?,
        owner_lease_expires_at_ms: row.get(6)?,
        provenance_canonicalization_algorithm: row.get(7)?,
        provenance_canonicalization_version: row.get(8)?,
        session_id: row.get(9)?,
        parent_run_id: row.get(10)?,
        branch_id: row.get(11)?,
        tenant_id: row.get(12)?,
        action_id: row.get(13)?,
        objective_id: row.get(14)?,
        task_id: row.get(15)?,
        model_id: row.get(16)?,
        provider_id: row.get(17)?,
        config_digest: row.get(18)?,
        prompt_digest: row.get(19)?,
        prompt_digest_absence: row.get(20)?,
        input_digest: row.get(21)?,
        input_digest_absence: row.get(22)?,
        output_digest: row.get(23)?,
        output_digest_absence: row.get(24)?,
        tool_digest: row.get(25)?,
        tool_digest_absence: row.get(26)?,
        skill_digest: row.get(27)?,
        skill_digest_absence: row.get(28)?,
        policy_digest: row.get(29)?,
        policy_digest_absence: row.get(30)?,
        sandbox_digest: row.get(31)?,
        sandbox_digest_absence: row.get(32)?,
    })
}

fn raw_event(row: &Row<'_>) -> rusqlite::Result<RawEvent> {
    Ok(RawEvent {
        event_id: row.get(0)?,
        run_id: row.get(1)?,
        sequence: row.get(2)?,
        kind: row.get(3)?,
        append_key: row.get(4)?,
        effect_intent: row.get(5)?,
        artifact_degradation_reason: row.get(6)?,
        public_identity: row.get(7)?,
        fingerprint_algorithm: row.get(8)?,
        fingerprint_algorithm_version: row.get(9)?,
        fingerprint_key_id: row.get(10)?,
        fingerprint_key_version: row.get(11)?,
        fingerprint: row.get(12)?,
        sealed_artifact_envelope_digest: row.get(13)?,
        sealed_artifact_key_id: row.get(14)?,
        sealed_artifact_schema_version: row.get(15)?,
        sealed_artifact_encryption_algorithm: row.get(16)?,
        sealed_artifact_canonicalization_algorithm: row.get(17)?,
        sealed_artifact_canonicalization_version: row.get(18)?,
        sealed_artifact_redactor_version: row.get(19)?,
        run_provenance: row.get(20)?,
        public_payload: row.get(21)?,
        manifest_hash: row.get(22)?,
        previous_hash: row.get(23)?,
        event_hash: row.get(24)?,
        created_at_ms: row.get(25)?,
    })
}

fn raw_outbox_record(row: &Row<'_>) -> rusqlite::Result<RawOutboxRecord> {
    Ok(RawOutboxRecord {
        outbox_id: row.get(0)?,
        run_id: row.get(1)?,
        event_id: row.get(2)?,
        destination: row.get(3)?,
        required: row.get(4)?,
        idempotency_key: row.get(5)?,
        public_payload: row.get(6)?,
        sealed_artifact_envelope_digest: row.get(7)?,
        sealed_artifact_key_id: row.get(8)?,
        sealed_artifact_schema_version: row.get(9)?,
        sealed_artifact_encryption_algorithm: row.get(10)?,
        sealed_artifact_canonicalization_algorithm: row.get(11)?,
        sealed_artifact_canonicalization_version: row.get(12)?,
        sealed_artifact_redactor_version: row.get(13)?,
        state: row.get(14)?,
        attempt_count: row.get(15)?,
        max_attempts: row.get(16)?,
        lease_generation: row.get(17)?,
        delivery_receipt: row.get(18)?,
        delivery_error: row.get(19)?,
    })
}

fn raw_append_receipt(row: &Row<'_>) -> rusqlite::Result<RawAppendReceipt> {
    Ok(RawAppendReceipt {
        event_id: row.get(0)?,
        run_id: row.get(1)?,
        event_sequence: row.get(2)?,
        event_hash: row.get(3)?,
        sealed_artifact_envelope_digest: row.get(4)?,
        sealed_artifact_key_id: row.get(5)?,
        sealed_artifact_schema_version: row.get(6)?,
        sealed_artifact_encryption_algorithm: row.get(7)?,
        sealed_artifact_canonicalization_algorithm: row.get(8)?,
        sealed_artifact_canonicalization_version: row.get(9)?,
        sealed_artifact_redactor_version: row.get(10)?,
        event_kind: row.get(11)?,
        artifact_degradation_reason: row.get(12)?,
    })
}

fn load_outbox_records_for_run(
    connection: &rusqlite::Connection,
    run_id: RunId,
) -> LedgerResult<Vec<OutboxRecord>> {
    let mut statement = connection.prepare(
        "SELECT outbox_id, run_id, event_id, destination, required, idempotency_key,
                public_payload_jcs,
                sealed_artifact_envelope_digest, sealed_artifact_key_id,
                sealed_artifact_schema_version, sealed_artifact_encryption_algorithm,
                sealed_artifact_canonicalization_algorithm,
                sealed_artifact_canonicalization_version, sealed_artifact_redactor_version,
                state, attempt_count, max_attempts, lease_generation,
                delivery_receipt, delivery_error
         FROM session_run_outbox WHERE run_id = ?1 ORDER BY event_id, manifest_order",
    )?;
    let rows = statement
        .query_map([run_id.to_string()], raw_outbox_record)?
        .collect::<Result<Vec<_>, _>>()?;
    rows.into_iter().map(OutboxRecord::try_from).collect()
}

fn load_outbox_records_for_event(
    connection: &rusqlite::Connection,
    event_id: EventId,
) -> LedgerResult<Vec<OutboxRecord>> {
    let mut statement = connection.prepare(
        "SELECT outbox_id, run_id, event_id, destination, required, idempotency_key,
                public_payload_jcs,
                sealed_artifact_envelope_digest, sealed_artifact_key_id,
                sealed_artifact_schema_version, sealed_artifact_encryption_algorithm,
                sealed_artifact_canonicalization_algorithm,
                sealed_artifact_canonicalization_version, sealed_artifact_redactor_version,
                state, attempt_count, max_attempts, lease_generation,
                delivery_receipt, delivery_error
         FROM session_run_outbox WHERE event_id = ?1 ORDER BY manifest_order",
    )?;
    let rows = statement
        .query_map([event_id.to_string()], raw_outbox_record)?
        .collect::<Result<Vec<_>, _>>()?;
    rows.into_iter().map(OutboxRecord::try_from).collect()
}

pub(crate) fn verify_event_delivery_projection(
    event: &SessionRunEvent,
    manifest_jcs: &CanonicalJson,
    manifest_hash: &str,
    actual_outbox: &[OutboxRecord],
) -> LedgerResult<()> {
    let expected = DispatchPolicyV1::derive(
        event.event_id,
        event.effect_intent,
        &event.public_payload,
        event.sealed_artifact_ref.as_ref(),
    )
    .map_err(|error| LedgerError::IntegrityViolation(error.to_string()))?;
    verify_event_delivery_projection_with_manifest(
        event,
        manifest_jcs,
        manifest_hash,
        &expected,
        actual_outbox.iter(),
    )
}

fn verify_event_delivery_projection_with_manifest<'a>(
    event: &SessionRunEvent,
    manifest_jcs: &CanonicalJson,
    manifest_hash: &str,
    expected: &DeliveryManifest,
    actual_outbox: impl Iterator<Item = &'a OutboxRecord>,
) -> LedgerResult<()> {
    if manifest_hash != event.manifest_hash
        || sha256_hex(manifest_jcs.as_str().as_bytes()) != manifest_hash
        || *manifest_jcs != expected.canonical_json
    {
        return Err(LedgerError::IntegrityViolation(
            "event manifest does not equal the policy-derived canonical manifest".to_string(),
        ));
    }
    let mut actual_outbox = actual_outbox;
    for expected in &expected.entries {
        let actual = actual_outbox.next().ok_or_else(|| {
            LedgerError::IntegrityViolation(
                "event outbox cardinality does not equal its manifest".to_string(),
            )
        })?;
        if actual.run_id != event.run_id
            || actual.event_id != event.event_id
            || actual.destination != expected.destination
            || actual.required != expected.required
            || actual.idempotency_key != expected.idempotency_key
            || actual.public_payload != expected.public_payload
            || actual.sealed_artifact_ref != expected.sealed_artifact_ref
            || event.sealed_artifact_ref.as_ref() != Some(&actual.sealed_artifact_ref)
        {
            return Err(LedgerError::IntegrityViolation(
                "outbox reference or projection does not equal the event manifest".to_string(),
            ));
        }
    }
    if actual_outbox.next().is_some() {
        return Err(LedgerError::IntegrityViolation(
            "event outbox cardinality does not equal its manifest".to_string(),
        ));
    }
    Ok(())
}

pub(crate) fn verify_event_nonresumable_projection(
    event: &SessionRunEvent,
    effects: &[NonResumableEffectSnapshot],
    all_events: &[SessionRunEvent],
) -> LedgerResult<()> {
    let matching = effects
        .iter()
        .filter(|effect| effect.record.event_id == event.event_id)
        .collect::<Vec<_>>();
    if event.effect_intent != EffectIntent::LocalOneShotV1 {
        if matching.is_empty() {
            return Ok(());
        }
        return Err(LedgerError::IntegrityViolation(
            "non-local event has a non-resumable effect projection".to_string(),
        ));
    }
    if matching.len() != 1 {
        return Err(LedgerError::IntegrityViolation(
            "local one-shot event must have exactly one effect projection".to_string(),
        ));
    }
    let effect = matching[0];
    let record = &effect.record;
    if record.run_id != event.run_id
        || record.idempotency_key != derive_nonresumable_idempotency_key(event.event_id)?
    {
        return Err(LedgerError::IntegrityViolation(
            "non-resumable effect identity does not match its source event".to_string(),
        ));
    }
    let shape_valid = match record.state {
        NonResumableEffectState::Armed => {
            effect.claim_binding.is_none()
                && record.claimed_at_ms.is_none()
                && record.result_payload_digest.is_none()
                && record.result_event_id.is_none()
                && record.terminal_at_ms.is_none()
        }
        NonResumableEffectState::Executing => {
            effect.claim_binding.is_some()
                && record.claimed_at_ms.is_some()
                && record.result_event_id.is_none()
                && record.terminal_at_ms.is_none()
        }
        NonResumableEffectState::Completed => {
            effect.claim_binding.is_some()
                && record.claimed_at_ms.is_some()
                && record.result_payload_digest.is_some()
                && record.result_event_id.is_some()
                && record.terminal_at_ms.is_some()
        }
        NonResumableEffectState::Abandoned => {
            effect.claim_binding.is_none()
                && record.claimed_at_ms.is_none()
                && record.result_payload_digest.is_none()
                && record.result_event_id.is_none()
                && record.terminal_at_ms.is_some()
        }
        NonResumableEffectState::UnknownNonResumable => {
            effect.claim_binding.is_some()
                && record.claimed_at_ms.is_some()
                && record.result_event_id.is_none()
                && record.terminal_at_ms.is_some()
        }
    };
    if !shape_valid {
        return Err(LedgerError::IntegrityViolation(
            "non-resumable effect state has inconsistent evidence fields".to_string(),
        ));
    }
    if record.state == NonResumableEffectState::Completed {
        let result_event_id = record.result_event_id.ok_or_else(|| {
            LedgerError::IntegrityViolation(
                "completed non-resumable effect lacks a result event".to_string(),
            )
        })?;
        let result = all_events
            .iter()
            .find(|candidate| candidate.event_id == result_event_id)
            .ok_or_else(|| {
                LedgerError::IntegrityViolation(
                    "non-resumable result event is outside the source run".to_string(),
                )
            })?;
        let claim_binding = effect.claim_binding.as_ref().ok_or_else(|| {
            LedgerError::IntegrityViolation(
                "completed non-resumable effect lacks a claim binding".to_string(),
            )
        })?;
        let result_payload_digest = record.result_payload_digest.as_ref().ok_or_else(|| {
            LedgerError::IntegrityViolation(
                "completed non-resumable effect lacks a result commitment".to_string(),
            )
        })?;
        verify_nonresumable_result_projection(event, result, claim_binding, result_payload_digest)?;
    }
    Ok(())
}

fn verify_nonresumable_result_projection(
    source: &SessionRunEvent,
    result: &SessionRunEvent,
    claim_binding: &Sha256Digest,
    result_payload_digest: &Sha256Digest,
) -> LedgerResult<()> {
    let expected_append_key =
        derive_nonresumable_result_append_key(source.event_id, claim_binding)?;
    let payload_value = result.public_payload.as_value()?;
    let observed_payload = payload_value
        .as_object()
        .and_then(|object| object.get("payload"))
        .ok_or_else(|| {
            LedgerError::IntegrityViolation(
                "non-resumable result payload lacks its wrapped observation".to_string(),
            )
        })?;
    let expected_payload = CanonicalJson::from_value(&json!({
        "schema": NONRESUMABLE_RESULT_SCHEMA,
        "schema_version": RUN_EVENT_BINDING_SCHEMA_VERSION,
        "source_event_id": source.event_id,
        "claim_binding": claim_binding,
        "payload": observed_payload,
    }))?;
    let actual_result_digest =
        Sha256Digest::new(sha256_hex(result.public_payload.as_str().as_bytes()))?;
    if source.effect_intent != EffectIntent::LocalOneShotV1
        || result.run_id != source.run_id
        || result.sequence <= source.sequence
        || result.kind != SessionRunEventKind::Append
        || result.append_key.as_ref() != Some(&expected_append_key)
        || result.effect_intent != EffectIntent::None
        || result.sealed_artifact_ref.is_none()
        || result.public_payload != expected_payload
        || actual_result_digest != *result_payload_digest
    {
        return Err(LedgerError::IntegrityViolation(
            "non-resumable result event does not match its claim-bound source".to_string(),
        ));
    }
    Ok(())
}

fn verify_event_delivery_projection_in_db(
    factory: &LedgerConnectionFactory,
    event: &SessionRunEvent,
) -> LedgerResult<()> {
    let connection = factory.open()?;
    let (manifest_jcs, manifest_hash): (String, String) = connection.query_row(
        "SELECT manifest_jcs, manifest_hash FROM session_run_manifests WHERE event_id = ?1",
        [event.event_id.to_string()],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let manifest_jcs = CanonicalJson::from_jcs_text(manifest_jcs).map_err(|_| {
        LedgerError::IntegrityViolation("manifest is not canonical JCS-v1".to_string())
    })?;
    let actual_outbox = load_outbox_records_for_event(&connection, event.event_id)?;
    verify_event_delivery_projection(event, &manifest_jcs, &manifest_hash, &actual_outbox)
}

fn verify_run_snapshot_contents(
    header: &RunHeader,
    status: &RunStatus,
    head_hash: &str,
    events: &[SessionRunEvent],
    manifests: &[LedgerManifestSnapshot],
    outbox: &[LedgerOutboxSnapshot],
    nonresumable_effects: &[NonResumableEffectSnapshot],
) -> LedgerResult<()> {
    if header.run_id != status.run_id {
        return Err(LedgerError::IntegrityViolation(
            "snapshot header and status name different runs".to_string(),
        ));
    }
    let first = events.first().ok_or_else(|| {
        LedgerError::IntegrityViolation("snapshot has no immutable run-start event".to_string())
    })?;
    if first.event_id != header.start_event_id || first.kind != SessionRunEventKind::RunStart {
        return Err(LedgerError::IntegrityViolation(
            "snapshot first event does not equal the run header start event".to_string(),
        ));
    }
    if events.len() != manifests.len() {
        return Err(LedgerError::IntegrityViolation(
            "snapshot event and manifest counts differ".to_string(),
        ));
    }

    let expected_provenance = header.provenance.canonical_json()?;
    let mut previous_hash = genesis_hash();
    let mut run_start_count = 0_u64;
    let mut accounted_outbox = 0_usize;
    let mut accounted_nonresumable = 0_usize;
    for (index, (event, manifest)) in events.iter().zip(manifests.iter()).enumerate() {
        let expected_sequence = u64::try_from(index)
            .map_err(|_| LedgerError::NumericOverflow)?
            .checked_add(1)
            .ok_or(LedgerError::NumericOverflow)?;
        if event.run_id != header.run_id
            || event.sequence != expected_sequence
            || event.previous_hash != previous_hash
            || manifest.event_id != event.event_id
        {
            return Err(LedgerError::IntegrityViolation(
                "snapshot event ordering, chain link, or manifest linkage is invalid".to_string(),
            ));
        }
        if event.kind == SessionRunEventKind::RunStart {
            run_start_count = run_start_count
                .checked_add(1)
                .ok_or(LedgerError::NumericOverflow)?;
            if event.run_provenance.as_ref() != Some(&expected_provenance) {
                return Err(LedgerError::IntegrityViolation(
                    "run-start provenance does not equal the immutable run header".to_string(),
                ));
            }
        }
        if recompute_event_hash(event)? != event.event_hash {
            return Err(LedgerError::IntegrityViolation(
                "snapshot event hash does not match its canonical body".to_string(),
            ));
        }

        let expected_manifest = DispatchPolicyV1::derive(
            event.event_id,
            event.effect_intent,
            &event.public_payload,
            event.sealed_artifact_ref.as_ref(),
        )?;
        let mut event_outbox = outbox
            .iter()
            .filter(|entry| entry.record.event_id == event.event_id);
        for expected in &expected_manifest.entries {
            let actual = event_outbox.next().ok_or_else(|| {
                LedgerError::IntegrityViolation(
                    "snapshot outbox cardinality does not equal its manifest".to_string(),
                )
            })?;
            if actual.manifest_order != expected.order {
                return Err(LedgerError::IntegrityViolation(
                    "snapshot outbox manifest order does not equal the derived policy".to_string(),
                ));
            }
        }
        if event_outbox.next().is_some() {
            return Err(LedgerError::IntegrityViolation(
                "snapshot outbox cardinality does not equal its manifest".to_string(),
            ));
        }
        accounted_outbox = accounted_outbox
            .checked_add(expected_manifest.entries.len())
            .ok_or(LedgerError::NumericOverflow)?;
        verify_event_delivery_projection_with_manifest(
            event,
            &manifest.canonical_json,
            &manifest.hash,
            &expected_manifest,
            outbox
                .iter()
                .filter(|entry| entry.record.event_id == event.event_id)
                .map(|entry| &entry.record),
        )?;
        verify_event_nonresumable_projection(event, nonresumable_effects, events)?;
        if event.effect_intent == EffectIntent::LocalOneShotV1 {
            accounted_nonresumable = accounted_nonresumable
                .checked_add(1)
                .ok_or(LedgerError::NumericOverflow)?;
        }
        previous_hash = event.event_hash.clone();
    }
    if run_start_count != 1
        || accounted_outbox != outbox.len()
        || accounted_nonresumable != nonresumable_effects.len()
        || previous_hash != head_hash
    {
        return Err(LedgerError::IntegrityViolation(
            "snapshot start count, projection linkage, or head watermark is invalid".to_string(),
        ));
    }
    verify_run_status_projection(status, events, outbox)?;
    let unknown_count = nonresumable_effects
        .iter()
        .filter(|effect| effect.record.state == NonResumableEffectState::UnknownNonResumable)
        .count();
    let has_active = nonresumable_effects.iter().any(|effect| {
        matches!(
            effect.record.state,
            NonResumableEffectState::Armed | NonResumableEffectState::Executing
        )
    });
    let terminal_reason = status.exit_terminal_reason;
    if terminal_reason.is_some() && has_active {
        return Err(LedgerError::IntegrityViolation(
            "terminal run retains an armed or executing non-resumable effect".to_string(),
        ));
    }
    if terminal_reason == Some(TerminalReason::Completed)
        && nonresumable_effects
            .iter()
            .any(|effect| effect.record.state != NonResumableEffectState::Completed)
    {
        return Err(LedgerError::IntegrityViolation(
            "completed run contains an incomplete non-resumable effect".to_string(),
        ));
    }
    if terminal_reason.is_none()
        && nonresumable_effects.iter().any(|effect| {
            matches!(
                effect.record.state,
                NonResumableEffectState::Abandoned | NonResumableEffectState::UnknownNonResumable
            )
        })
    {
        return Err(LedgerError::IntegrityViolation(
            "non-terminal run contains a terminalized non-resumable effect".to_string(),
        ));
    }
    if terminal_reason.is_some()
        && ((terminal_reason == Some(TerminalReason::EffectNonResumableUnknown))
            != (unknown_count > 0))
    {
        return Err(LedgerError::IntegrityViolation(
            "terminal reason and unknown non-resumable effect projection disagree".to_string(),
        ));
    }
    Ok(())
}

fn verify_run_status_projection(
    status: &RunStatus,
    events: &[SessionRunEvent],
    outbox: &[LedgerOutboxSnapshot],
) -> LedgerResult<()> {
    let exit_events = events
        .iter()
        .filter(|event| event.kind == SessionRunEventKind::RunExit)
        .collect::<Vec<_>>();
    let required_dead_lettered = outbox
        .iter()
        .any(|entry| entry.record.required && entry.record.state == OutboxState::DeadLettered);
    let Some(terminal_reason) = status.exit_terminal_reason else {
        let expected_final = required_dead_lettered.then_some(RunOutcome::DeadLettered);
        if !exit_events.is_empty()
            || status.exit_event_id.is_some()
            || status.candidate_outcome.is_some()
            || status.required_dead_lettered != required_dead_lettered
            || status.pending_required_delivery
            || status.final_outcome != expected_final
        {
            return Err(LedgerError::IntegrityViolation(
                "non-terminal run status disagrees with its event and outbox projections"
                    .to_string(),
            ));
        }
        return Ok(());
    };
    if exit_events.len() != 1 || status.exit_event_id != Some(exit_events[0].event_id) {
        return Err(LedgerError::IntegrityViolation(
            "terminal run must contain exactly one run-exit event".to_string(),
        ));
    }
    let binding = exit_events[0].public_payload.as_value()?;
    let object = binding.as_object().ok_or_else(|| {
        LedgerError::IntegrityViolation("run-exit binding is not an object".to_string())
    })?;
    if object.len() != 4
        || object.get("schema").and_then(serde_json::Value::as_str) != Some(RUN_EXIT_BINDING_SCHEMA)
        || object
            .get("schema_version")
            .and_then(serde_json::Value::as_u64)
            != Some(u64::from(RUN_EVENT_BINDING_SCHEMA_VERSION))
        || object
            .get("terminal_reason")
            .and_then(serde_json::Value::as_str)
            != Some(terminal_reason.as_str())
        || !object.contains_key("payload")
    {
        return Err(LedgerError::IntegrityViolation(
            "run status terminal reason is not bound by its run-exit event".to_string(),
        ));
    }

    let unresolved_required = outbox
        .iter()
        .any(|entry| entry.record.required && entry.record.state != OutboxState::Acknowledged);
    let (pending_required_delivery, final_outcome) = if required_dead_lettered {
        (false, Some(RunOutcome::DeadLettered))
    } else if unresolved_required {
        (true, None)
    } else {
        (false, Some(terminal_reason.outcome()))
    };
    if status.candidate_outcome != Some(terminal_reason.outcome())
        || status.required_dead_lettered != required_dead_lettered
        || status.pending_required_delivery != pending_required_delivery
        || status.final_outcome != final_outcome
    {
        return Err(LedgerError::IntegrityViolation(
            "run status outcomes do not reconcile with its exit and outbox".to_string(),
        ));
    }
    Ok(())
}

fn parse_optional_artifact_ref(
    envelope_digest: Option<String>,
    key_id: Option<String>,
    schema_version: Option<i64>,
    encryption_algorithm: Option<String>,
    canonicalization_algorithm: Option<String>,
    canonicalization_version: Option<i64>,
    redactor_version: Option<String>,
) -> LedgerResult<Option<SealedArtifactRef>> {
    match (
        envelope_digest,
        key_id,
        schema_version,
        encryption_algorithm,
        canonicalization_algorithm,
        canonicalization_version,
        redactor_version,
    ) {
        (None, None, None, None, None, None, None) => Ok(None),
        (
            Some(envelope_digest),
            Some(key_id),
            Some(schema_version),
            Some(encryption_algorithm),
            Some(canonicalization_algorithm),
            Some(canonicalization_version),
            Some(redactor_version),
        ) => Ok(Some(SealedArtifactRef::from_db(
            envelope_digest,
            key_id,
            schema_version,
            encryption_algorithm,
            canonicalization_algorithm,
            canonicalization_version,
            redactor_version,
        )?)),
        _ => Err(LedgerError::InvalidStoredState {
            kind: "sealed artifact reference",
            value: "partially populated".to_string(),
        }),
    }
}

fn parse_run_id(value: String) -> LedgerResult<RunId> {
    value.parse().map_err(|_| LedgerError::InvalidStoredState {
        kind: "run id",
        value,
    })
}

fn parse_event_id(value: String) -> LedgerResult<EventId> {
    value.parse().map_err(|_| LedgerError::InvalidStoredState {
        kind: "event id",
        value,
    })
}

fn parse_outbox_id(value: String) -> LedgerResult<OutboxRowId> {
    value.parse().map_err(|_| LedgerError::InvalidStoredState {
        kind: "outbox id",
        value,
    })
}

fn parse_replay_operation_instance_id(value: String) -> LedgerResult<String> {
    if value.is_empty() || value.len() > 256 {
        return Err(LedgerError::InvalidStoredState {
            kind: "replay operation instance id",
            value,
        });
    }
    Ok(value)
}

fn parse_bool(kind: &'static str, value: i64) -> LedgerResult<bool> {
    match value {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(LedgerError::InvalidStoredState {
            kind,
            value: value.to_string(),
        }),
    }
}

fn parse_nonnegative_u64(kind: &'static str, value: i64) -> LedgerResult<u64> {
    u64::try_from(value).map_err(|_| LedgerError::InvalidStoredState {
        kind,
        value: value.to_string(),
    })
}

fn parse_positive_u64(kind: &'static str, value: i64) -> LedgerResult<u64> {
    let value = parse_nonnegative_u64(kind, value)?;
    if value == 0 {
        return Err(LedgerError::InvalidStoredState {
            kind,
            value: "0".to_string(),
        });
    }
    Ok(value)
}

fn parse_nonnegative_u32(kind: &'static str, value: i64) -> LedgerResult<u32> {
    u32::try_from(value).map_err(|_| LedgerError::InvalidStoredState {
        kind,
        value: value.to_string(),
    })
}

fn parse_positive_u32(kind: &'static str, value: i64) -> LedgerResult<u32> {
    let value = parse_nonnegative_u32(kind, value)?;
    if value == 0 {
        return Err(LedgerError::InvalidStoredState {
            kind,
            value: "0".to_string(),
        });
    }
    Ok(value)
}

fn validated_digest_string(kind: &'static str, value: String) -> LedgerResult<String> {
    Sha256Digest::new(value)
        .map(|digest| digest.as_str().to_string())
        .map_err(|_| LedgerError::InvalidStoredState {
            kind,
            value: "not a full lowercase SHA-256 digest".to_string(),
        })
}
