use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior};
use sha2::{Digest, Sha256};

use super::error::{LedgerError, LedgerResult};

pub const LEDGER_SCHEMA_VERSION: i64 = 6;
pub const DEFAULT_BUSY_TIMEOUT: Duration = Duration::from_secs(3);
const MAX_BUSY_TIMEOUT: Duration = Duration::from_secs(30);
const SCHEMA_FINGERPRINT_MARKER: &str = "run_ledger_schema_fingerprint";
const V1_SCHEMA_FINGERPRINT_DOMAIN: &[u8] = b"hermes.run-ledger.schema/v1\0";
const V2_SCHEMA_FINGERPRINT_DOMAIN: &[u8] = b"hermes.run-ledger.schema/v2\0";
const V3_SCHEMA_FINGERPRINT_DOMAIN: &[u8] = b"hermes.run-ledger.schema/v3\0";
const V4_SCHEMA_FINGERPRINT_DOMAIN: &[u8] = b"hermes.run-ledger.schema/v4\0";
const V5_SCHEMA_FINGERPRINT_DOMAIN: &[u8] = b"hermes.run-ledger.schema/v5\0";
const V6_SCHEMA_FINGERPRINT_DOMAIN: &[u8] = b"hermes.run-ledger.schema/v6\0";

const STATE_META_DDL: &str = r#"
CREATE TABLE state_meta (
    key TEXT PRIMARY KEY NOT NULL,
    value TEXT NOT NULL
)
"#;
const LEGACY_SESSION_STATE_META_DDL: &str = r#"
CREATE TABLE state_meta (
    key TEXT PRIMARY KEY,
    value TEXT
)
"#;

const SESSION_RUNS_DDL: &str = r#"
CREATE TABLE session_runs (
    run_id TEXT PRIMARY KEY NOT NULL,
    session_id TEXT NOT NULL,
    parent_run_id TEXT REFERENCES session_runs(run_id) ON DELETE RESTRICT,
    branch_id TEXT NOT NULL,
    tenant_id TEXT NOT NULL,
    action_id TEXT NOT NULL,
    objective_id TEXT NOT NULL,
    task_id TEXT NOT NULL,
    model_id TEXT NOT NULL,
    provider_id TEXT NOT NULL,
    config_digest TEXT NOT NULL CHECK (length(config_digest) = 64 AND config_digest NOT GLOB '*[^0-9a-f]*'),
    prompt_digest TEXT CHECK (prompt_digest IS NULL OR (length(prompt_digest) = 64 AND prompt_digest NOT GLOB '*[^0-9a-f]*')),
    prompt_digest_absence TEXT CHECK (prompt_digest_absence IS NULL OR prompt_digest_absence IN ('unavailable', 'not_applicable')),
    input_digest TEXT CHECK (input_digest IS NULL OR (length(input_digest) = 64 AND input_digest NOT GLOB '*[^0-9a-f]*')),
    input_digest_absence TEXT CHECK (input_digest_absence IS NULL OR input_digest_absence IN ('unavailable', 'not_applicable')),
    output_digest TEXT CHECK (output_digest IS NULL OR (length(output_digest) = 64 AND output_digest NOT GLOB '*[^0-9a-f]*')),
    output_digest_absence TEXT CHECK (output_digest_absence IS NULL OR output_digest_absence IN ('unavailable', 'not_applicable')),
    tool_digest TEXT CHECK (tool_digest IS NULL OR (length(tool_digest) = 64 AND tool_digest NOT GLOB '*[^0-9a-f]*')),
    tool_digest_absence TEXT CHECK (tool_digest_absence IS NULL OR tool_digest_absence IN ('unavailable', 'not_applicable')),
    skill_digest TEXT CHECK (skill_digest IS NULL OR (length(skill_digest) = 64 AND skill_digest NOT GLOB '*[^0-9a-f]*')),
    skill_digest_absence TEXT CHECK (skill_digest_absence IS NULL OR skill_digest_absence IN ('unavailable', 'not_applicable')),
    policy_digest TEXT CHECK (policy_digest IS NULL OR (length(policy_digest) = 64 AND policy_digest NOT GLOB '*[^0-9a-f]*')),
    policy_digest_absence TEXT CHECK (policy_digest_absence IS NULL OR policy_digest_absence IN ('unavailable', 'not_applicable')),
    sandbox_digest TEXT CHECK (sandbox_digest IS NULL OR (length(sandbox_digest) = 64 AND sandbox_digest NOT GLOB '*[^0-9a-f]*')),
    sandbox_digest_absence TEXT CHECK (sandbox_digest_absence IS NULL OR sandbox_digest_absence IN ('unavailable', 'not_applicable')),
    provenance_canonicalization_algorithm TEXT NOT NULL CHECK (provenance_canonicalization_algorithm = 'rfc8785-jcs'),
    provenance_canonicalization_version INTEGER NOT NULL CHECK (provenance_canonicalization_version = 1),
    execution_mode TEXT NOT NULL CHECK (execution_mode IN ('manual', 'reviewed', 'autonomous_local', 'ci', 'remote_worker')),
    execution_mode_source TEXT NOT NULL CHECK (execution_mode_source IN ('built_in_default', 'profile', 'config_file', 'environment', 'cli')),
    owner_id TEXT NOT NULL CHECK (length(owner_id) BETWEEN 1 AND 256),
    owner_lease_generation INTEGER NOT NULL CHECK (owner_lease_generation > 0),
    owner_lease_token BLOB NOT NULL CHECK (length(owner_lease_token) = 16),
    owner_lease_expires_at_ms INTEGER NOT NULL,
    start_event_id TEXT NOT NULL UNIQUE REFERENCES session_run_events(event_id) ON DELETE RESTRICT DEFERRABLE INITIALLY DEFERRED,
    started_at_ms INTEGER NOT NULL,
    next_event_sequence INTEGER NOT NULL CHECK (next_event_sequence >= 0),
    head_hash TEXT NOT NULL CHECK (length(head_hash) = 64 AND head_hash NOT GLOB '*[^0-9a-f]*'),
    exit_event_id TEXT UNIQUE REFERENCES session_run_events(event_id) ON DELETE RESTRICT DEFERRABLE INITIALLY DEFERRED,
    exit_terminal_reason TEXT CHECK (exit_terminal_reason IN ('completed', 'execution_failure', 'timeout', 'cancellation', 'policy_blocked', 'required_delivery_dead_lettered', 'process_death', 'effect_non_resumable_unknown')),
    candidate_outcome TEXT CHECK (candidate_outcome IN ('success', 'failure', 'timeout', 'cancellation', 'blocked', 'dead_lettered')),
    final_outcome TEXT CHECK (final_outcome IN ('success', 'failure', 'timeout', 'cancellation', 'blocked', 'dead_lettered')),
    required_dead_lettered INTEGER NOT NULL DEFAULT 0 CHECK (required_dead_lettered IN (0, 1)),
    pending_required_delivery INTEGER NOT NULL DEFAULT 0 CHECK (pending_required_delivery IN (0, 1)),
    CHECK (parent_run_id IS NULL OR parent_run_id != run_id),
    CHECK ((prompt_digest IS NOT NULL AND prompt_digest_absence IS NULL) OR (prompt_digest IS NULL AND prompt_digest_absence IS NOT NULL)),
    CHECK ((input_digest IS NOT NULL AND input_digest_absence IS NULL) OR (input_digest IS NULL AND input_digest_absence IS NOT NULL)),
    CHECK ((output_digest IS NOT NULL AND output_digest_absence IS NULL) OR (output_digest IS NULL AND output_digest_absence IS NOT NULL)),
    CHECK ((tool_digest IS NOT NULL AND tool_digest_absence IS NULL) OR (tool_digest IS NULL AND tool_digest_absence IS NOT NULL)),
    CHECK ((skill_digest IS NOT NULL AND skill_digest_absence IS NULL) OR (skill_digest IS NULL AND skill_digest_absence IS NOT NULL)),
    CHECK ((policy_digest IS NOT NULL AND policy_digest_absence IS NULL) OR (policy_digest IS NULL AND policy_digest_absence IS NOT NULL)),
    CHECK ((sandbox_digest IS NOT NULL AND sandbox_digest_absence IS NULL) OR (sandbox_digest IS NULL AND sandbox_digest_absence IS NOT NULL))
)
"#;

const SESSION_RUN_EVENTS_V1_DDL: &str = r#"
CREATE TABLE session_run_events (
    event_id TEXT PRIMARY KEY NOT NULL,
    run_id TEXT NOT NULL REFERENCES session_runs(run_id) ON DELETE RESTRICT,
    event_sequence INTEGER NOT NULL CHECK (event_sequence > 0),
    event_kind TEXT NOT NULL CHECK (event_kind IN ('run_start', 'append', 'delivery_result', 'run_exit', 'manual_append_degraded_audit')),
    append_key TEXT UNIQUE,
    effect_intent TEXT NOT NULL CHECK (effect_intent IN ('none', 'alpha_runtime_v1', 'local_one_shot_v1')),
    artifact_degradation_reason TEXT CHECK (artifact_degradation_reason IS NULL OR artifact_degradation_reason IN ('sealing_key_unavailable', 'artifact_store_unavailable')),
    public_identity_jcs TEXT,
    private_fingerprint_algorithm TEXT,
    private_fingerprint_algorithm_version INTEGER,
    private_fingerprint_key_id TEXT,
    private_fingerprint_key_version INTEGER,
    private_fingerprint TEXT CHECK (private_fingerprint IS NULL OR (length(private_fingerprint) = 64 AND private_fingerprint NOT GLOB '*[^0-9a-f]*')),
    sealed_artifact_envelope_digest TEXT CHECK (sealed_artifact_envelope_digest IS NULL OR (length(sealed_artifact_envelope_digest) = 64 AND sealed_artifact_envelope_digest NOT GLOB '*[^0-9a-f]*')),
    sealed_artifact_key_id TEXT,
    sealed_artifact_schema_version INTEGER,
    sealed_artifact_encryption_algorithm TEXT,
    sealed_artifact_canonicalization_algorithm TEXT,
    sealed_artifact_canonicalization_version INTEGER,
    sealed_artifact_redactor_version TEXT,
    run_provenance_jcs TEXT,
    public_payload_jcs TEXT NOT NULL,
    manifest_hash TEXT NOT NULL CHECK (length(manifest_hash) = 64 AND manifest_hash NOT GLOB '*[^0-9a-f]*'),
    previous_hash TEXT NOT NULL CHECK (length(previous_hash) = 64 AND previous_hash NOT GLOB '*[^0-9a-f]*'),
    event_hash TEXT NOT NULL CHECK (length(event_hash) = 64 AND event_hash NOT GLOB '*[^0-9a-f]*'),
    canonicalization_algorithm TEXT NOT NULL CHECK (canonicalization_algorithm = 'rfc8785-jcs'),
    canonicalization_version INTEGER NOT NULL CHECK (canonicalization_version = 1),
    created_at_ms INTEGER NOT NULL,
    UNIQUE(run_id, event_sequence),
    CHECK ((public_identity_jcs IS NULL AND private_fingerprint_algorithm IS NULL AND private_fingerprint_algorithm_version IS NULL AND private_fingerprint_key_id IS NULL AND private_fingerprint_key_version IS NULL AND private_fingerprint IS NULL) OR (public_identity_jcs IS NOT NULL AND private_fingerprint_algorithm = 'hmac-sha256' AND private_fingerprint_algorithm_version = 1 AND private_fingerprint_key_id IS NOT NULL AND private_fingerprint_key_version > 0 AND private_fingerprint IS NOT NULL)),
    CHECK ((sealed_artifact_envelope_digest IS NULL AND sealed_artifact_key_id IS NULL AND sealed_artifact_schema_version IS NULL AND sealed_artifact_encryption_algorithm IS NULL AND sealed_artifact_canonicalization_algorithm IS NULL AND sealed_artifact_canonicalization_version IS NULL AND sealed_artifact_redactor_version IS NULL) OR (sealed_artifact_envelope_digest IS NOT NULL AND sealed_artifact_key_id IS NOT NULL AND sealed_artifact_schema_version = 1 AND sealed_artifact_encryption_algorithm = 'aes-256-gcm' AND sealed_artifact_canonicalization_algorithm = 'rfc8785-jcs' AND sealed_artifact_canonicalization_version = 1 AND sealed_artifact_redactor_version IS NOT NULL)),
    CHECK (effect_intent = 'none' OR sealed_artifact_envelope_digest IS NOT NULL),
    CHECK ((event_kind = 'run_start' AND run_provenance_jcs IS NOT NULL) OR (event_kind != 'run_start' AND run_provenance_jcs IS NULL)),
    CHECK (
        (event_kind = 'append' AND append_key IS NOT NULL AND public_identity_jcs IS NOT NULL AND artifact_degradation_reason IS NULL)
        OR (event_kind = 'manual_append_degraded_audit' AND append_key IS NOT NULL AND public_identity_jcs IS NULL AND artifact_degradation_reason IS NOT NULL AND effect_intent = 'none' AND sealed_artifact_envelope_digest IS NULL)
        OR (event_kind NOT IN ('append', 'manual_append_degraded_audit') AND append_key IS NULL AND public_identity_jcs IS NULL AND artifact_degradation_reason IS NULL)
    )
)
"#;
const SESSION_RUN_EVENTS_V2_DDL: &str = r#"
CREATE TABLE session_run_events (
    event_id TEXT PRIMARY KEY NOT NULL,
    run_id TEXT NOT NULL REFERENCES session_runs(run_id) ON DELETE RESTRICT,
    event_sequence INTEGER NOT NULL CHECK (event_sequence > 0),
    event_kind TEXT NOT NULL CHECK (event_kind IN ('run_start', 'append', 'delivery_result', 'run_exit', 'manual_append_degraded_audit')),
    append_key TEXT UNIQUE,
    effect_intent TEXT NOT NULL CHECK (effect_intent IN ('none', 'alpha_runtime_v1', 'local_one_shot_v1')),
    artifact_degradation_reason TEXT CHECK (artifact_degradation_reason IS NULL OR artifact_degradation_reason IN ('sealing_key_unavailable', 'artifact_store_unavailable')),
    public_identity_jcs TEXT,
    private_fingerprint_algorithm TEXT,
    private_fingerprint_algorithm_version INTEGER,
    private_fingerprint_key_id TEXT,
    private_fingerprint_key_version INTEGER,
    private_fingerprint TEXT CHECK (private_fingerprint IS NULL OR (length(private_fingerprint) = 64 AND private_fingerprint NOT GLOB '*[^0-9a-f]*')),
    sealed_artifact_envelope_digest TEXT CHECK (sealed_artifact_envelope_digest IS NULL OR (length(sealed_artifact_envelope_digest) = 64 AND sealed_artifact_envelope_digest NOT GLOB '*[^0-9a-f]*')),
    sealed_artifact_key_id TEXT,
    sealed_artifact_schema_version INTEGER,
    sealed_artifact_encryption_algorithm TEXT,
    sealed_artifact_canonicalization_algorithm TEXT,
    sealed_artifact_canonicalization_version INTEGER,
    sealed_artifact_redactor_version TEXT,
    run_provenance_jcs TEXT,
    public_payload_jcs TEXT NOT NULL,
    manifest_hash TEXT NOT NULL CHECK (length(manifest_hash) = 64 AND manifest_hash NOT GLOB '*[^0-9a-f]*'),
    previous_hash TEXT NOT NULL CHECK (length(previous_hash) = 64 AND previous_hash NOT GLOB '*[^0-9a-f]*'),
    event_hash TEXT NOT NULL CHECK (length(event_hash) = 64 AND event_hash NOT GLOB '*[^0-9a-f]*'),
    canonicalization_algorithm TEXT NOT NULL CHECK (canonicalization_algorithm = 'rfc8785-jcs'),
    canonicalization_version INTEGER NOT NULL CHECK (canonicalization_version = 1),
    created_at_ms INTEGER NOT NULL,
    UNIQUE(run_id, event_sequence),
    CHECK (((public_identity_jcs IS NULL AND private_fingerprint_algorithm IS NULL AND private_fingerprint_algorithm_version IS NULL AND private_fingerprint_key_id IS NULL AND private_fingerprint_key_version IS NULL AND private_fingerprint IS NULL) OR (public_identity_jcs IS NOT NULL AND private_fingerprint_algorithm = 'hmac-sha256' AND private_fingerprint_algorithm_version = 1 AND private_fingerprint_key_id IS NOT NULL AND private_fingerprint_key_version > 0 AND private_fingerprint IS NOT NULL)) IS TRUE),
    CHECK (((sealed_artifact_envelope_digest IS NULL AND sealed_artifact_key_id IS NULL AND sealed_artifact_schema_version IS NULL AND sealed_artifact_encryption_algorithm IS NULL AND sealed_artifact_canonicalization_algorithm IS NULL AND sealed_artifact_canonicalization_version IS NULL AND sealed_artifact_redactor_version IS NULL) OR (sealed_artifact_envelope_digest IS NOT NULL AND sealed_artifact_key_id IS NOT NULL AND sealed_artifact_schema_version = 1 AND sealed_artifact_encryption_algorithm = 'aes-256-gcm' AND sealed_artifact_canonicalization_algorithm = 'rfc8785-jcs' AND sealed_artifact_canonicalization_version = 1 AND sealed_artifact_redactor_version IS NOT NULL)) IS TRUE),
    CHECK (effect_intent = 'none' OR sealed_artifact_envelope_digest IS NOT NULL),
    CHECK ((event_kind = 'run_start' AND run_provenance_jcs IS NOT NULL) OR (event_kind != 'run_start' AND run_provenance_jcs IS NULL)),
    CHECK (
        (event_kind = 'append' AND append_key IS NOT NULL AND public_identity_jcs IS NOT NULL AND artifact_degradation_reason IS NULL)
        OR (event_kind = 'manual_append_degraded_audit' AND append_key IS NOT NULL AND public_identity_jcs IS NULL AND artifact_degradation_reason IS NOT NULL AND effect_intent = 'none' AND sealed_artifact_envelope_digest IS NULL)
        OR (
            event_kind = 'delivery_result'
            AND append_key IS NULL
            AND effect_intent = 'none'
            AND artifact_degradation_reason IS NULL
            AND (
                (public_identity_jcs IS NULL AND sealed_artifact_envelope_digest IS NULL)
                OR (public_identity_jcs IS NOT NULL AND sealed_artifact_envelope_digest IS NOT NULL)
            )
        )
        OR (
            event_kind IN ('run_start', 'run_exit')
            AND append_key IS NULL
            AND public_identity_jcs IS NULL
            AND artifact_degradation_reason IS NULL
            AND sealed_artifact_envelope_digest IS NULL
        )
    )
)
"#;

const SESSION_RUN_MANIFESTS_DDL: &str = r#"
CREATE TABLE session_run_manifests (
    event_id TEXT PRIMARY KEY NOT NULL REFERENCES session_run_events(event_id) ON DELETE RESTRICT,
    manifest_jcs TEXT NOT NULL,
    manifest_hash TEXT NOT NULL CHECK (length(manifest_hash) = 64 AND manifest_hash NOT GLOB '*[^0-9a-f]*'),
    canonicalization_algorithm TEXT NOT NULL CHECK (canonicalization_algorithm = 'rfc8785-jcs'),
    canonicalization_version INTEGER NOT NULL CHECK (canonicalization_version = 1)
)
"#;

const SESSION_RUN_APPEND_RESERVATIONS_DDL: &str = r#"
CREATE TABLE session_run_append_reservations (
    append_key TEXT PRIMARY KEY NOT NULL,
    run_id TEXT NOT NULL REFERENCES session_runs(run_id) ON DELETE RESTRICT,
    event_id TEXT NOT NULL UNIQUE,
    frozen_event_at_ms INTEGER NOT NULL,
    public_identity_jcs TEXT NOT NULL,
    private_fingerprint_algorithm TEXT NOT NULL CHECK (private_fingerprint_algorithm = 'hmac-sha256'),
    private_fingerprint_algorithm_version INTEGER NOT NULL CHECK (private_fingerprint_algorithm_version = 1),
    private_fingerprint_key_id TEXT NOT NULL,
    private_fingerprint_key_version INTEGER NOT NULL CHECK (private_fingerprint_key_version > 0),
    private_fingerprint TEXT NOT NULL CHECK (length(private_fingerprint) = 64 AND private_fingerprint NOT GLOB '*[^0-9a-f]*'),
    effect_intent TEXT NOT NULL CHECK (effect_intent IN ('none', 'alpha_runtime_v1', 'local_one_shot_v1')),
    public_payload_jcs TEXT NOT NULL,
    sealed_artifact_envelope_digest TEXT CHECK (sealed_artifact_envelope_digest IS NULL OR (length(sealed_artifact_envelope_digest) = 64 AND sealed_artifact_envelope_digest NOT GLOB '*[^0-9a-f]*')),
    sealed_artifact_key_id TEXT,
    sealed_artifact_schema_version INTEGER,
    sealed_artifact_encryption_algorithm TEXT,
    sealed_artifact_canonicalization_algorithm TEXT,
    sealed_artifact_canonicalization_version INTEGER,
    sealed_artifact_redactor_version TEXT,
    lease_generation INTEGER NOT NULL CHECK (lease_generation > 0),
    lease_token BLOB NOT NULL,
    lease_expires_at_ms INTEGER NOT NULL,
    committed_event_id TEXT UNIQUE REFERENCES session_run_events(event_id) ON DELETE RESTRICT,
    committed_at_ms INTEGER,
    terminal_audit_event_id TEXT UNIQUE REFERENCES session_run_events(event_id) ON DELETE RESTRICT,
    terminal_degraded_at_ms INTEGER,
    CHECK ((sealed_artifact_envelope_digest IS NULL AND sealed_artifact_key_id IS NULL AND sealed_artifact_schema_version IS NULL AND sealed_artifact_encryption_algorithm IS NULL AND sealed_artifact_canonicalization_algorithm IS NULL AND sealed_artifact_canonicalization_version IS NULL AND sealed_artifact_redactor_version IS NULL) OR (sealed_artifact_envelope_digest IS NOT NULL AND sealed_artifact_key_id IS NOT NULL AND sealed_artifact_schema_version = 1 AND sealed_artifact_encryption_algorithm = 'aes-256-gcm' AND sealed_artifact_canonicalization_algorithm = 'rfc8785-jcs' AND sealed_artifact_canonicalization_version = 1 AND sealed_artifact_redactor_version IS NOT NULL)),
    CHECK ((committed_event_id IS NULL AND committed_at_ms IS NULL) OR (committed_event_id IS NOT NULL AND committed_at_ms IS NOT NULL)),
    CHECK ((terminal_audit_event_id IS NULL AND terminal_degraded_at_ms IS NULL) OR (terminal_audit_event_id IS NOT NULL AND terminal_degraded_at_ms IS NOT NULL)),
    CHECK (committed_event_id IS NULL OR terminal_audit_event_id IS NULL),
    CHECK (terminal_audit_event_id IS NULL OR (sealed_artifact_envelope_digest IS NULL AND sealed_artifact_key_id IS NULL AND sealed_artifact_schema_version IS NULL AND sealed_artifact_encryption_algorithm IS NULL AND sealed_artifact_canonicalization_algorithm IS NULL AND sealed_artifact_canonicalization_version IS NULL AND sealed_artifact_redactor_version IS NULL))
)
"#;

const SESSION_RUN_NONRESUMABLE_EFFECTS_DDL: &str = r#"
CREATE TABLE session_run_nonresumable_effects (
    event_id TEXT PRIMARY KEY NOT NULL REFERENCES session_run_events(event_id) ON DELETE RESTRICT,
    run_id TEXT NOT NULL REFERENCES session_runs(run_id) ON DELETE RESTRICT,
    state TEXT NOT NULL CHECK (state IN ('armed', 'executing', 'completed', 'abandoned', 'unknown_non_resumable')),
    idempotency_key TEXT NOT NULL UNIQUE CHECK (length(idempotency_key) = 64 AND idempotency_key NOT GLOB '*[^0-9a-f]*'),
    claim_token BLOB,
    claimed_at_ms INTEGER,
    result_payload_digest TEXT CHECK (result_payload_digest IS NULL OR (length(result_payload_digest) = 64 AND result_payload_digest NOT GLOB '*[^0-9a-f]*')),
    result_event_id TEXT UNIQUE REFERENCES session_run_events(event_id) ON DELETE RESTRICT,
    terminal_at_ms INTEGER,
    CHECK (
        (state = 'armed' AND claim_token IS NULL AND claimed_at_ms IS NULL AND result_payload_digest IS NULL AND result_event_id IS NULL AND terminal_at_ms IS NULL)
        OR (state = 'executing' AND claim_token IS NOT NULL AND claimed_at_ms IS NOT NULL AND result_event_id IS NULL AND terminal_at_ms IS NULL)
        OR (state = 'completed' AND claim_token IS NOT NULL AND claimed_at_ms IS NOT NULL AND result_payload_digest IS NOT NULL AND result_event_id IS NOT NULL AND terminal_at_ms IS NOT NULL)
        OR (state = 'abandoned' AND claim_token IS NULL AND claimed_at_ms IS NULL AND result_payload_digest IS NULL AND result_event_id IS NULL AND terminal_at_ms IS NOT NULL)
        OR (state = 'unknown_non_resumable' AND claim_token IS NOT NULL AND claimed_at_ms IS NOT NULL AND result_event_id IS NULL AND terminal_at_ms IS NOT NULL)
    )
)
"#;

const SESSION_RUN_OUTBOX_V1_DDL: &str = r#"
CREATE TABLE session_run_outbox (
    outbox_id TEXT PRIMARY KEY NOT NULL,
    run_id TEXT NOT NULL REFERENCES session_runs(run_id) ON DELETE RESTRICT,
    event_id TEXT NOT NULL REFERENCES session_run_events(event_id) ON DELETE RESTRICT,
    manifest_order INTEGER NOT NULL CHECK (manifest_order >= 0),
    destination TEXT NOT NULL CHECK (destination IN ('alpha_runtime_v1')),
    required INTEGER NOT NULL CHECK (required IN (0, 1)),
    idempotency_key TEXT NOT NULL CHECK (length(idempotency_key) = 64 AND idempotency_key NOT GLOB '*[^0-9a-f]*'),
    public_payload_jcs TEXT NOT NULL,
    sealed_artifact_envelope_digest TEXT NOT NULL CHECK (length(sealed_artifact_envelope_digest) = 64 AND sealed_artifact_envelope_digest NOT GLOB '*[^0-9a-f]*'),
    sealed_artifact_key_id TEXT NOT NULL,
    sealed_artifact_schema_version INTEGER NOT NULL CHECK (sealed_artifact_schema_version = 1),
    sealed_artifact_encryption_algorithm TEXT NOT NULL CHECK (sealed_artifact_encryption_algorithm = 'aes-256-gcm'),
    sealed_artifact_canonicalization_algorithm TEXT NOT NULL CHECK (sealed_artifact_canonicalization_algorithm = 'rfc8785-jcs'),
    sealed_artifact_canonicalization_version INTEGER NOT NULL CHECK (sealed_artifact_canonicalization_version = 1),
    sealed_artifact_redactor_version TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('ready', 'leased', 'acknowledged', 'dead_lettered')),
    attempt_count INTEGER NOT NULL DEFAULT 0 CHECK (attempt_count >= 0),
    max_attempts INTEGER NOT NULL CHECK (max_attempts > 0),
    lease_generation INTEGER NOT NULL DEFAULT 0 CHECK (lease_generation >= 0),
    lease_token BLOB,
    lease_expires_at_ms INTEGER,
    delivery_receipt TEXT,
    delivery_error TEXT CHECK (delivery_error IS NULL OR delivery_error IN ('missing_route', 'invalid_payload', 'route_decode_failed', 'handler_rejected', 'attempts_exhausted')),
    terminal_at_ms INTEGER,
    UNIQUE(event_id, manifest_order),
    UNIQUE(destination, idempotency_key),
    CHECK (attempt_count <= max_attempts),
    CHECK ((state = 'leased' AND lease_token IS NOT NULL AND lease_expires_at_ms IS NOT NULL) OR (state != 'leased' AND lease_token IS NULL AND lease_expires_at_ms IS NULL)),
    CHECK ((state = 'acknowledged' AND delivery_receipt IS NOT NULL AND delivery_error IS NULL AND terminal_at_ms IS NOT NULL) OR (state = 'dead_lettered' AND delivery_receipt IS NULL AND delivery_error IS NOT NULL AND terminal_at_ms IS NOT NULL) OR (state IN ('ready', 'leased') AND delivery_receipt IS NULL AND delivery_error IS NULL AND terminal_at_ms IS NULL))
)
"#;
const SESSION_RUN_OUTBOX_DDL: &str = r#"
CREATE TABLE session_run_outbox (
    outbox_id TEXT PRIMARY KEY NOT NULL,
    run_id TEXT NOT NULL REFERENCES session_runs(run_id) ON DELETE RESTRICT,
    event_id TEXT NOT NULL REFERENCES session_run_events(event_id) ON DELETE RESTRICT,
    manifest_order INTEGER NOT NULL CHECK (manifest_order >= 0),
    destination TEXT NOT NULL CHECK (destination IN ('alpha_runtime_v1')),
    required INTEGER NOT NULL CHECK (required IN (0, 1)),
    idempotency_key TEXT NOT NULL CHECK (length(idempotency_key) = 64 AND idempotency_key NOT GLOB '*[^0-9a-f]*'),
    public_payload_jcs TEXT NOT NULL,
    sealed_artifact_envelope_digest TEXT NOT NULL CHECK (length(sealed_artifact_envelope_digest) = 64 AND sealed_artifact_envelope_digest NOT GLOB '*[^0-9a-f]*'),
    sealed_artifact_key_id TEXT NOT NULL,
    sealed_artifact_schema_version INTEGER NOT NULL CHECK (sealed_artifact_schema_version = 1),
    sealed_artifact_encryption_algorithm TEXT NOT NULL CHECK (sealed_artifact_encryption_algorithm = 'aes-256-gcm'),
    sealed_artifact_canonicalization_algorithm TEXT NOT NULL CHECK (sealed_artifact_canonicalization_algorithm = 'rfc8785-jcs'),
    sealed_artifact_canonicalization_version INTEGER NOT NULL CHECK (sealed_artifact_canonicalization_version = 1),
    sealed_artifact_redactor_version TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('ready', 'leased', 'acknowledged', 'dead_lettered')),
    attempt_count INTEGER NOT NULL DEFAULT 0 CHECK (attempt_count >= 0),
    max_attempts INTEGER NOT NULL CHECK (max_attempts > 0),
    lease_generation INTEGER NOT NULL DEFAULT 0 CHECK (lease_generation >= 0),
    lease_token BLOB,
    lease_expires_at_ms INTEGER,
    delivery_receipt TEXT,
    delivery_error TEXT CHECK (delivery_error IS NULL OR delivery_error IN ('missing_route', 'invalid_payload', 'route_decode_failed', 'invalid_variant', 'sealed_task_mismatch', 'durable_store_failure', 'handler_rejected', 'attempts_exhausted')),
    terminal_at_ms INTEGER,
    UNIQUE(event_id, manifest_order),
    UNIQUE(destination, idempotency_key),
    CHECK (attempt_count <= max_attempts),
    CHECK ((state = 'leased' AND lease_token IS NOT NULL AND lease_expires_at_ms IS NOT NULL) OR (state != 'leased' AND lease_token IS NULL AND lease_expires_at_ms IS NULL)),
    CHECK ((state = 'acknowledged' AND delivery_receipt IS NOT NULL AND delivery_error IS NULL AND terminal_at_ms IS NOT NULL) OR (state = 'dead_lettered' AND delivery_receipt IS NULL AND delivery_error IS NOT NULL AND terminal_at_ms IS NOT NULL) OR (state IN ('ready', 'leased') AND delivery_receipt IS NULL AND delivery_error IS NULL AND terminal_at_ms IS NULL))
)
"#;

// Frozen v3 event definition. v4 adds the authoritative RunStart sealed
// artifact binding; never revise this literal because deployed v3 markers must
// be recognized and rebuilt transactionally.
const SESSION_RUN_EVENTS_V3_DDL: &str = r#"
CREATE TABLE session_run_events (
    event_id TEXT PRIMARY KEY NOT NULL,
    run_id TEXT NOT NULL REFERENCES session_runs(run_id) ON DELETE RESTRICT,
    event_sequence INTEGER NOT NULL CHECK (event_sequence > 0),
    event_kind TEXT NOT NULL CHECK (event_kind IN ('run_start', 'append', 'delivery_result', 'run_exit', 'manual_append_degraded_audit', 'artifact_state_audit')),
    append_key TEXT UNIQUE,
    effect_intent TEXT NOT NULL CHECK (effect_intent IN ('none', 'alpha_runtime_v1', 'local_one_shot_v1')),
    artifact_degradation_reason TEXT CHECK (artifact_degradation_reason IS NULL OR artifact_degradation_reason IN ('sealing_key_unavailable', 'artifact_store_unavailable')),
    public_identity_jcs TEXT,
    private_fingerprint_algorithm TEXT,
    private_fingerprint_algorithm_version INTEGER,
    private_fingerprint_key_id TEXT,
    private_fingerprint_key_version INTEGER,
    private_fingerprint TEXT CHECK (private_fingerprint IS NULL OR (length(private_fingerprint) = 64 AND private_fingerprint NOT GLOB '*[^0-9a-f]*')),
    sealed_artifact_envelope_digest TEXT CHECK (sealed_artifact_envelope_digest IS NULL OR (length(sealed_artifact_envelope_digest) = 64 AND sealed_artifact_envelope_digest NOT GLOB '*[^0-9a-f]*')),
    sealed_artifact_key_id TEXT,
    sealed_artifact_schema_version INTEGER,
    sealed_artifact_encryption_algorithm TEXT,
    sealed_artifact_canonicalization_algorithm TEXT,
    sealed_artifact_canonicalization_version INTEGER,
    sealed_artifact_redactor_version TEXT,
    run_provenance_jcs TEXT,
    public_payload_jcs TEXT NOT NULL,
    manifest_hash TEXT NOT NULL CHECK (length(manifest_hash) = 64 AND manifest_hash NOT GLOB '*[^0-9a-f]*'),
    previous_hash TEXT NOT NULL CHECK (length(previous_hash) = 64 AND previous_hash NOT GLOB '*[^0-9a-f]*'),
    event_hash TEXT NOT NULL CHECK (length(event_hash) = 64 AND event_hash NOT GLOB '*[^0-9a-f]*'),
    canonicalization_algorithm TEXT NOT NULL CHECK (canonicalization_algorithm = 'rfc8785-jcs'),
    canonicalization_version INTEGER NOT NULL CHECK (canonicalization_version = 1),
    created_at_ms INTEGER NOT NULL,
    UNIQUE(run_id, event_sequence),
    CHECK (((public_identity_jcs IS NULL AND private_fingerprint_algorithm IS NULL AND private_fingerprint_algorithm_version IS NULL AND private_fingerprint_key_id IS NULL AND private_fingerprint_key_version IS NULL AND private_fingerprint IS NULL) OR (public_identity_jcs IS NOT NULL AND private_fingerprint_algorithm = 'hmac-sha256' AND private_fingerprint_algorithm_version = 1 AND private_fingerprint_key_id IS NOT NULL AND private_fingerprint_key_version > 0 AND private_fingerprint IS NOT NULL)) IS TRUE),
    CHECK (((sealed_artifact_envelope_digest IS NULL AND sealed_artifact_key_id IS NULL AND sealed_artifact_schema_version IS NULL AND sealed_artifact_encryption_algorithm IS NULL AND sealed_artifact_canonicalization_algorithm IS NULL AND sealed_artifact_canonicalization_version IS NULL AND sealed_artifact_redactor_version IS NULL) OR (sealed_artifact_envelope_digest IS NOT NULL AND sealed_artifact_key_id IS NOT NULL AND sealed_artifact_schema_version = 1 AND sealed_artifact_encryption_algorithm = 'aes-256-gcm' AND sealed_artifact_canonicalization_algorithm = 'rfc8785-jcs' AND sealed_artifact_canonicalization_version = 1 AND sealed_artifact_redactor_version IS NOT NULL)) IS TRUE),
    CHECK (effect_intent = 'none' OR sealed_artifact_envelope_digest IS NOT NULL),
    CHECK ((event_kind = 'run_start' AND run_provenance_jcs IS NOT NULL) OR (event_kind != 'run_start' AND run_provenance_jcs IS NULL)),
    CHECK (
        (event_kind = 'append' AND append_key IS NOT NULL AND public_identity_jcs IS NOT NULL AND artifact_degradation_reason IS NULL)
        OR (event_kind = 'manual_append_degraded_audit' AND append_key IS NOT NULL AND public_identity_jcs IS NULL AND artifact_degradation_reason IS NOT NULL AND effect_intent = 'none' AND sealed_artifact_envelope_digest IS NULL)
        OR (
            event_kind = 'delivery_result'
            AND append_key IS NULL
            AND effect_intent = 'none'
            AND artifact_degradation_reason IS NULL
            AND (
                (public_identity_jcs IS NULL AND sealed_artifact_envelope_digest IS NULL)
                OR (public_identity_jcs IS NOT NULL AND sealed_artifact_envelope_digest IS NOT NULL)
            )
        )
        OR (
            event_kind = 'run_start'
            AND append_key IS NULL
            AND effect_intent = 'none'
            AND public_identity_jcs IS NULL
            AND artifact_degradation_reason IS NULL
            AND sealed_artifact_envelope_digest IS NULL
        )
        OR (
            event_kind IN ('run_exit', 'artifact_state_audit')
            AND append_key IS NULL
            AND public_identity_jcs IS NULL
            AND artifact_degradation_reason IS NULL
            AND sealed_artifact_envelope_digest IS NULL
        )
    )
)
"#;
const SESSION_RUN_EVENTS_V4_DDL: &str = r#"
CREATE TABLE session_run_events (
    event_id TEXT PRIMARY KEY NOT NULL,
    run_id TEXT NOT NULL REFERENCES session_runs(run_id) ON DELETE RESTRICT,
    event_sequence INTEGER NOT NULL CHECK (event_sequence > 0),
    event_kind TEXT NOT NULL CHECK (event_kind IN ('run_start', 'append', 'delivery_result', 'run_exit', 'manual_append_degraded_audit', 'artifact_state_audit')),
    append_key TEXT UNIQUE,
    effect_intent TEXT NOT NULL CHECK (effect_intent IN ('none', 'alpha_runtime_v1', 'local_one_shot_v1')),
    artifact_degradation_reason TEXT CHECK (artifact_degradation_reason IS NULL OR artifact_degradation_reason IN ('sealing_key_unavailable', 'artifact_store_unavailable')),
    public_identity_jcs TEXT,
    private_fingerprint_algorithm TEXT,
    private_fingerprint_algorithm_version INTEGER,
    private_fingerprint_key_id TEXT,
    private_fingerprint_key_version INTEGER,
    private_fingerprint TEXT CHECK (private_fingerprint IS NULL OR (length(private_fingerprint) = 64 AND private_fingerprint NOT GLOB '*[^0-9a-f]*')),
    sealed_artifact_envelope_digest TEXT CHECK (sealed_artifact_envelope_digest IS NULL OR (length(sealed_artifact_envelope_digest) = 64 AND sealed_artifact_envelope_digest NOT GLOB '*[^0-9a-f]*')),
    sealed_artifact_key_id TEXT,
    sealed_artifact_schema_version INTEGER,
    sealed_artifact_encryption_algorithm TEXT,
    sealed_artifact_canonicalization_algorithm TEXT,
    sealed_artifact_canonicalization_version INTEGER,
    sealed_artifact_redactor_version TEXT,
    run_provenance_jcs TEXT,
    public_payload_jcs TEXT NOT NULL,
    manifest_hash TEXT NOT NULL CHECK (length(manifest_hash) = 64 AND manifest_hash NOT GLOB '*[^0-9a-f]*'),
    previous_hash TEXT NOT NULL CHECK (length(previous_hash) = 64 AND previous_hash NOT GLOB '*[^0-9a-f]*'),
    event_hash TEXT NOT NULL CHECK (length(event_hash) = 64 AND event_hash NOT GLOB '*[^0-9a-f]*'),
    canonicalization_algorithm TEXT NOT NULL CHECK (canonicalization_algorithm = 'rfc8785-jcs'),
    canonicalization_version INTEGER NOT NULL CHECK (canonicalization_version = 1),
    created_at_ms INTEGER NOT NULL,
    UNIQUE(run_id, event_sequence),
    CHECK (((public_identity_jcs IS NULL AND private_fingerprint_algorithm IS NULL AND private_fingerprint_algorithm_version IS NULL AND private_fingerprint_key_id IS NULL AND private_fingerprint_key_version IS NULL AND private_fingerprint IS NULL) OR (public_identity_jcs IS NOT NULL AND private_fingerprint_algorithm = 'hmac-sha256' AND private_fingerprint_algorithm_version = 1 AND private_fingerprint_key_id IS NOT NULL AND private_fingerprint_key_version > 0 AND private_fingerprint IS NOT NULL)) IS TRUE),
    CHECK (((sealed_artifact_envelope_digest IS NULL AND sealed_artifact_key_id IS NULL AND sealed_artifact_schema_version IS NULL AND sealed_artifact_encryption_algorithm IS NULL AND sealed_artifact_canonicalization_algorithm IS NULL AND sealed_artifact_canonicalization_version IS NULL AND sealed_artifact_redactor_version IS NULL) OR (sealed_artifact_envelope_digest IS NOT NULL AND sealed_artifact_key_id IS NOT NULL AND sealed_artifact_schema_version = 1 AND sealed_artifact_encryption_algorithm = 'aes-256-gcm' AND sealed_artifact_canonicalization_algorithm = 'rfc8785-jcs' AND sealed_artifact_canonicalization_version = 1 AND sealed_artifact_redactor_version IS NOT NULL)) IS TRUE),
    CHECK (effect_intent = 'none' OR sealed_artifact_envelope_digest IS NOT NULL),
    CHECK ((event_kind = 'run_start' AND run_provenance_jcs IS NOT NULL) OR (event_kind != 'run_start' AND run_provenance_jcs IS NULL)),
    CHECK (
        (event_kind = 'append' AND append_key IS NOT NULL AND public_identity_jcs IS NOT NULL AND artifact_degradation_reason IS NULL)
        OR (event_kind = 'manual_append_degraded_audit' AND append_key IS NOT NULL AND public_identity_jcs IS NULL AND artifact_degradation_reason IS NOT NULL AND effect_intent = 'none' AND sealed_artifact_envelope_digest IS NULL)
        OR (
            event_kind = 'delivery_result'
            AND append_key IS NULL
            AND effect_intent = 'none'
            AND artifact_degradation_reason IS NULL
            AND (
                (public_identity_jcs IS NULL AND sealed_artifact_envelope_digest IS NULL)
                OR (public_identity_jcs IS NOT NULL AND sealed_artifact_envelope_digest IS NOT NULL)
            )
        )
        OR (
            event_kind = 'run_start'
            AND append_key IS NULL
            AND effect_intent = 'none'
            AND artifact_degradation_reason IS NULL
            AND (
                (public_identity_jcs IS NULL AND sealed_artifact_envelope_digest IS NULL)
                OR (public_identity_jcs IS NOT NULL AND sealed_artifact_envelope_digest IS NOT NULL)
            )
        )
        OR (
            event_kind IN ('run_exit', 'artifact_state_audit')
            AND append_key IS NULL
            AND public_identity_jcs IS NULL
            AND artifact_degradation_reason IS NULL
            AND sealed_artifact_envelope_digest IS NULL
        )
    )
)
"#;

const SESSION_RUN_ARTIFACT_OBJECTS_DDL: &str = r#"
CREATE TABLE session_run_artifact_objects (
    envelope_digest TEXT PRIMARY KEY NOT NULL CHECK (length(envelope_digest) = 64 AND envelope_digest NOT GLOB '*[^0-9a-f]*'),
    key_id TEXT NOT NULL,
    schema_version INTEGER NOT NULL CHECK (schema_version = 1),
    encryption_algorithm TEXT NOT NULL CHECK (encryption_algorithm = 'aes-256-gcm'),
    canonicalization_algorithm TEXT NOT NULL CHECK (canonicalization_algorithm = 'rfc8785-jcs'),
    canonicalization_version INTEGER NOT NULL CHECK (canonicalization_version = 1),
    redactor_version TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('unverified', 'available', 'unavailable_missing', 'unavailable_corrupt', 'unavailable_key_revoked', 'retiring', 'retired')),
    state_reason TEXT CHECK (state_reason IS NULL OR state_reason IN ('missing', 'corrupt', 'key_revoked', 'retention_deleted')),
    transition_sequence INTEGER NOT NULL CHECK (transition_sequence >= 0),
    tombstoned_at_ms INTEGER,
    retired_at_ms INTEGER,
    CHECK (
        (state IN ('unverified', 'available') AND state_reason IS NULL AND tombstoned_at_ms IS NULL AND retired_at_ms IS NULL)
        OR (state = 'unavailable_missing' AND state_reason = 'missing' AND tombstoned_at_ms IS NULL AND retired_at_ms IS NULL)
        OR (state = 'unavailable_corrupt' AND state_reason = 'corrupt' AND tombstoned_at_ms IS NULL AND retired_at_ms IS NULL)
        OR (state = 'unavailable_key_revoked' AND state_reason = 'key_revoked' AND tombstoned_at_ms IS NULL AND retired_at_ms IS NULL)
        OR (state = 'retiring' AND state_reason = 'retention_deleted' AND tombstoned_at_ms IS NOT NULL AND retired_at_ms IS NULL)
        OR (state = 'retired' AND state_reason = 'retention_deleted' AND tombstoned_at_ms IS NOT NULL AND retired_at_ms IS NOT NULL)
    )
)
"#;

const SESSION_RUN_ARTIFACT_REFS_DDL: &str = r#"
CREATE TABLE session_run_artifact_refs (
    event_id TEXT NOT NULL REFERENCES session_run_events(event_id) ON DELETE RESTRICT,
    run_id TEXT NOT NULL REFERENCES session_runs(run_id) ON DELETE RESTRICT,
    envelope_digest TEXT NOT NULL REFERENCES session_run_artifact_objects(envelope_digest) ON DELETE RESTRICT,
    state TEXT NOT NULL CHECK (state IN ('unverified', 'available', 'unavailable_missing', 'unavailable_corrupt', 'unavailable_key_revoked', 'retiring', 'retired')),
    state_reason TEXT CHECK (state_reason IS NULL OR state_reason IN ('missing', 'corrupt', 'key_revoked', 'retention_deleted')),
    transition_sequence INTEGER NOT NULL CHECK (transition_sequence >= 0),
    state_changed_at_ms INTEGER NOT NULL,
    PRIMARY KEY (event_id, envelope_digest),
    CHECK (
        (state IN ('unverified', 'available') AND state_reason IS NULL)
        OR (state = 'unavailable_missing' AND state_reason = 'missing')
        OR (state = 'unavailable_corrupt' AND state_reason = 'corrupt')
        OR (state = 'unavailable_key_revoked' AND state_reason = 'key_revoked')
        OR (state IN ('retiring', 'retired') AND state_reason = 'retention_deleted')
    )
)
"#;

// Frozen v4 exact-replay latch definition. Keep this literal for a verified
// v4→v5 migration; v5 adds replay-fidelity states and latch-pair integrity.
const SESSION_RUN_EXACT_REPLAY_V4_DDL: &str = r#"
CREATE TABLE session_run_exact_replay (
    run_id TEXT PRIMARY KEY NOT NULL REFERENCES session_runs(run_id) ON DELETE RESTRICT,
    exact_replay INTEGER NOT NULL CHECK (exact_replay IN (0, 1)),
    state TEXT NOT NULL CHECK (state IN ('unverified', 'available', 'unavailable_missing', 'unavailable_corrupt', 'unavailable_key_revoked', 'retiring', 'retired')),
    state_reason TEXT CHECK (state_reason IS NULL OR state_reason IN ('missing', 'corrupt', 'key_revoked', 'retention_deleted')),
    transition_sequence INTEGER NOT NULL CHECK (transition_sequence >= 0),
    latched_at_ms INTEGER,
    latched_by_event_id TEXT REFERENCES session_run_events(event_id) ON DELETE RESTRICT,
    CHECK (
        (state = 'available' AND exact_replay = 1 AND state_reason IS NULL)
        OR (state = 'unverified' AND exact_replay = 0 AND state_reason IS NULL)
        OR (state = 'unavailable_missing' AND exact_replay = 0 AND state_reason = 'missing')
        OR (state = 'unavailable_corrupt' AND exact_replay = 0 AND state_reason = 'corrupt')
        OR (state = 'unavailable_key_revoked' AND exact_replay = 0 AND state_reason = 'key_revoked')
        OR (state IN ('retiring', 'retired') AND exact_replay = 0 AND state_reason = 'retention_deleted')
    )
)
"#;

const SESSION_RUN_EXACT_REPLAY_DDL: &str = r#"
CREATE TABLE session_run_exact_replay (
    run_id TEXT PRIMARY KEY NOT NULL REFERENCES session_runs(run_id) ON DELETE RESTRICT,
    exact_replay INTEGER NOT NULL CHECK (exact_replay IN (0, 1)),
    state TEXT NOT NULL CHECK (state IN ('unverified', 'available', 'unavailable_missing', 'unavailable_corrupt', 'unavailable_key_revoked', 'unavailable_sanitized', 'retiring', 'retired')),
    state_reason TEXT CHECK (state_reason IS NULL OR state_reason IN ('missing', 'corrupt', 'key_revoked', 'retention_deleted', 'sanitized')),
    transition_sequence INTEGER NOT NULL CHECK (transition_sequence >= 0),
    latched_at_ms INTEGER,
    latched_by_event_id TEXT REFERENCES session_run_events(event_id) ON DELETE RESTRICT,
    CHECK (
        (state = 'available' AND exact_replay = 1 AND state_reason IS NULL)
        OR (state = 'unverified' AND exact_replay = 0 AND state_reason IS NULL)
        OR (state = 'unavailable_missing' AND exact_replay = 0 AND state_reason = 'missing')
        OR (state = 'unavailable_corrupt' AND exact_replay = 0 AND state_reason = 'corrupt')
        OR (state = 'unavailable_key_revoked' AND exact_replay = 0 AND state_reason = 'key_revoked')
        OR (state = 'unavailable_sanitized' AND exact_replay = 0 AND state_reason = 'sanitized')
        OR (state IN ('retiring', 'retired') AND exact_replay = 0 AND state_reason = 'retention_deleted')
    ),
    CHECK (
        (transition_sequence = 0 AND latched_at_ms IS NULL AND latched_by_event_id IS NULL)
        OR (transition_sequence > 0 AND latched_at_ms IS NOT NULL AND latched_by_event_id IS NOT NULL)
    )
)
"#;

const SESSION_RUN_REPLAY_APPEND_CONTRACTS_DDL: &str = r#"
CREATE TABLE session_run_replay_append_contracts (
    append_key TEXT PRIMARY KEY NOT NULL REFERENCES session_run_append_reservations(append_key) ON DELETE RESTRICT,
    run_id TEXT NOT NULL REFERENCES session_runs(run_id) ON DELETE RESTRICT,
    reserved_event_id TEXT NOT NULL,
    operation_kind TEXT NOT NULL CHECK (operation_kind IN ('run', 'model', 'tool', 'delegation', 'oauth', 'delivery', 'agent_output')),
    phase TEXT NOT NULL CHECK (phase IN ('request', 'result')),
    resolution_source_event_id TEXT,
    requested_fidelity TEXT NOT NULL CHECK (requested_fidelity IN ('exact_private', 'metadata_only')),
    created_at_ms INTEGER NOT NULL
)
"#;

const SESSION_RUN_REPLAY_OBLIGATIONS_V5_DDL: &str = r#"
CREATE TABLE session_run_replay_obligations (
    run_id TEXT NOT NULL REFERENCES session_runs(run_id) ON DELETE RESTRICT,
    source_event_id TEXT NOT NULL REFERENCES session_run_events(event_id) ON DELETE RESTRICT,
    operation_kind TEXT NOT NULL CHECK (operation_kind IN ('run', 'model', 'tool', 'delegation', 'oauth', 'delivery', 'agent_output')),
    phase TEXT NOT NULL CHECK (phase IN ('request', 'result')),
    replay_fidelity TEXT NOT NULL CHECK (replay_fidelity IN ('pending', 'exact_private', 'metadata_only', 'unavailable')),
    resolved_event_id TEXT REFERENCES session_run_events(event_id) ON DELETE RESTRICT,
    created_at_ms INTEGER NOT NULL,
    resolved_at_ms INTEGER,
    PRIMARY KEY (run_id, source_event_id, operation_kind, phase),
    CHECK (
        (replay_fidelity = 'pending' AND resolved_event_id IS NULL AND resolved_at_ms IS NULL)
        OR (replay_fidelity IN ('exact_private', 'metadata_only', 'unavailable')
            AND resolved_event_id IS NOT NULL AND resolved_at_ms IS NOT NULL)
    )
)
"#;


const SESSION_RUN_REPLAY_OBLIGATIONS_DDL: &str = r#"
CREATE TABLE session_run_replay_obligations (
    run_id TEXT NOT NULL REFERENCES session_runs(run_id) ON DELETE RESTRICT,
    source_event_id TEXT NOT NULL REFERENCES session_run_events(event_id) ON DELETE RESTRICT,
    operation_kind TEXT NOT NULL CHECK (operation_kind IN ('run', 'model', 'tool', 'delegation', 'oauth', 'delivery', 'agent_output')),
    phase TEXT NOT NULL CHECK (phase IN ('request', 'result')),
    operation_instance_id TEXT NOT NULL CHECK (length(operation_instance_id) > 0 AND length(operation_instance_id) <= 256),
    replay_fidelity TEXT NOT NULL CHECK (replay_fidelity IN ('pending', 'exact_private', 'metadata_only', 'unavailable')),
    outcome TEXT NOT NULL CHECK (outcome IN ('not_applicable', 'returned', 'handler_error', 'effect_non_resumable_unknown', 'cancelled', 'timed_out', 'unavailable')),
    resolved_event_id TEXT REFERENCES session_run_events(event_id) ON DELETE RESTRICT,
    created_at_ms INTEGER NOT NULL,
    resolved_at_ms INTEGER,
    PRIMARY KEY (run_id, source_event_id, operation_kind, phase, operation_instance_id),
    CHECK (
        (phase = 'request' AND outcome = 'not_applicable'
            AND replay_fidelity IN ('exact_private', 'metadata_only', 'unavailable')
            AND resolved_event_id IS NOT NULL AND resolved_at_ms IS NOT NULL)
        OR (phase = 'result' AND replay_fidelity = 'pending' AND outcome = 'unavailable'
            AND resolved_event_id IS NULL AND resolved_at_ms IS NULL)
        OR (phase = 'result' AND replay_fidelity IN ('exact_private', 'metadata_only', 'unavailable')
            AND outcome IN ('returned', 'handler_error', 'effect_non_resumable_unknown', 'cancelled', 'timed_out', 'unavailable')
            AND resolved_event_id IS NOT NULL AND resolved_at_ms IS NOT NULL)
    )
)
"#;

const SESSION_RUN_ARTIFACT_TRANSITIONS_DDL: &str = r#"
CREATE TABLE session_run_artifact_transitions (
    event_id TEXT NOT NULL,
    envelope_digest TEXT NOT NULL,
    run_id TEXT NOT NULL REFERENCES session_runs(run_id) ON DELETE RESTRICT,
    transition_sequence INTEGER NOT NULL CHECK (transition_sequence > 0),
    from_state TEXT NOT NULL CHECK (from_state IN ('unverified', 'available', 'unavailable_missing', 'unavailable_corrupt', 'unavailable_key_revoked', 'retiring', 'retired')),
    to_state TEXT NOT NULL CHECK (to_state IN ('unverified', 'available', 'unavailable_missing', 'unavailable_corrupt', 'unavailable_key_revoked', 'retiring', 'retired')),
    state_reason TEXT CHECK (state_reason IS NULL OR state_reason IN ('missing', 'corrupt', 'key_revoked', 'retention_deleted')),
    audit_event_id TEXT NOT NULL UNIQUE REFERENCES session_run_events(event_id) ON DELETE RESTRICT,
    created_at_ms INTEGER NOT NULL,
    PRIMARY KEY (event_id, envelope_digest, transition_sequence),
    FOREIGN KEY (event_id, envelope_digest) REFERENCES session_run_artifact_refs(event_id, envelope_digest) ON DELETE RESTRICT,
    CHECK (
        (to_state IN ('unverified', 'available') AND state_reason IS NULL)
        OR (to_state = 'unavailable_missing' AND state_reason = 'missing')
        OR (to_state = 'unavailable_corrupt' AND state_reason = 'corrupt')
        OR (to_state = 'unavailable_key_revoked' AND state_reason = 'key_revoked')
        OR (to_state IN ('retiring', 'retired') AND state_reason = 'retention_deleted')
    )
)
"#;

const IDX_SESSION_RUNS_SESSION_STARTED_DDL: &str = r#"
CREATE INDEX idx_session_runs_session_started ON session_runs(session_id, started_at_ms, run_id)
"#;
const IDX_SESSION_RUNS_OPEN_OWNER_LEASE_DDL: &str = r#"
CREATE INDEX idx_session_runs_open_owner_lease ON session_runs(exit_event_id, owner_lease_expires_at_ms, run_id)
"#;
const IDX_SESSION_RUN_EVENTS_RUN_SEQUENCE_DDL: &str = r#"
CREATE INDEX idx_session_run_events_run_sequence ON session_run_events(run_id, event_sequence)
"#;
const IDX_SESSION_RUN_NONRESUMABLE_RUN_STATE_DDL: &str = r#"
CREATE INDEX idx_session_run_nonresumable_run_state ON session_run_nonresumable_effects(run_id, state, event_id)
"#;
const IDX_SESSION_RUN_OUTBOX_CLAIM_DDL: &str = r#"
CREATE INDEX idx_session_run_outbox_claim ON session_run_outbox(state, lease_expires_at_ms, outbox_id)
"#;
const IDX_SESSION_RUN_OUTBOX_RUN_DDL: &str = r#"
CREATE INDEX idx_session_run_outbox_run ON session_run_outbox(run_id, state, outbox_id)
"#;
const IDX_SESSION_RUN_ARTIFACT_REFS_OBJECT_STATE_DDL: &str = r#"
CREATE INDEX idx_session_run_artifact_refs_object_state ON session_run_artifact_refs(envelope_digest, state, event_id)
"#;
const IDX_SESSION_RUN_ARTIFACT_TRANSITIONS_RUN_DDL: &str = r#"
CREATE INDEX idx_session_run_artifact_transitions_run ON session_run_artifact_transitions(run_id, created_at_ms, audit_event_id)
"#;
const IDX_SESSION_RUN_REPLAY_OBLIGATIONS_RUN_DDL: &str = r#"
CREATE INDEX idx_session_run_replay_obligations_run
ON session_run_replay_obligations(run_id, replay_fidelity, source_event_id)
"#;

const V2_TABLES: &[TableSpec] = &[
    TableSpec {
        name: "state_meta",
        sql: STATE_META_DDL,
        columns: &["key", "value"],
        foreign_keys: &[],
    },
    TableSpec {
        name: "session_runs",
        sql: SESSION_RUNS_DDL,
        columns: &[
            "run_id",
            "session_id",
            "parent_run_id",
            "branch_id",
            "tenant_id",
            "action_id",
            "objective_id",
            "task_id",
            "model_id",
            "provider_id",
            "config_digest",
            "prompt_digest",
            "prompt_digest_absence",
            "input_digest",
            "input_digest_absence",
            "output_digest",
            "output_digest_absence",
            "tool_digest",
            "tool_digest_absence",
            "skill_digest",
            "skill_digest_absence",
            "policy_digest",
            "policy_digest_absence",
            "sandbox_digest",
            "sandbox_digest_absence",
            "provenance_canonicalization_algorithm",
            "provenance_canonicalization_version",
            "execution_mode",
            "execution_mode_source",
            "owner_id",
            "owner_lease_generation",
            "owner_lease_token",
            "owner_lease_expires_at_ms",
            "start_event_id",
            "started_at_ms",
            "next_event_sequence",
            "head_hash",
            "exit_event_id",
            "exit_terminal_reason",
            "candidate_outcome",
            "final_outcome",
            "required_dead_lettered",
            "pending_required_delivery",
        ],
        foreign_keys: &[
            ForeignKeySpec::new("session_run_events", "exit_event_id", "event_id"),
            ForeignKeySpec::new("session_runs", "parent_run_id", "run_id"),
            ForeignKeySpec::new("session_run_events", "start_event_id", "event_id"),
        ],
    },
    TableSpec {
        name: "session_run_events",
        sql: SESSION_RUN_EVENTS_V2_DDL,
        columns: &[
            "event_id",
            "run_id",
            "event_sequence",
            "event_kind",
            "append_key",
            "effect_intent",
            "artifact_degradation_reason",
            "public_identity_jcs",
            "private_fingerprint_algorithm",
            "private_fingerprint_algorithm_version",
            "private_fingerprint_key_id",
            "private_fingerprint_key_version",
            "private_fingerprint",
            "sealed_artifact_envelope_digest",
            "sealed_artifact_key_id",
            "sealed_artifact_schema_version",
            "sealed_artifact_encryption_algorithm",
            "sealed_artifact_canonicalization_algorithm",
            "sealed_artifact_canonicalization_version",
            "sealed_artifact_redactor_version",
            "run_provenance_jcs",
            "public_payload_jcs",
            "manifest_hash",
            "previous_hash",
            "event_hash",
            "canonicalization_algorithm",
            "canonicalization_version",
            "created_at_ms",
        ],
        foreign_keys: &[ForeignKeySpec::new("session_runs", "run_id", "run_id")],
    },
    TableSpec {
        name: "session_run_manifests",
        sql: SESSION_RUN_MANIFESTS_DDL,
        columns: &[
            "event_id",
            "manifest_jcs",
            "manifest_hash",
            "canonicalization_algorithm",
            "canonicalization_version",
        ],
        foreign_keys: &[ForeignKeySpec::new(
            "session_run_events",
            "event_id",
            "event_id",
        )],
    },
    TableSpec {
        name: "session_run_append_reservations",
        sql: SESSION_RUN_APPEND_RESERVATIONS_DDL,
        columns: &[
            "append_key",
            "run_id",
            "event_id",
            "frozen_event_at_ms",
            "public_identity_jcs",
            "private_fingerprint_algorithm",
            "private_fingerprint_algorithm_version",
            "private_fingerprint_key_id",
            "private_fingerprint_key_version",
            "private_fingerprint",
            "effect_intent",
            "public_payload_jcs",
            "sealed_artifact_envelope_digest",
            "sealed_artifact_key_id",
            "sealed_artifact_schema_version",
            "sealed_artifact_encryption_algorithm",
            "sealed_artifact_canonicalization_algorithm",
            "sealed_artifact_canonicalization_version",
            "sealed_artifact_redactor_version",
            "lease_generation",
            "lease_token",
            "lease_expires_at_ms",
            "committed_event_id",
            "committed_at_ms",
            "terminal_audit_event_id",
            "terminal_degraded_at_ms",
        ],
        foreign_keys: &[
            ForeignKeySpec::new("session_run_events", "committed_event_id", "event_id"),
            ForeignKeySpec::new("session_run_events", "terminal_audit_event_id", "event_id"),
            ForeignKeySpec::new("session_runs", "run_id", "run_id"),
        ],
    },
    TableSpec {
        name: "session_run_nonresumable_effects",
        sql: SESSION_RUN_NONRESUMABLE_EFFECTS_DDL,
        columns: &[
            "event_id",
            "run_id",
            "state",
            "idempotency_key",
            "claim_token",
            "claimed_at_ms",
            "result_payload_digest",
            "result_event_id",
            "terminal_at_ms",
        ],
        foreign_keys: &[
            ForeignKeySpec::new("session_run_events", "event_id", "event_id"),
            ForeignKeySpec::new("session_run_events", "result_event_id", "event_id"),
            ForeignKeySpec::new("session_runs", "run_id", "run_id"),
        ],
    },
    TableSpec {
        name: "session_run_outbox",
        sql: SESSION_RUN_OUTBOX_DDL,
        columns: &[
            "outbox_id",
            "run_id",
            "event_id",
            "manifest_order",
            "destination",
            "required",
            "idempotency_key",
            "public_payload_jcs",
            "sealed_artifact_envelope_digest",
            "sealed_artifact_key_id",
            "sealed_artifact_schema_version",
            "sealed_artifact_encryption_algorithm",
            "sealed_artifact_canonicalization_algorithm",
            "sealed_artifact_canonicalization_version",
            "sealed_artifact_redactor_version",
            "state",
            "attempt_count",
            "max_attempts",
            "lease_generation",
            "lease_token",
            "lease_expires_at_ms",
            "delivery_receipt",
            "delivery_error",
            "terminal_at_ms",
        ],
        foreign_keys: &[
            ForeignKeySpec::new("session_run_events", "event_id", "event_id"),
            ForeignKeySpec::new("session_runs", "run_id", "run_id"),
        ],
    },
];

const V2_INDEXES: &[IndexSpec] = &[
    IndexSpec::new(
        "idx_session_runs_session_started",
        "session_runs",
        IDX_SESSION_RUNS_SESSION_STARTED_DDL,
    ),
    IndexSpec::new(
        "idx_session_runs_open_owner_lease",
        "session_runs",
        IDX_SESSION_RUNS_OPEN_OWNER_LEASE_DDL,
    ),
    IndexSpec::new(
        "idx_session_run_events_run_sequence",
        "session_run_events",
        IDX_SESSION_RUN_EVENTS_RUN_SEQUENCE_DDL,
    ),
    IndexSpec::new(
        "idx_session_run_nonresumable_run_state",
        "session_run_nonresumable_effects",
        IDX_SESSION_RUN_NONRESUMABLE_RUN_STATE_DDL,
    ),
    IndexSpec::new(
        "idx_session_run_outbox_claim",
        "session_run_outbox",
        IDX_SESSION_RUN_OUTBOX_CLAIM_DDL,
    ),
    IndexSpec::new(
        "idx_session_run_outbox_run",
        "session_run_outbox",
        IDX_SESSION_RUN_OUTBOX_RUN_DDL,
    ),
];

// The v1 table slice freezes the old event and outbox definitions for recognition and
// fingerprinting. Shared column and foreign-key metadata is unchanged by this migration.
const V1_TABLES: &[TableSpec] = &[
    TableSpec {
        name: "state_meta",
        sql: STATE_META_DDL,
        columns: V2_TABLES[0].columns,
        foreign_keys: V2_TABLES[0].foreign_keys,
    },
    TableSpec {
        name: "session_runs",
        sql: SESSION_RUNS_DDL,
        columns: V2_TABLES[1].columns,
        foreign_keys: V2_TABLES[1].foreign_keys,
    },
    TableSpec {
        name: "session_run_events",
        sql: SESSION_RUN_EVENTS_V1_DDL,
        columns: V2_TABLES[2].columns,
        foreign_keys: V2_TABLES[2].foreign_keys,
    },
    TableSpec {
        name: "session_run_manifests",
        sql: SESSION_RUN_MANIFESTS_DDL,
        columns: V2_TABLES[3].columns,
        foreign_keys: V2_TABLES[3].foreign_keys,
    },
    TableSpec {
        name: "session_run_append_reservations",
        sql: SESSION_RUN_APPEND_RESERVATIONS_DDL,
        columns: V2_TABLES[4].columns,
        foreign_keys: V2_TABLES[4].foreign_keys,
    },
    TableSpec {
        name: "session_run_nonresumable_effects",
        sql: SESSION_RUN_NONRESUMABLE_EFFECTS_DDL,
        columns: V2_TABLES[5].columns,
        foreign_keys: V2_TABLES[5].foreign_keys,
    },
    TableSpec {
        name: "session_run_outbox",
        sql: SESSION_RUN_OUTBOX_V1_DDL,
        columns: V2_TABLES[6].columns,
        foreign_keys: V2_TABLES[6].foreign_keys,
    },
];

const V1_INDEXES: &[IndexSpec] = &[
    IndexSpec::new(
        "idx_session_runs_session_started",
        "session_runs",
        IDX_SESSION_RUNS_SESSION_STARTED_DDL,
    ),
    IndexSpec::new(
        "idx_session_runs_open_owner_lease",
        "session_runs",
        IDX_SESSION_RUNS_OPEN_OWNER_LEASE_DDL,
    ),
    IndexSpec::new(
        "idx_session_run_events_run_sequence",
        "session_run_events",
        IDX_SESSION_RUN_EVENTS_RUN_SEQUENCE_DDL,
    ),
    IndexSpec::new(
        "idx_session_run_nonresumable_run_state",
        "session_run_nonresumable_effects",
        IDX_SESSION_RUN_NONRESUMABLE_RUN_STATE_DDL,
    ),
    IndexSpec::new(
        "idx_session_run_outbox_claim",
        "session_run_outbox",
        IDX_SESSION_RUN_OUTBOX_CLAIM_DDL,
    ),
    IndexSpec::new(
        "idx_session_run_outbox_run",
        "session_run_outbox",
        IDX_SESSION_RUN_OUTBOX_RUN_DDL,
    ),
];

// v3 introduced durable artifact state. v4 freezes that schema and upgrades
// the event table for authoritative RunStart sealed bindings.
const V4_TABLES: &[TableSpec] = &[
    TableSpec {
        name: "state_meta",
        sql: STATE_META_DDL,
        columns: V2_TABLES[0].columns,
        foreign_keys: V2_TABLES[0].foreign_keys,
    },
    TableSpec {
        name: "session_runs",
        sql: SESSION_RUNS_DDL,
        columns: V2_TABLES[1].columns,
        foreign_keys: V2_TABLES[1].foreign_keys,
    },
    TableSpec {
        name: "session_run_events",
        sql: SESSION_RUN_EVENTS_V4_DDL,
        columns: V2_TABLES[2].columns,
        foreign_keys: V2_TABLES[2].foreign_keys,
    },
    TableSpec {
        name: "session_run_manifests",
        sql: SESSION_RUN_MANIFESTS_DDL,
        columns: V2_TABLES[3].columns,
        foreign_keys: V2_TABLES[3].foreign_keys,
    },
    TableSpec {
        name: "session_run_append_reservations",
        sql: SESSION_RUN_APPEND_RESERVATIONS_DDL,
        columns: V2_TABLES[4].columns,
        foreign_keys: V2_TABLES[4].foreign_keys,
    },
    TableSpec {
        name: "session_run_nonresumable_effects",
        sql: SESSION_RUN_NONRESUMABLE_EFFECTS_DDL,
        columns: V2_TABLES[5].columns,
        foreign_keys: V2_TABLES[5].foreign_keys,
    },
    TableSpec {
        name: "session_run_outbox",
        sql: SESSION_RUN_OUTBOX_DDL,
        columns: V2_TABLES[6].columns,
        foreign_keys: V2_TABLES[6].foreign_keys,
    },
    TableSpec {
        name: "session_run_artifact_objects",
        sql: SESSION_RUN_ARTIFACT_OBJECTS_DDL,
        columns: &[
            "envelope_digest",
            "key_id",
            "schema_version",
            "encryption_algorithm",
            "canonicalization_algorithm",
            "canonicalization_version",
            "redactor_version",
            "state",
            "state_reason",
            "transition_sequence",
            "tombstoned_at_ms",
            "retired_at_ms",
        ],
        foreign_keys: &[],
    },
    TableSpec {
        name: "session_run_artifact_refs",
        sql: SESSION_RUN_ARTIFACT_REFS_DDL,
        columns: &[
            "event_id",
            "run_id",
            "envelope_digest",
            "state",
            "state_reason",
            "transition_sequence",
            "state_changed_at_ms",
        ],
        foreign_keys: &[
            ForeignKeySpec::new("session_run_artifact_objects", "envelope_digest", "envelope_digest"),
            ForeignKeySpec::new("session_run_events", "event_id", "event_id"),
            ForeignKeySpec::new("session_runs", "run_id", "run_id"),
        ],
    },
    TableSpec {
        name: "session_run_exact_replay",
        sql: SESSION_RUN_EXACT_REPLAY_V4_DDL,
        columns: &[
            "run_id",
            "exact_replay",
            "state",
            "state_reason",
            "transition_sequence",
            "latched_at_ms",
            "latched_by_event_id",
        ],
        foreign_keys: &[
            ForeignKeySpec::new("session_run_events", "latched_by_event_id", "event_id"),
            ForeignKeySpec::new("session_runs", "run_id", "run_id"),
        ],
    },
    TableSpec {
        name: "session_run_artifact_transitions",
        sql: SESSION_RUN_ARTIFACT_TRANSITIONS_DDL,
        columns: &[
            "event_id",
            "envelope_digest",
            "run_id",
            "transition_sequence",
            "from_state",
            "to_state",
            "state_reason",
            "audit_event_id",
            "created_at_ms",
        ],
        foreign_keys: &[
            ForeignKeySpec::new("session_run_artifact_refs", "envelope_digest", "envelope_digest"),
            ForeignKeySpec::new("session_run_artifact_refs", "event_id", "event_id"),
            ForeignKeySpec::new("session_run_events", "audit_event_id", "event_id"),
            ForeignKeySpec::new("session_runs", "run_id", "run_id"),
        ],
    },
];

const V4_INDEXES: &[IndexSpec] = &[
    V2_INDEXES[0],
    V2_INDEXES[1],
    V2_INDEXES[2],
    V2_INDEXES[3],
    V2_INDEXES[4],
    V2_INDEXES[5],
    IndexSpec::new(
        "idx_session_run_artifact_refs_object_state",
        "session_run_artifact_refs",
        IDX_SESSION_RUN_ARTIFACT_REFS_OBJECT_STATE_DDL,
    ),
    IndexSpec::new(
        "idx_session_run_artifact_transitions_run",
        "session_run_artifact_transitions",
        IDX_SESSION_RUN_ARTIFACT_TRANSITIONS_RUN_DDL,
    ),
];

// v5 adds typed replay-obligation fidelity while preserving the frozen v4
// event chain. The exact-replay table is rebuilt to enforce latch integrity.
const V5_TABLES: &[TableSpec] = &[
    V4_TABLES[0],
    V4_TABLES[1],
    V4_TABLES[2],
    V4_TABLES[3],
    V4_TABLES[4],
    V4_TABLES[5],
    V4_TABLES[6],
    V4_TABLES[7],
    V4_TABLES[8],
    TableSpec {
        name: "session_run_exact_replay",
        sql: SESSION_RUN_EXACT_REPLAY_DDL,
        columns: &[
            "run_id",
            "exact_replay",
            "state",
            "state_reason",
            "transition_sequence",
            "latched_at_ms",
            "latched_by_event_id",
        ],
        foreign_keys: &[
            ForeignKeySpec::new("session_run_events", "latched_by_event_id", "event_id"),
            ForeignKeySpec::new("session_runs", "run_id", "run_id"),
        ],
    },
    V4_TABLES[10],
    TableSpec {
        name: "session_run_replay_append_contracts",
        sql: SESSION_RUN_REPLAY_APPEND_CONTRACTS_DDL,
        columns: &[
            "append_key",
            "run_id",
            "reserved_event_id",
            "operation_kind",
            "phase",
            "resolution_source_event_id",
            "requested_fidelity",
            "created_at_ms",
        ],
        foreign_keys: &[
            ForeignKeySpec::new(
                "session_run_append_reservations",
                "append_key",
                "append_key",
            ),
            ForeignKeySpec::new("session_runs", "run_id", "run_id"),
        ],
    },
    TableSpec {
        name: "session_run_replay_obligations",
        sql: SESSION_RUN_REPLAY_OBLIGATIONS_V5_DDL,
        columns: &[
            "run_id",
            "source_event_id",
            "operation_kind",
            "phase",
            "replay_fidelity",
            "resolved_event_id",
            "created_at_ms",
            "resolved_at_ms",
        ],
        foreign_keys: &[
            ForeignKeySpec::new("session_run_events", "resolved_event_id", "event_id"),
            ForeignKeySpec::new("session_run_events", "source_event_id", "event_id"),
            ForeignKeySpec::new("session_runs", "run_id", "run_id"),
        ],
    },
];

const V5_INDEXES: &[IndexSpec] = &[
    V4_INDEXES[0],
    V4_INDEXES[1],
    V4_INDEXES[2],
    V4_INDEXES[3],
    V4_INDEXES[4],
    V4_INDEXES[5],
    V4_INDEXES[6],
    V4_INDEXES[7],
    IndexSpec::new(
        "idx_session_run_replay_obligations_run",
        "session_run_replay_obligations",
        IDX_SESSION_RUN_REPLAY_OBLIGATIONS_RUN_DDL,
    ),
];

const V6_TABLES: &[TableSpec] = &[
    V5_TABLES[0],
    V5_TABLES[1],
    V5_TABLES[2],
    V5_TABLES[3],
    V5_TABLES[4],
    V5_TABLES[5],
    V5_TABLES[6],
    V5_TABLES[7],
    V5_TABLES[8],
    V5_TABLES[9],
    V5_TABLES[10],
    V5_TABLES[11],
    TableSpec {
        name: "session_run_replay_obligations",
        sql: SESSION_RUN_REPLAY_OBLIGATIONS_DDL,
        columns: &[
            "run_id",
            "source_event_id",
            "operation_kind",
            "phase",
            "operation_instance_id",
            "replay_fidelity",
            "outcome",
            "resolved_event_id",
            "created_at_ms",
            "resolved_at_ms",
        ],
        foreign_keys: &[
            ForeignKeySpec::new("session_run_events", "resolved_event_id", "event_id"),
            ForeignKeySpec::new("session_run_events", "source_event_id", "event_id"),
            ForeignKeySpec::new("session_runs", "run_id", "run_id"),
        ],
    },
];
const V6_INDEXES: &[IndexSpec] = V5_INDEXES;


// Do not derive this from the current event definition: a v3 marker must
// continue to validate against the exact frozen v3 DDL before v3→v4 rebuild.
const V3_TABLES: &[TableSpec] = &[
    V4_TABLES[0],
    V4_TABLES[1],
    TableSpec {
        name: "session_run_events",
        sql: SESSION_RUN_EVENTS_V3_DDL,
        columns: V2_TABLES[2].columns,
        foreign_keys: V2_TABLES[2].foreign_keys,
    },
    V4_TABLES[3],
    V4_TABLES[4],
    V4_TABLES[5],
    V4_TABLES[6],
    V4_TABLES[7],
    V4_TABLES[8],
    V4_TABLES[9],
    V4_TABLES[10],
];
const V3_INDEXES: &[IndexSpec] = V4_INDEXES;

#[derive(Clone, Copy)]
struct TableSpec {
    name: &'static str,
    sql: &'static str,
    columns: &'static [&'static str],
    foreign_keys: &'static [ForeignKeySpec],
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct ForeignKeySpec {
    table: &'static str,
    from: &'static str,
    to: &'static str,
}

impl ForeignKeySpec {
    const fn new(table: &'static str, from: &'static str, to: &'static str) -> Self {
        Self { table, from, to }
    }
}

#[derive(Clone, Copy)]
struct IndexSpec {
    name: &'static str,
    table: &'static str,
    sql: &'static str,
}

impl IndexSpec {
    const fn new(name: &'static str, table: &'static str, sql: &'static str) -> Self {
        Self { name, table, sql }
    }
}

#[derive(Debug, Clone)]
pub struct LedgerConnectionFactory {
    database_path: PathBuf,
    busy_timeout: Duration,
}

impl LedgerConnectionFactory {
    pub fn new(database_path: impl Into<PathBuf>) -> Self {
        Self {
            database_path: database_path.into(),
            busy_timeout: DEFAULT_BUSY_TIMEOUT,
        }
    }

    pub fn with_busy_timeout(
        database_path: impl Into<PathBuf>,
        busy_timeout: Duration,
    ) -> LedgerResult<Self> {
        validate_busy_timeout(busy_timeout)?;
        Ok(Self {
            database_path: database_path.into(),
            busy_timeout,
        })
    }

    pub fn database_path(&self) -> &Path {
        &self.database_path
    }

    pub fn busy_timeout(&self) -> Duration {
        self.busy_timeout
    }

    /// Open a fully configured and exactly validated ledger connection.
    pub(crate) fn open(&self) -> LedgerResult<Connection> {
        let connection = self.open_configured()?;
        validate_markers_and_schema(&connection)?;
        Ok(connection)
    }

    /// Atomically create or upgrade to the only supported v4 ledger schema.
    pub fn migrate_and_cut_over_v0_to_v1(&self) -> LedgerResult<()> {
        self.migrate_inner(MigrationAbort::Never)
    }

    pub fn validate_cutover(&self) -> LedgerResult<()> {
        let connection = self.open_configured()?;
        validate_markers_and_schema(&connection)
    }

    pub(crate) fn open_configured(&self) -> LedgerResult<Connection> {
        let connection = Connection::open(&self.database_path)?;
        configure_connection(&connection, self.busy_timeout)?;
        Ok(connection)
    }

    #[cfg(test)]
    pub(crate) fn migrate_and_abort_for_test(&self) -> LedgerResult<()> {
        self.migrate_inner(MigrationAbort::AfterSchemaValidation)
    }

    #[cfg(test)]
    pub(crate) fn migrate_and_abort_after_markers_for_test(&self) -> LedgerResult<()> {
        self.migrate_inner(MigrationAbort::AfterMarkerWrite)
    }

    #[cfg(test)]
    pub(crate) fn v1_migration_operation_count_for_test() -> usize {
        V1_MIGRATION_OPERATIONS.len()
    }

    #[cfg(test)]
    pub(crate) fn v1_migration_commit_boundary_count_for_test() -> usize {
        V1_MIGRATION_COMMIT_BOUNDARIES.len()
    }

    #[cfg(test)]
    pub(crate) fn v1_migration_checkpoint_count_for_test() -> usize {
        V1_MIGRATION_OPERATIONS.len() * 2 + V1_MIGRATION_COMMIT_BOUNDARIES.len()
    }

    #[cfg(test)]
    pub(crate) fn migrate_and_abort_at_v1_checkpoint_for_test(
        &self,
        checkpoint_index: usize,
    ) -> LedgerResult<()> {
        let checkpoint = v1_migration_checkpoint_at(checkpoint_index).ok_or_else(|| {
            LedgerError::SchemaValidation(format!(
                "unknown v1 migration checkpoint index {checkpoint_index}"
            ))
        })?;
        self.migrate_inner(MigrationAbort::At(checkpoint))
    }

    #[cfg(test)]
    pub(crate) fn v2_migration_operation_count_for_test() -> usize {
        V2_MIGRATION_OPERATIONS.len()
    }

    #[cfg(test)]
    pub(crate) fn v2_migration_commit_boundary_count_for_test() -> usize {
        V2_MIGRATION_COMMIT_BOUNDARIES.len()
    }

    #[cfg(test)]
    pub(crate) fn v2_migration_checkpoint_count_for_test() -> usize {
        V2_MIGRATION_OPERATIONS.len() * 2 + V2_MIGRATION_COMMIT_BOUNDARIES.len()
    }

    #[cfg(test)]
    pub(crate) fn migrate_and_abort_at_v2_checkpoint_for_test(
        &self,
        checkpoint_index: usize,
    ) -> LedgerResult<()> {
        let checkpoint = v2_migration_checkpoint_at(checkpoint_index).ok_or_else(|| {
            LedgerError::SchemaValidation(format!(
                "unknown v2 migration checkpoint index {checkpoint_index}"
            ))
        })?;
        self.migrate_inner(MigrationAbort::At(checkpoint))
    }

    #[cfg(test)]
    pub(crate) fn v2_migration_checkpoint_case_for_test(
        checkpoint_index: usize,
    ) -> Option<(String, &'static str)> {
        let checkpoint = v2_migration_checkpoint_at(checkpoint_index)?;
        Some((
            checkpoint.step.crash_step_name(),
            checkpoint.boundary.as_str(),
        ))
    }

    #[cfg(test)]
    pub(crate) fn migrate_and_hard_abort_at_v2_checkpoint_for_test(
        &self,
        step: &str,
        boundary: &str,
    ) -> LedgerResult<()> {
        let checkpoint = v2_migration_checkpoint_from_case(step, boundary).ok_or_else(|| {
            LedgerError::SchemaValidation(format!(
                "unknown v2 migration crash checkpoint step={step:?}, boundary={boundary:?}"
            ))
        })?;
        self.migrate_inner(MigrationAbort::HardAbortAt(checkpoint))
    }

    #[cfg(test)]
    pub(crate) fn v1_migration_checkpoint_case_for_test(
        checkpoint_index: usize,
    ) -> Option<(String, &'static str)> {
        let checkpoint = v1_migration_checkpoint_at(checkpoint_index)?;
        Some((
            checkpoint.step.crash_step_name(),
            checkpoint.boundary.as_str(),
        ))
    }

    #[cfg(test)]
    pub(crate) fn migrate_and_hard_abort_at_v1_checkpoint_for_test(
        &self,
        step: &str,
        boundary: &str,
    ) -> LedgerResult<()> {
        let checkpoint = v1_migration_checkpoint_from_case(step, boundary).ok_or_else(|| {
            LedgerError::SchemaValidation(format!(
                "unknown v1 migration crash checkpoint step={step:?}, boundary={boundary:?}"
            ))
        })?;
        self.migrate_inner(MigrationAbort::HardAbortAt(checkpoint))
    }

    #[cfg(test)]
    pub(crate) fn create_v1_schema_for_test(&self) -> LedgerResult<()> {
        let mut connection = self.open_configured()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        ensure_state_meta(&transaction)?;
        create_v1_schema(&transaction)?;
        validate_v1_schema(&transaction)?;
        write_meta(&transaction, "run_ledger_schema_version", "1")?;
        write_meta(&transaction, "ledger_primary", "1")?;
        write_meta(
            &transaction,
            SCHEMA_FINGERPRINT_MARKER,
            &expected_v1_schema_fingerprint(),
        )?;
        transaction.commit()?;
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn create_v2_schema_for_test(&self) -> LedgerResult<()> {
        let mut connection = self.open_configured()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        ensure_state_meta(&transaction)?;
        create_v2_schema_for_test(&transaction)?;
        validate_v2_schema(&transaction)?;
        write_meta(&transaction, "run_ledger_schema_version", "2")?;
        write_meta(&transaction, "ledger_primary", "1")?;
        write_meta(
            &transaction,
            SCHEMA_FINGERPRINT_MARKER,
            &expected_v2_schema_fingerprint(),
        )?;
        transaction.commit()?;
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn create_v3_schema_for_test(&self) -> LedgerResult<()> {
        let mut connection = self.open_configured()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        ensure_state_meta(&transaction)?;
        create_frozen_v3_schema_for_test(&transaction)?;
        validate_v3_schema(&transaction)?;
        write_meta(&transaction, "run_ledger_schema_version", "3")?;
        write_meta(&transaction, "ledger_primary", "1")?;
        write_meta(
            &transaction,
            SCHEMA_FINGERPRINT_MARKER,
            &expected_v3_schema_fingerprint(),
        )?;
        transaction.commit()?;
        Ok(())
    }

    fn migrate_inner(&self, abort: MigrationAbort) -> LedgerResult<()> {
        let mut connection = self.open_configured()?;
        suspend_foreign_keys(&connection)?;
        let migration = (|| {
            let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            ensure_state_meta(&transaction)?;
            let schema_marker = read_meta(&transaction, "run_ledger_schema_version")?;
            let primary_marker = read_meta(&transaction, "ledger_primary")?;
            let fingerprint_marker = read_meta(&transaction, SCHEMA_FINGERPRINT_MARKER)?;

            enum MigrationPlan {
                CurrentV6,
                FreshV6,
                UpgradeV1,
                UpgradeV2,
                UpgradeV3,
                UpgradeV4,
                UpgradeV5,
            }

            let plan = match (&schema_marker, &primary_marker, &fingerprint_marker) {
                (Some(schema), _, _)
                    if schema
                        .parse::<i64>()
                        .is_ok_and(|version| version > LEDGER_SCHEMA_VERSION) =>
                {
                    return Err(LedgerError::NewerSchemaVersion {
                        version: schema.clone(),
                    });
                }
                (Some(schema), Some(primary), Some(fingerprint))
                    if schema == "6"
                        && primary == "1"
                        && fingerprint == &expected_schema_fingerprint() =>
                {
                    MigrationPlan::CurrentV6
                }
                (Some(schema), Some(primary), Some(_)) if schema == "6" && primary == "1" => {
                    return Err(LedgerError::SchemaFingerprintMismatch {
                        fingerprint: fingerprint_marker,
                    });
                }
                (Some(schema), Some(primary), Some(fingerprint))
                    if schema == "5"
                        && primary == "1"
                        && fingerprint == &expected_v5_schema_fingerprint() =>
                {
                    MigrationPlan::UpgradeV5
                }
                (Some(schema), Some(primary), Some(_)) if schema == "5" && primary == "1" => {
                    return Err(LedgerError::SchemaFingerprintMismatch {
                        fingerprint: fingerprint_marker,
                    });
                }
                (Some(schema), Some(primary), Some(fingerprint))
                    if schema == "4"
                        && primary == "1"
                        && fingerprint == &expected_v4_schema_fingerprint() =>
                {
                    MigrationPlan::UpgradeV4
                }
                (Some(schema), Some(primary), Some(_)) if schema == "4" && primary == "1" => {
                    return Err(LedgerError::SchemaFingerprintMismatch {
                        fingerprint: fingerprint_marker,
                    });
                }
                (Some(schema), Some(primary), Some(fingerprint))
                    if schema == "3"
                        && primary == "1"
                        && fingerprint == &expected_v3_schema_fingerprint() =>
                {
                    MigrationPlan::UpgradeV3
                }
                (Some(schema), Some(primary), Some(_)) if schema == "3" && primary == "1" => {
                    return Err(LedgerError::SchemaFingerprintMismatch {
                        fingerprint: fingerprint_marker,
                    });
                }
                (Some(schema), Some(primary), Some(fingerprint))
                    if schema == "2"
                        && primary == "1"
                        && fingerprint == &expected_v2_schema_fingerprint() =>
                {
                    MigrationPlan::UpgradeV2
                }
                (Some(schema), Some(primary), Some(_)) if schema == "2" && primary == "1" => {
                    return Err(LedgerError::SchemaFingerprintMismatch {
                        fingerprint: fingerprint_marker,
                    });
                }
                (Some(schema), Some(primary), Some(fingerprint))
                    if schema == "1"
                        && primary == "1"
                        && fingerprint == &expected_v1_schema_fingerprint() =>
                {
                    MigrationPlan::UpgradeV1
                }
                (Some(schema), Some(primary), Some(_)) if schema == "1" && primary == "1" => {
                    return Err(LedgerError::SchemaFingerprintMismatch {
                        fingerprint: fingerprint_marker,
                    });
                }
                (None, None, None) => MigrationPlan::FreshV6,
                (Some(schema), Some(primary), None) if schema == "0" && primary == "0" => {
                    MigrationPlan::FreshV6
                }
                _ => {
                    return Err(LedgerError::MarkerMismatch {
                        schema: schema_marker,
                        primary: primary_marker,
                    });
                }
            };

            match plan {
                MigrationPlan::CurrentV6 => {
                    validate_schema(&transaction)?;
                    validate_foreign_key_integrity(&transaction)?;
                    transaction.commit()?;
                    Ok(())
                }
                MigrationPlan::FreshV6 => {
                    create_v6_schema(&transaction)?;
                    finalize_v6_cutover(transaction, abort)
                }
                MigrationPlan::UpgradeV1 => {
                    validate_v1_schema(&transaction)?;
                    migrate_v1_to_v2_schema(&transaction, abort)?;
                    migrate_v2_to_v3_schema(&transaction, abort)?;
                    migrate_v3_to_v4_schema(&transaction, abort)?;
                    migrate_v4_to_v5_schema(&transaction)?;
                    migrate_v5_to_v6_schema(&transaction)?;
                    finalize_v6_cutover(transaction, abort)
                }
                MigrationPlan::UpgradeV2 => {
                    validate_v2_schema(&transaction)?;
                    migrate_v2_to_v3_schema(&transaction, abort)?;
                    migrate_v3_to_v4_schema(&transaction, abort)?;
                    migrate_v4_to_v5_schema(&transaction)?;
                    migrate_v5_to_v6_schema(&transaction)?;
                    finalize_v6_cutover(transaction, abort)
                }
                MigrationPlan::UpgradeV3 => {
                    validate_v3_schema(&transaction)?;
                    migrate_v3_to_v4_schema(&transaction, abort)?;
                    migrate_v4_to_v5_schema(&transaction)?;
                    migrate_v5_to_v6_schema(&transaction)?;
                    finalize_v6_cutover(transaction, abort)
                }
                MigrationPlan::UpgradeV4 => {
                    validate_v4_schema(&transaction)?;
                    migrate_v4_to_v5_schema(&transaction)?;
                    migrate_v5_to_v6_schema(&transaction)?;
                    finalize_v6_cutover(transaction, abort)
                }
                MigrationPlan::UpgradeV5 => {
                    validate_v5_schema(&transaction)?;
                    migrate_v5_to_v6_schema(&transaction)?;
                    finalize_v6_cutover(transaction, abort)
                }
            }
        })();
        let restore_foreign_keys = restore_foreign_keys(&connection);
        match (migration, restore_foreign_keys) {
            (Err(error), _) => Err(error),
            (Ok(()), Ok(())) => Ok(()),
            (Ok(()), Err(error)) => Err(error),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum MigrationAbort {
    Never,
    AfterSchemaValidation,
    AfterMarkerWrite,
    At(MigrationCheckpoint),
    #[cfg(test)]
    HardAbortAt(MigrationCheckpoint),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MigrationStep {
    DropIndex(&'static str),
    /// Distinct step identities keep crash injection exhaustive across the
    /// three schema generations that rebuild this same index name.
    DropV1EventSequenceIndex,
    DropV2EventSequenceIndex,
    DropV3EventSequenceIndex,
    CreateEventsTable,
    CreateOutboxTable,
    CopyEvents,
    CopyOutbox,
    DropOutboxTable,
    DropEventsTable,
    RenameEventsTable,
    RenameOutboxTable,
    CreateIndex(&'static str),
    CreateV3EventsTable,
    CopyV3Events,
    DropV2EventsTable,
    RenameV3EventsTable,
    CreateArtifactObjectsTable,
    CreateArtifactRefsTable,
    CreateExactReplayTable,
    CreateArtifactTransitionsTable,
    SeedArtifactObjects,
    SeedArtifactRefs,
    SeedExactReplay,
    CreateV4EventsTable,
    CopyV4Events,
    DropV3EventsTable,
    RenameV4EventsTable,
    CreateV4Index(&'static str),
    CreateV3Index(&'static str),
    WriteSchemaMarker,
    WritePrimaryMarker,
    WriteFingerprintMarker,
    Commit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MigrationBoundary {
    Before,
    After,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct MigrationCheckpoint {
    step: MigrationStep,
    boundary: MigrationBoundary,
}

impl MigrationCheckpoint {
    const fn before(step: MigrationStep) -> Self {
        Self {
            step,
            boundary: MigrationBoundary::Before,
        }
    }

    const fn after(step: MigrationStep) -> Self {
        Self {
            step,
            boundary: MigrationBoundary::After,
        }
    }
}

#[cfg(test)]
impl MigrationStep {
    fn crash_step_name(self) -> String {
        match self {
            Self::DropIndex(index) => format!("drop-index:{index}"),
            Self::DropV1EventSequenceIndex => "drop-v1-event-sequence-index".to_string(),
            Self::DropV2EventSequenceIndex => "drop-v2-event-sequence-index".to_string(),
            Self::DropV3EventSequenceIndex => "drop-v3-event-sequence-index".to_string(),
            Self::CreateEventsTable => "create-events-table".to_string(),
            Self::CreateOutboxTable => "create-outbox-table".to_string(),
            Self::CopyEvents => "copy-events".to_string(),
            Self::CopyOutbox => "copy-outbox".to_string(),
            Self::DropOutboxTable => "drop-outbox-table".to_string(),
            Self::DropEventsTable => "drop-events-table".to_string(),
            Self::RenameEventsTable => "rename-events-table".to_string(),
            Self::RenameOutboxTable => "rename-outbox-table".to_string(),
            Self::CreateIndex(index) => format!("create-index:{index}"),
            Self::CreateV3EventsTable => "create-v3-events-table".to_string(),
            Self::CopyV3Events => "copy-v3-events".to_string(),
            Self::DropV2EventsTable => "drop-v2-events-table".to_string(),
            Self::RenameV3EventsTable => "rename-v3-events-table".to_string(),
            Self::CreateArtifactObjectsTable => "create-artifact-objects-table".to_string(),
            Self::CreateArtifactRefsTable => "create-artifact-refs-table".to_string(),
            Self::CreateExactReplayTable => "create-exact-replay-table".to_string(),
            Self::CreateV4EventsTable => "create-v4-events-table".to_string(),
            Self::CopyV4Events => "copy-v4-events".to_string(),
            Self::DropV3EventsTable => "drop-v3-events-table".to_string(),
            Self::RenameV4EventsTable => "rename-v4-events-table".to_string(),
            Self::CreateV4Index(index) => format!("create-v4-index:{index}"),
            Self::CreateArtifactTransitionsTable => "create-artifact-transitions-table".to_string(),
            Self::SeedArtifactObjects => "seed-artifact-objects".to_string(),
            Self::SeedArtifactRefs => "seed-artifact-refs".to_string(),
            Self::SeedExactReplay => "seed-exact-replay".to_string(),
            Self::CreateV3Index(index) => format!("create-v3-index:{index}"),
            Self::WriteSchemaMarker => "write-schema-marker".to_string(),
            Self::WritePrimaryMarker => "write-primary-marker".to_string(),
            Self::WriteFingerprintMarker => "write-fingerprint-marker".to_string(),
            Self::Commit => "commit".to_string(),
        }
    }
}

#[cfg(test)]
impl MigrationBoundary {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Before => "before",
            Self::After => "after",
        }
    }

    fn from_str(value: &str) -> Option<Self> {
        match value {
            "before" => Some(Self::Before),
            "after" => Some(Self::After),
            _ => None,
        }
    }
}

#[cfg(test)]
const V1_MIGRATION_OPERATIONS: &[MigrationStep] = &[
    MigrationStep::DropIndex("idx_session_runs_session_started"),
    MigrationStep::DropIndex("idx_session_runs_open_owner_lease"),
    MigrationStep::DropV1EventSequenceIndex,
    MigrationStep::DropIndex("idx_session_run_nonresumable_run_state"),
    MigrationStep::DropIndex("idx_session_run_outbox_claim"),
    MigrationStep::DropIndex("idx_session_run_outbox_run"),
    MigrationStep::CreateEventsTable,
    MigrationStep::CreateOutboxTable,
    MigrationStep::CopyEvents,
    MigrationStep::CopyOutbox,
    MigrationStep::DropOutboxTable,
    MigrationStep::DropEventsTable,
    MigrationStep::RenameEventsTable,
    MigrationStep::RenameOutboxTable,
    MigrationStep::CreateIndex("idx_session_runs_session_started"),
    MigrationStep::CreateIndex("idx_session_runs_open_owner_lease"),
    MigrationStep::CreateIndex("idx_session_run_events_run_sequence"),
    MigrationStep::CreateIndex("idx_session_run_nonresumable_run_state"),
    MigrationStep::CreateIndex("idx_session_run_outbox_claim"),
    MigrationStep::CreateIndex("idx_session_run_outbox_run"),
    MigrationStep::DropV2EventSequenceIndex,
    MigrationStep::CreateV3EventsTable,
    MigrationStep::CopyV3Events,
    MigrationStep::DropV2EventsTable,
    MigrationStep::RenameV3EventsTable,
    MigrationStep::CreateV3Index("idx_session_run_events_run_sequence"),
    MigrationStep::CreateArtifactObjectsTable,
    MigrationStep::CreateArtifactRefsTable,
    MigrationStep::CreateExactReplayTable,
    MigrationStep::CreateArtifactTransitionsTable,
    MigrationStep::SeedArtifactObjects,
    MigrationStep::SeedArtifactRefs,
    MigrationStep::SeedExactReplay,
    MigrationStep::CreateV3Index("idx_session_run_artifact_refs_object_state"),
    MigrationStep::CreateV3Index("idx_session_run_artifact_transitions_run"),
    MigrationStep::DropV3EventSequenceIndex,
    MigrationStep::CreateV4EventsTable,
    MigrationStep::CopyV4Events,
    MigrationStep::DropV3EventsTable,
    MigrationStep::RenameV4EventsTable,
    MigrationStep::CreateV4Index("idx_session_run_events_run_sequence"),
    MigrationStep::WriteSchemaMarker,
    MigrationStep::WritePrimaryMarker,
    MigrationStep::WriteFingerprintMarker,
];

/// Every durable operation on the direct v2→v4 route.
#[cfg(test)]
const V2_MIGRATION_OPERATIONS: &[MigrationStep] = &[
    MigrationStep::DropV2EventSequenceIndex,
    MigrationStep::CreateV3EventsTable,
    MigrationStep::CopyV3Events,
    MigrationStep::DropV2EventsTable,
    MigrationStep::RenameV3EventsTable,
    MigrationStep::CreateV3Index("idx_session_run_events_run_sequence"),
    MigrationStep::CreateArtifactObjectsTable,
    MigrationStep::CreateArtifactRefsTable,
    MigrationStep::CreateExactReplayTable,
    MigrationStep::CreateArtifactTransitionsTable,
    MigrationStep::SeedArtifactObjects,
    MigrationStep::SeedArtifactRefs,
    MigrationStep::SeedExactReplay,
    MigrationStep::CreateV3Index("idx_session_run_artifact_refs_object_state"),
    MigrationStep::CreateV3Index("idx_session_run_artifact_transitions_run"),
    MigrationStep::DropV3EventSequenceIndex,
    MigrationStep::CreateV4EventsTable,
    MigrationStep::CopyV4Events,
    MigrationStep::DropV3EventsTable,
    MigrationStep::RenameV4EventsTable,
    MigrationStep::CreateV4Index("idx_session_run_events_run_sequence"),
    MigrationStep::WriteSchemaMarker,
    MigrationStep::WritePrimaryMarker,
    MigrationStep::WriteFingerprintMarker,
];

#[cfg(test)]
const V2_MIGRATION_COMMIT_BOUNDARIES: &[MigrationCheckpoint] =
    &[MigrationCheckpoint::before(MigrationStep::Commit)];

#[cfg(test)]
const V1_MIGRATION_COMMIT_BOUNDARIES: &[MigrationCheckpoint] =
    &[MigrationCheckpoint::before(MigrationStep::Commit)];

#[cfg(test)]
fn v1_migration_checkpoint_at(index: usize) -> Option<MigrationCheckpoint> {
    let operation_checkpoints = V1_MIGRATION_OPERATIONS.len() * 2;
    if index < operation_checkpoints {
        let step = V1_MIGRATION_OPERATIONS[index / 2];
        let boundary = if index % 2 == 0 {
            MigrationBoundary::Before
        } else {
            MigrationBoundary::After
        };
        return Some(MigrationCheckpoint { step, boundary });
    }
    V1_MIGRATION_COMMIT_BOUNDARIES
        .get(index - operation_checkpoints)
        .copied()
}

#[cfg(test)]
fn v2_migration_checkpoint_at(index: usize) -> Option<MigrationCheckpoint> {
    let operation_checkpoints = V2_MIGRATION_OPERATIONS.len() * 2;
    if index < operation_checkpoints {
        let step = V2_MIGRATION_OPERATIONS[index / 2];
        let boundary = if index % 2 == 0 {
            MigrationBoundary::Before
        } else {
            MigrationBoundary::After
        };
        return Some(MigrationCheckpoint { step, boundary });
    }
    V2_MIGRATION_COMMIT_BOUNDARIES
        .get(index - operation_checkpoints)
        .copied()
}

#[cfg(test)]
fn v1_migration_checkpoint_from_case(
    step_name: &str,
    boundary_name: &str,
) -> Option<MigrationCheckpoint> {
    let boundary = MigrationBoundary::from_str(boundary_name)?;
    for step in V1_MIGRATION_OPERATIONS.iter().copied() {
        if step.crash_step_name() == step_name {
            return Some(MigrationCheckpoint { step, boundary });
        }
    }
    V1_MIGRATION_COMMIT_BOUNDARIES
        .iter()
        .copied()
        .find(|checkpoint| {
            checkpoint.step.crash_step_name() == step_name && checkpoint.boundary == boundary
        })
}

#[cfg(test)]
fn v2_migration_checkpoint_from_case(
    step_name: &str,
    boundary_name: &str,
) -> Option<MigrationCheckpoint> {
    let boundary = MigrationBoundary::from_str(boundary_name)?;
    for step in V2_MIGRATION_OPERATIONS.iter().copied() {
        if step.crash_step_name() == step_name {
            return Some(MigrationCheckpoint { step, boundary });
        }
    }
    V2_MIGRATION_COMMIT_BOUNDARIES
        .iter()
        .copied()
        .find(|checkpoint| {
            checkpoint.step.crash_step_name() == step_name && checkpoint.boundary == boundary
        })
}

fn abort_at(abort: MigrationAbort, checkpoint: MigrationCheckpoint) -> LedgerResult<()> {
    #[cfg(test)]
    if abort == MigrationAbort::HardAbortAt(checkpoint) {
        std::process::abort();
    }
    if abort == MigrationAbort::At(checkpoint) {
        return Err(LedgerError::MigrationAborted);
    }
    Ok(())
}

fn run_migration_operation(
    abort: MigrationAbort,
    step: MigrationStep,
    operation: impl FnOnce() -> LedgerResult<()>,
) -> LedgerResult<()> {
    abort_at(abort, MigrationCheckpoint::before(step))?;
    operation()?;
    abort_at(abort, MigrationCheckpoint::after(step))
}

fn validate_busy_timeout(value: Duration) -> LedgerResult<()> {
    if value.is_zero() || value > MAX_BUSY_TIMEOUT {
        return Err(LedgerError::InvalidBusyTimeout {
            milliseconds: value.as_millis(),
        });
    }
    Ok(())
}

fn configure_connection(connection: &Connection, busy_timeout: Duration) -> LedgerResult<()> {
    validate_busy_timeout(busy_timeout)?;
    let journal_mode: String =
        connection.query_row("PRAGMA journal_mode=WAL", [], |row| row.get(0))?;
    if !journal_mode.eq_ignore_ascii_case("wal") {
        return Err(LedgerError::SchemaValidation(format!(
            "expected WAL journal mode, received {journal_mode:?}"
        )));
    }

    connection.execute_batch("PRAGMA synchronous=FULL; PRAGMA foreign_keys=ON;")?;
    connection.busy_timeout(busy_timeout)?;

    let observed_journal_mode: String =
        connection.query_row("PRAGMA journal_mode", [], |row| row.get(0))?;
    let synchronous: i64 = connection.query_row("PRAGMA synchronous", [], |row| row.get(0))?;
    let foreign_keys: i64 = connection.query_row("PRAGMA foreign_keys", [], |row| row.get(0))?;
    let configured_timeout: i64 =
        connection.query_row("PRAGMA busy_timeout", [], |row| row.get(0))?;

    if !observed_journal_mode.eq_ignore_ascii_case("wal")
        || synchronous != 2
        || foreign_keys != 1
        || configured_timeout != i64::try_from(busy_timeout.as_millis()).unwrap_or(i64::MAX)
    {
        return Err(LedgerError::SchemaValidation(
            "ledger connection pragmas could not be enforced".to_string(),
        ));
    }
    Ok(())
}

fn ensure_state_meta(transaction: &Transaction<'_>) -> LedgerResult<()> {
    if !table_exists(transaction, "state_meta")? {
        transaction.execute_batch(STATE_META_DDL)?;
        return Ok(());
    }
    if validate_table(transaction, &V4_TABLES[0]).is_ok() {
        return Ok(());
    }

    let actual_sql: String = transaction.query_row(
        "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'state_meta'",
        [],
        |row| row.get(0),
    )?;
    if normalize_sql(&actual_sql) != normalize_sql(LEGACY_SESSION_STATE_META_DDL) {
        return validate_table(transaction, &V4_TABLES[0]);
    }
    let invalid_rows: i64 = transaction.query_row(
        "SELECT COUNT(*) FROM state_meta WHERE key IS NULL OR value IS NULL",
        [],
        |row| row.get(0),
    )?;
    if invalid_rows != 0 {
        return Err(LedgerError::SchemaValidation(format!(
            "legacy state_meta contains {invalid_rows} null key/value row(s)"
        )));
    }

    transaction.execute_batch("ALTER TABLE state_meta RENAME TO state_meta_legacy_session_v0;")?;
    transaction.execute_batch(STATE_META_DDL)?;
    transaction.execute(
        "INSERT INTO state_meta (key, value)
         SELECT key, value FROM state_meta_legacy_session_v0",
        [],
    )?;
    transaction.execute_batch("DROP TABLE state_meta_legacy_session_v0;")?;
    validate_table(transaction, &V4_TABLES[0])
}

fn suspend_foreign_keys(connection: &Connection) -> LedgerResult<()> {
    // SQLite only applies this pragma outside a transaction; the rebuild is protected by the
    // immediate transaction started immediately afterwards.
    connection.execute_batch("PRAGMA foreign_keys=OFF;")?;
    let foreign_keys: i64 = connection.query_row("PRAGMA foreign_keys", [], |row| row.get(0))?;
    if foreign_keys != 0 {
        return Err(LedgerError::SchemaValidation(
            "ledger migration could not suspend foreign-key enforcement".to_string(),
        ));
    }
    Ok(())
}

fn restore_foreign_keys(connection: &Connection) -> LedgerResult<()> {
    connection.execute_batch("PRAGMA foreign_keys=ON;")?;
    let foreign_keys: i64 = connection.query_row("PRAGMA foreign_keys", [], |row| row.get(0))?;
    if foreign_keys != 1 {
        return Err(LedgerError::SchemaValidation(
            "ledger migration could not restore foreign-key enforcement".to_string(),
        ));
    }
    Ok(())
}

fn finalize_v6_cutover(
    transaction: Transaction<'_>,
    abort: MigrationAbort,
) -> LedgerResult<()> {
    validate_schema(&transaction)?;
    validate_foreign_key_integrity(&transaction)?;
    if abort == MigrationAbort::AfterSchemaValidation {
        return Err(LedgerError::MigrationAborted);
    }

    run_migration_operation(abort, MigrationStep::WriteSchemaMarker, || {
        write_meta(&transaction, "run_ledger_schema_version", "6")
    })?;
    run_migration_operation(abort, MigrationStep::WritePrimaryMarker, || {
        write_meta(&transaction, "ledger_primary", "1")
    })?;
    run_migration_operation(abort, MigrationStep::WriteFingerprintMarker, || {
        write_meta(
            &transaction,
            SCHEMA_FINGERPRINT_MARKER,
            &expected_schema_fingerprint(),
        )
    })?;
    if abort == MigrationAbort::AfterMarkerWrite {
        return Err(LedgerError::MigrationAborted);
    }
    validate_markers_and_schema(&transaction)?;
    abort_at(
        abort,
        MigrationCheckpoint::before(MigrationStep::Commit),
    )?;
    transaction.commit()?;
    Ok(())
}

fn create_v6_schema(transaction: &Transaction<'_>) -> LedgerResult<()> {
    create_schema(transaction, V6_TABLES, V6_INDEXES)
}

#[cfg(test)]
fn create_v2_schema_for_test(transaction: &Transaction<'_>) -> LedgerResult<()> {
    create_schema(transaction, V2_TABLES, V2_INDEXES)
}

#[cfg(test)]
fn create_frozen_v3_schema_for_test(transaction: &Transaction<'_>) -> LedgerResult<()> {
    create_schema(transaction, V3_TABLES, V3_INDEXES)
}

#[cfg(test)]
fn create_v1_schema(transaction: &Transaction<'_>) -> LedgerResult<()> {
    create_schema(transaction, V1_TABLES, V1_INDEXES)
}

fn create_schema(
    transaction: &Transaction<'_>,
    tables: &[TableSpec],
    indexes: &[IndexSpec],
) -> LedgerResult<()> {
    for table in &tables[1..] {
        if table_exists(transaction, table.name)? {
            return Err(LedgerError::SchemaValidation(format!(
                "cannot create schema over pre-existing table {:?}",
                table.name
            )));
        }
        transaction.execute_batch(table.sql)?;
    }
    for index in indexes {
        transaction.execute_batch(index.sql)?;
    }
    Ok(())
}

fn migrate_v1_to_v2_schema(
    transaction: &Transaction<'_>,
    abort: MigrationAbort,
) -> LedgerResult<()> {
    const REBUILT_EVENTS_TABLE: &str = "session_run_events_v2_rebuild";
    const REBUILT_OUTBOX_TABLE: &str = "session_run_outbox_v2_rebuild";
    for table in [REBUILT_EVENTS_TABLE, REBUILT_OUTBOX_TABLE] {
        if table_exists(transaction, table)? {
            return Err(LedgerError::SchemaValidation(format!(
                "cannot migrate over unexpected table {table:?}"
            )));
        }
    }

    for index in V1_INDEXES {
        let step = if index.name == "idx_session_run_events_run_sequence" {
            MigrationStep::DropV1EventSequenceIndex
        } else {
            MigrationStep::DropIndex(index.name)
        };
        run_migration_operation(abort, step, || {
            transaction.execute_batch(&format!("DROP INDEX {}", index.name))?;
            Ok(())
        })?;
    }

    let rebuilt_events_sql = SESSION_RUN_EVENTS_V2_DDL.replacen(
        "CREATE TABLE session_run_events",
        "CREATE TABLE session_run_events_v2_rebuild",
        1,
    );
    run_migration_operation(abort, MigrationStep::CreateEventsTable, || {
        transaction.execute_batch(&rebuilt_events_sql)?;
        Ok(())
    })?;
    let rebuilt_outbox_sql = SESSION_RUN_OUTBOX_DDL.replacen(
        "CREATE TABLE session_run_outbox",
        "CREATE TABLE session_run_outbox_v2_rebuild",
        1,
    );
    run_migration_operation(abort, MigrationStep::CreateOutboxTable, || {
        transaction.execute_batch(&rebuilt_outbox_sql)?;
        Ok(())
    })?;

    let event_columns = V2_TABLES[2].columns.join(", ");
    run_migration_operation(abort, MigrationStep::CopyEvents, || {
        transaction.execute(
            &format!(
                "INSERT INTO {REBUILT_EVENTS_TABLE} ({event_columns}) \
                 SELECT {event_columns} FROM session_run_events"
            ),
            [],
        )?;
        Ok(())
    })?;
    let outbox_columns = V2_TABLES[6].columns.join(", ");
    run_migration_operation(abort, MigrationStep::CopyOutbox, || {
        transaction.execute(
            &format!(
                "INSERT INTO {REBUILT_OUTBOX_TABLE} ({outbox_columns}) \
                 SELECT {outbox_columns} FROM session_run_outbox"
            ),
            [],
        )?;
        Ok(())
    })?;

    run_migration_operation(abort, MigrationStep::DropOutboxTable, || {
        transaction.execute_batch("DROP TABLE session_run_outbox")?;
        Ok(())
    })?;
    run_migration_operation(abort, MigrationStep::DropEventsTable, || {
        transaction.execute_batch("DROP TABLE session_run_events")?;
        Ok(())
    })?;
    run_migration_operation(abort, MigrationStep::RenameEventsTable, || {
        transaction.execute_batch(
            "ALTER TABLE session_run_events_v2_rebuild RENAME TO session_run_events",
        )?;
        Ok(())
    })?;
    run_migration_operation(abort, MigrationStep::RenameOutboxTable, || {
        transaction.execute_batch(
            "ALTER TABLE session_run_outbox_v2_rebuild RENAME TO session_run_outbox",
        )?;
        Ok(())
    })?;

    for index in V2_INDEXES {
        run_migration_operation(abort, MigrationStep::CreateIndex(index.name), || {
            transaction.execute_batch(index.sql)?;
            Ok(())
        })?;
    }
    Ok(())
}

/// Upgrade v2 ledgers without trusting historical digest presence as proof of
/// private-CAS availability. Every historical reference remains `unverified`
/// until the lifecycle coordinator verifies it while holding the store lock.
fn migrate_v2_to_v3_schema(
    transaction: &Transaction<'_>,
    abort: MigrationAbort,
) -> LedgerResult<()> {
    const REBUILT_EVENTS_TABLE: &str = "session_run_events_v3_rebuild";
    let unexpected_tables = [
        REBUILT_EVENTS_TABLE,
        "session_run_artifact_objects",
        "session_run_artifact_refs",
        "session_run_exact_replay",
        "session_run_artifact_transitions",
    ];
    for table in unexpected_tables {
        if table_exists(transaction, table)? {
            return Err(LedgerError::SchemaValidation(format!(
                "cannot migrate over unexpected table {table:?}"
            )));
        }
    }
    validate_v2_artifact_reference_aliases(transaction)?;

    run_migration_operation(
        abort,
        MigrationStep::DropV2EventSequenceIndex,
        || {
            transaction.execute_batch("DROP INDEX idx_session_run_events_run_sequence")?;
            Ok(())
        },
    )?;
    let rebuilt_events_sql = SESSION_RUN_EVENTS_V3_DDL.replacen(
        "CREATE TABLE session_run_events",
        "CREATE TABLE session_run_events_v3_rebuild",
        1,
    );
    run_migration_operation(abort, MigrationStep::CreateV3EventsTable, || {
        transaction.execute_batch(&rebuilt_events_sql)?;
        Ok(())
    })?;
    let event_columns = V2_TABLES[2].columns.join(", ");
    run_migration_operation(abort, MigrationStep::CopyV3Events, || {
        transaction.execute(
            &format!(
                "INSERT INTO {REBUILT_EVENTS_TABLE} ({event_columns}) \
                 SELECT {event_columns} FROM session_run_events"
            ),
            [],
        )?;
        Ok(())
    })?;
    run_migration_operation(abort, MigrationStep::DropV2EventsTable, || {
        transaction.execute_batch("DROP TABLE session_run_events")?;
        Ok(())
    })?;
    run_migration_operation(abort, MigrationStep::RenameV3EventsTable, || {
        transaction.execute_batch(
            "ALTER TABLE session_run_events_v3_rebuild RENAME TO session_run_events",
        )?;
        Ok(())
    })?;
    run_migration_operation(
        abort,
        MigrationStep::CreateV3Index("idx_session_run_events_run_sequence"),
        || {
            transaction.execute_batch(IDX_SESSION_RUN_EVENTS_RUN_SEQUENCE_DDL)?;
            Ok(())
        },
    )?;

    run_migration_operation(abort, MigrationStep::CreateArtifactObjectsTable, || {
        transaction.execute_batch(SESSION_RUN_ARTIFACT_OBJECTS_DDL)?;
        Ok(())
    })?;
    run_migration_operation(abort, MigrationStep::CreateArtifactRefsTable, || {
        transaction.execute_batch(SESSION_RUN_ARTIFACT_REFS_DDL)?;
        Ok(())
    })?;
    run_migration_operation(abort, MigrationStep::CreateExactReplayTable, || {
        transaction.execute_batch(SESSION_RUN_EXACT_REPLAY_V4_DDL)?;
        Ok(())
    })?;
    run_migration_operation(abort, MigrationStep::CreateArtifactTransitionsTable, || {
        transaction.execute_batch(SESSION_RUN_ARTIFACT_TRANSITIONS_DDL)?;
        Ok(())
    })?;
    run_migration_operation(abort, MigrationStep::SeedArtifactObjects, || {
        transaction.execute(
            "INSERT INTO session_run_artifact_objects (
                envelope_digest, key_id, schema_version, encryption_algorithm,
                canonicalization_algorithm, canonicalization_version, redactor_version,
                state, state_reason, transition_sequence, tombstoned_at_ms, retired_at_ms
             )
             SELECT sealed_artifact_envelope_digest, sealed_artifact_key_id,
                    sealed_artifact_schema_version, sealed_artifact_encryption_algorithm,
                    sealed_artifact_canonicalization_algorithm,
                    sealed_artifact_canonicalization_version,
                    sealed_artifact_redactor_version,
                    'unverified', NULL, 0, NULL, NULL
             FROM session_run_events
             WHERE sealed_artifact_envelope_digest IS NOT NULL
             GROUP BY sealed_artifact_envelope_digest",
            [],
        )?;
        Ok(())
    })?;
    run_migration_operation(abort, MigrationStep::SeedArtifactRefs, || {
        transaction.execute(
            "INSERT INTO session_run_artifact_refs (
                event_id, run_id, envelope_digest, state, state_reason,
                transition_sequence, state_changed_at_ms
             )
             SELECT event_id, run_id, sealed_artifact_envelope_digest,
                    'unverified', NULL, 0, created_at_ms
             FROM session_run_events
             WHERE sealed_artifact_envelope_digest IS NOT NULL",
            [],
        )?;
        Ok(())
    })?;
    run_migration_operation(abort, MigrationStep::SeedExactReplay, || {
        transaction.execute(
            "INSERT INTO session_run_exact_replay (
                run_id, exact_replay, state, state_reason, transition_sequence,
                latched_at_ms, latched_by_event_id
             )
             SELECT r.run_id,
                    CASE WHEN EXISTS (
                        SELECT 1 FROM session_run_artifact_refs a
                        WHERE a.run_id = r.run_id
                    ) THEN 0 ELSE 1 END,
                    CASE WHEN EXISTS (
                        SELECT 1 FROM session_run_artifact_refs a
                        WHERE a.run_id = r.run_id
                    ) THEN 'unverified' ELSE 'available' END,
                    NULL, 0, NULL, NULL
             FROM session_runs r",
            [],
        )?;
        Ok(())
    })?;
    for index in &V3_INDEXES[6..] {
        run_migration_operation(abort, MigrationStep::CreateV3Index(index.name), || {
            transaction.execute_batch(index.sql)?;
            Ok(())
        })?;
    }
    Ok(())
}

/// Rebuild only the event table so a valid frozen v3 ledger gains the
/// authoritative RunStart sealed-reference shape. The entire upgrade, marker,
/// and fingerprint cut-over is one immediate transaction.
fn migrate_v3_to_v4_schema(
    transaction: &Transaction<'_>,
    abort: MigrationAbort,
) -> LedgerResult<()> {
    const REBUILT_EVENTS_TABLE: &str = "session_run_events_v4_rebuild";
    if table_exists(transaction, REBUILT_EVENTS_TABLE)? {
        return Err(LedgerError::SchemaValidation(format!(
            "cannot migrate over unexpected table {REBUILT_EVENTS_TABLE:?}"
        )));
    }
    run_migration_operation(
        abort,
        MigrationStep::DropV3EventSequenceIndex,
        || {
            transaction.execute_batch("DROP INDEX idx_session_run_events_run_sequence")?;
            Ok(())
        },
    )?;
    let rebuilt_events_sql = SESSION_RUN_EVENTS_V4_DDL.replacen(
        "CREATE TABLE session_run_events",
        "CREATE TABLE session_run_events_v4_rebuild",
        1,
    );
    run_migration_operation(abort, MigrationStep::CreateV4EventsTable, || {
        transaction.execute_batch(&rebuilt_events_sql)?;
        Ok(())
    })?;
    let event_columns = V2_TABLES[2].columns.join(", ");
    run_migration_operation(abort, MigrationStep::CopyV4Events, || {
        transaction.execute(
            &format!(
                "INSERT INTO {REBUILT_EVENTS_TABLE} ({event_columns}) \
                 SELECT {event_columns} FROM session_run_events"
            ),
            [],
        )?;
        Ok(())
    })?;
    run_migration_operation(abort, MigrationStep::DropV3EventsTable, || {
        transaction.execute_batch("DROP TABLE session_run_events")?;
        Ok(())
    })?;
    run_migration_operation(abort, MigrationStep::RenameV4EventsTable, || {
        transaction.execute_batch(
            "ALTER TABLE session_run_events_v4_rebuild RENAME TO session_run_events",
        )?;
        Ok(())
    })?;
    run_migration_operation(
        abort,
        MigrationStep::CreateV4Index("idx_session_run_events_run_sequence"),
        || {
            transaction.execute_batch(IDX_SESSION_RUN_EVENTS_RUN_SEQUENCE_DDL)?;
            Ok(())
        },
    )?;
    Ok(())
}

/// Upgrade the frozen v4 replay latch to the v5 typed-fidelity model.
///
/// v4 rows carried neither closed transcript fidelity nor pending-effect
/// obligations. They are therefore deliberately reset to unverified rather
/// than trusting physical CAS presence as proof of an exact replay.
fn migrate_v4_to_v5_schema(transaction: &Transaction<'_>) -> LedgerResult<()> {
    const LEGACY_EXACT_REPLAY_TABLE: &str = "session_run_exact_replay_v4_legacy";
    if table_exists(transaction, LEGACY_EXACT_REPLAY_TABLE)? {
        return Err(LedgerError::SchemaValidation(format!(
            "cannot migrate over unexpected table {LEGACY_EXACT_REPLAY_TABLE:?}"
        )));
    }
    transaction.execute_batch(
        "ALTER TABLE session_run_exact_replay
         RENAME TO session_run_exact_replay_v4_legacy",
    )?;
    transaction.execute_batch(SESSION_RUN_EXACT_REPLAY_DDL)?;
    transaction.execute(
        "INSERT INTO session_run_exact_replay (
            run_id, exact_replay, state, state_reason, transition_sequence,
            latched_at_ms, latched_by_event_id
         )
         SELECT run_id, 0, 'unverified', NULL, 0, NULL, NULL
         FROM session_run_exact_replay_v4_legacy",
        [],
    )?;
    transaction.execute_batch("DROP TABLE session_run_exact_replay_v4_legacy")?;
    transaction.execute_batch(SESSION_RUN_REPLAY_APPEND_CONTRACTS_DDL)?;
    transaction.execute_batch(SESSION_RUN_REPLAY_OBLIGATIONS_V5_DDL)?;
    transaction.execute_batch(IDX_SESSION_RUN_REPLAY_OBLIGATIONS_RUN_DDL)?;
    Ok(())
}

/// v5 did not bind a closed outcome to each replay-critical obligation. Its
/// rows are preserved as unavailable (or still pending) rather than promoted
/// from physical artifact availability to exact replay evidence.
fn migrate_v5_to_v6_schema(transaction: &Transaction<'_>) -> LedgerResult<()> {
    const LEGACY_OBLIGATIONS_TABLE: &str = "session_run_replay_obligations_v5_legacy";
    if table_exists(transaction, LEGACY_OBLIGATIONS_TABLE)? {
        return Err(LedgerError::SchemaValidation(format!(
            "cannot migrate over unexpected table {LEGACY_OBLIGATIONS_TABLE:?}"
        )));
    }
    // SQLite keeps a named index attached to the renamed legacy table. Drop
    // it before recreating the same canonical index on the v6 table.
    transaction.execute_batch("DROP INDEX idx_session_run_replay_obligations_run")?;
    transaction.execute_batch(
        "ALTER TABLE session_run_replay_obligations
         RENAME TO session_run_replay_obligations_v5_legacy",
    )?;
    transaction.execute_batch(SESSION_RUN_REPLAY_OBLIGATIONS_DDL)?;
    transaction.execute_batch(IDX_SESSION_RUN_REPLAY_OBLIGATIONS_RUN_DDL)?;
    transaction.execute(
        "INSERT INTO session_run_replay_obligations (
            run_id, source_event_id, operation_kind, phase, operation_instance_id,
            replay_fidelity, outcome, resolved_event_id, created_at_ms, resolved_at_ms
         )
         SELECT
            run_id,
            source_event_id,
            operation_kind,
            phase,
            source_event_id,
            CASE
                WHEN phase = 'result' AND replay_fidelity = 'pending' THEN 'pending'
                ELSE 'unavailable'
            END,
            CASE WHEN phase = 'request' THEN 'not_applicable' ELSE 'unavailable' END,
            CASE
                WHEN phase = 'result' AND replay_fidelity = 'pending' THEN NULL
                ELSE COALESCE(resolved_event_id, source_event_id)
            END,
            created_at_ms,
            CASE
                WHEN phase = 'result' AND replay_fidelity = 'pending' THEN NULL
                ELSE COALESCE(resolved_at_ms, created_at_ms)
            END
         FROM session_run_replay_obligations_v5_legacy",
        [],
    )?;
    transaction.execute_batch("DROP TABLE session_run_replay_obligations_v5_legacy")?;
    transaction.execute(
        "UPDATE session_run_exact_replay
         SET exact_replay = 0,
             state = 'unverified',
             state_reason = NULL,
             transition_sequence = 0,
             latched_at_ms = NULL,
             latched_by_event_id = NULL",
        [],
    )?;
    Ok(())
}


fn validate_v2_artifact_reference_aliases(connection: &Connection) -> LedgerResult<()> {
    let conflict: Option<String> = connection
        .query_row(
            "SELECT envelope_digest
             FROM (
                 SELECT DISTINCT sealed_artifact_envelope_digest AS envelope_digest,
                                 sealed_artifact_key_id,
                                 sealed_artifact_schema_version,
                                 sealed_artifact_encryption_algorithm,
                                 sealed_artifact_canonicalization_algorithm,
                                 sealed_artifact_canonicalization_version,
                                 sealed_artifact_redactor_version
                 FROM session_run_events
                 WHERE sealed_artifact_envelope_digest IS NOT NULL
             )
             GROUP BY envelope_digest
             HAVING COUNT(*) > 1
             LIMIT 1",
            [],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(digest) = conflict {
        return Err(LedgerError::ArtifactReferenceAliasConflict { digest });
    }
    Ok(())
}

fn validate_foreign_key_integrity(connection: &Connection) -> LedgerResult<()> {
    let mut statement = connection.prepare("PRAGMA foreign_key_check")?;
    let mut rows = statement.query([])?;
    if let Some(row) = rows.next()? {
        let table: String = row.get(0)?;
        let rowid: Option<i64> = row.get(1)?;
        let parent: String = row.get(2)?;
        let foreign_key: i64 = row.get(3)?;
        return Err(LedgerError::SchemaValidation(format!(
            "foreign_key_check failed for table {table:?}, rowid {rowid:?}, parent {parent:?}, foreign key {foreign_key}"
        )));
    }
    Ok(())
}

pub(crate) fn validate_markers_and_schema(connection: &Connection) -> LedgerResult<()> {
    if !table_exists(connection, "state_meta")? {
        return Err(LedgerError::NotMigrated);
    }
    validate_table(connection, &V6_TABLES[0])?;
    let schema_marker = read_meta(connection, "run_ledger_schema_version")?;
    let primary_marker = read_meta(connection, "ledger_primary")?;
    let fingerprint_marker = read_meta(connection, SCHEMA_FINGERPRINT_MARKER)?;
    match (&schema_marker, &primary_marker, &fingerprint_marker) {
        (Some(schema), _, _)
            if schema
                .parse::<i64>()
                .is_ok_and(|version| version > LEDGER_SCHEMA_VERSION) =>
        {
            Err(LedgerError::NewerSchemaVersion {
                version: schema.clone(),
            })
        }
        (Some(schema), Some(primary), Some(fingerprint))
            if schema == "6" && primary == "1" && fingerprint == &expected_schema_fingerprint() =>
        {
            validate_schema(connection)
        }
        (Some(schema), Some(primary), Some(_)) if schema == "6" && primary == "1" => {
            Err(LedgerError::SchemaFingerprintMismatch {
                fingerprint: fingerprint_marker,
            })
        }
        (Some(schema), Some(primary), Some(fingerprint))
            if schema == "5" && primary == "1" && fingerprint == &expected_v5_schema_fingerprint() =>
        {
            validate_v5_schema(connection)?;
            Err(LedgerError::SchemaValidation(
                "ledger schema v5 requires v6 migration".to_string(),
            ))
        }
        (Some(schema), Some(primary), Some(_)) if schema == "5" && primary == "1" => {
            Err(LedgerError::SchemaFingerprintMismatch {
                fingerprint: fingerprint_marker,
            })
        }
        (Some(schema), Some(primary), Some(fingerprint))
            if schema == "4" && primary == "1" && fingerprint == &expected_v4_schema_fingerprint() =>
        {
            validate_v4_schema(connection)?;
            Err(LedgerError::SchemaValidation(
                "ledger schema v4 requires v5 migration".to_string(),
            ))
        }
        (Some(schema), Some(primary), Some(_)) if schema == "4" && primary == "1" => {
            Err(LedgerError::SchemaFingerprintMismatch {
                fingerprint: fingerprint_marker,
            })
        }
        (Some(schema), Some(primary), Some(fingerprint))
            if schema == "3" && primary == "1" && fingerprint == &expected_v3_schema_fingerprint() =>
        {
            validate_v3_schema(connection)?;
            Err(LedgerError::SchemaValidation(
                "ledger schema v3 requires v4 migration".to_string(),
            ))
        }
        (Some(schema), Some(primary), Some(_)) if schema == "3" && primary == "1" => {
            Err(LedgerError::SchemaFingerprintMismatch {
                fingerprint: fingerprint_marker,
            })
        }
        (Some(schema), Some(primary), Some(fingerprint))
            if schema == "2"
                && primary == "1"
                && fingerprint == &expected_v2_schema_fingerprint() =>
        {
            validate_v2_schema(connection)?;
            Err(LedgerError::SchemaValidation(
                "ledger schema v2 requires v4 migration".to_string(),
            ))
        }
        (Some(schema), Some(primary), Some(_)) if schema == "2" && primary == "1" => {
            Err(LedgerError::SchemaFingerprintMismatch {
                fingerprint: fingerprint_marker,
            })
        }
        (Some(schema), Some(primary), Some(fingerprint))
            if schema == "1"
                && primary == "1"
                && fingerprint == &expected_v1_schema_fingerprint() =>
        {
            validate_v1_schema(connection)?;
            Err(LedgerError::SchemaValidation(
                "ledger schema v1 requires v4 migration".to_string(),
            ))
        }
        (Some(schema), Some(primary), Some(_)) if schema == "1" && primary == "1" => {
            Err(LedgerError::SchemaFingerprintMismatch {
                fingerprint: fingerprint_marker,
            })
        }
        (None, None, None) => Err(LedgerError::NotMigrated),
        _ => Err(LedgerError::MarkerMismatch {
            schema: schema_marker,
            primary: primary_marker,
        }),
    }
}

fn validate_schema(connection: &Connection) -> LedgerResult<()> {
    validate_schema_with_tables(connection, V6_TABLES, V6_INDEXES)
}

fn validate_v5_schema(connection: &Connection) -> LedgerResult<()> {
    validate_schema_with_tables(connection, V5_TABLES, V5_INDEXES)
}

fn validate_v4_schema(connection: &Connection) -> LedgerResult<()> {
    validate_schema_with_tables(connection, V4_TABLES, V4_INDEXES)
}

fn validate_v3_schema(connection: &Connection) -> LedgerResult<()> {
    validate_schema_with_tables(connection, V3_TABLES, V3_INDEXES)
}

fn validate_v2_schema(connection: &Connection) -> LedgerResult<()> {
    validate_schema_with_tables(connection, V2_TABLES, V2_INDEXES)
}

fn validate_v1_schema(connection: &Connection) -> LedgerResult<()> {
    validate_schema_with_tables(connection, V1_TABLES, V1_INDEXES)
}

fn validate_schema_with_tables(
    connection: &Connection,
    tables: &[TableSpec],
    indexes: &[IndexSpec],
) -> LedgerResult<()> {
    for table in tables {
        validate_table(connection, table)?;
    }
    validate_indexes(connection, indexes)
}

fn validate_table(connection: &Connection, expected: &TableSpec) -> LedgerResult<()> {
    let actual_sql: Option<String> = connection
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = ?1",
            [expected.name],
            |row| row.get(0),
        )
        .optional()?;
    let actual_sql = actual_sql.ok_or_else(|| {
        LedgerError::SchemaValidation(format!("required table {:?} is missing", expected.name))
    })?;
    if normalize_sql(&actual_sql) != normalize_sql(expected.sql) {
        return Err(LedgerError::SchemaValidation(format!(
            "table {:?} definition, constraints, or checks do not exactly match the canonical schema",
            expected.name
        )));
    }
    validate_columns(connection, expected.name, expected.columns)?;
    validate_foreign_keys(connection, expected.name, expected.foreign_keys)
}

fn validate_columns(connection: &Connection, table: &str, expected: &[&str]) -> LedgerResult<()> {
    let pragma = format!("PRAGMA table_info({table})");
    let mut statement = connection.prepare(&pragma)?;
    let actual = statement
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<Result<Vec<_>, _>>()?;
    if actual != expected {
        return Err(LedgerError::SchemaValidation(format!(
            "table {table:?} columns differ from the canonical definition"
        )));
    }
    Ok(())
}

fn validate_foreign_keys(
    connection: &Connection,
    table: &str,
    expected: &[ForeignKeySpec],
) -> LedgerResult<()> {
    let pragma = format!("PRAGMA foreign_key_list({table})");
    let mut statement = connection.prepare(&pragma)?;
    let mut actual = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, String>(7)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    actual.sort();
    let mut expected = expected
        .iter()
        .map(|foreign_key| {
            (
                foreign_key.table.to_string(),
                foreign_key.from.to_string(),
                foreign_key.to.to_string(),
                "NO ACTION".to_string(),
                "RESTRICT".to_string(),
                "NONE".to_string(),
            )
        })
        .collect::<Vec<_>>();
    expected.sort();
    if actual != expected {
        return Err(LedgerError::SchemaValidation(format!(
            "table {table:?} foreign-key set differs from the canonical definition"
        )));
    }
    Ok(())
}
fn validate_indexes(connection: &Connection, indexes: &[IndexSpec]) -> LedgerResult<()> {
    let mut statement = connection.prepare(
        "SELECT name, tbl_name, sql FROM sqlite_master
         WHERE type = 'index' AND sql IS NOT NULL
           AND tbl_name IN ('session_runs', 'session_run_events', 'session_run_manifests', 'session_run_append_reservations', 'session_run_nonresumable_effects', 'session_run_outbox', 'session_run_artifact_objects', 'session_run_artifact_refs', 'session_run_exact_replay', 'session_run_artifact_transitions', 'session_run_replay_obligations')
         ORDER BY name",
    )?;
    let actual = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let mut expected = indexes
        .iter()
        .map(|index| {
            (
                index.name.to_string(),
                index.table.to_string(),
                normalize_sql(index.sql),
            )
        })
        .collect::<Vec<_>>();
    expected.sort_by(|left, right| left.0.cmp(&right.0));
    let actual = actual
        .into_iter()
        .map(|(name, table, sql)| (name, table, normalize_sql(&sql)))
        .collect::<Vec<_>>();
    if actual != expected {
        return Err(LedgerError::SchemaValidation(
            "required ledger indexes are missing, altered, or supplemented".to_string(),
        ));
    }
    Ok(())
}

fn table_exists(connection: &Connection, table: &str) -> LedgerResult<bool> {
    let present: Option<i64> = connection
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1",
            [table],
            |row| row.get(0),
        )
        .optional()?;
    Ok(present.is_some())
}

fn read_meta(connection: &Connection, key: &str) -> LedgerResult<Option<String>> {
    let value = connection
        .query_row(
            "SELECT value FROM state_meta WHERE key = ?1",
            [key],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    Ok(value)
}

fn write_meta(transaction: &Transaction<'_>, key: &str, value: &str) -> LedgerResult<()> {
    transaction.execute(
        "INSERT INTO state_meta (key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        (key, value),
    )?;
    Ok(())
}

pub(crate) fn expected_schema_fingerprint() -> String {
    schema_fingerprint(V6_SCHEMA_FINGERPRINT_DOMAIN, V6_TABLES, V6_INDEXES)
}

fn expected_v5_schema_fingerprint() -> String {
    schema_fingerprint(V5_SCHEMA_FINGERPRINT_DOMAIN, V5_TABLES, V5_INDEXES)
}

#[cfg(test)]
pub(crate) fn expected_v5_schema_fingerprint_for_test() -> String {
    expected_v5_schema_fingerprint()
}

fn expected_v4_schema_fingerprint() -> String {
    schema_fingerprint(V4_SCHEMA_FINGERPRINT_DOMAIN, V4_TABLES, V4_INDEXES)
}

fn expected_v3_schema_fingerprint() -> String {
    schema_fingerprint(V3_SCHEMA_FINGERPRINT_DOMAIN, V3_TABLES, V3_INDEXES)
}

fn expected_v2_schema_fingerprint() -> String {
    schema_fingerprint(V2_SCHEMA_FINGERPRINT_DOMAIN, V2_TABLES, V2_INDEXES)
}

fn expected_v1_schema_fingerprint() -> String {
    schema_fingerprint(V1_SCHEMA_FINGERPRINT_DOMAIN, V1_TABLES, V1_INDEXES)
}

fn schema_fingerprint(
    domain: &[u8],
    tables: &[TableSpec],
    indexes: &[IndexSpec],
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(domain);
    for table in tables {
        hasher.update(table.name.as_bytes());
        hasher.update([0]);
        hasher.update(normalize_sql(table.sql).as_bytes());
        hasher.update([0]);
    }
    for index in indexes {
        hasher.update(index.name.as_bytes());
        hasher.update([0]);
        hasher.update(normalize_sql(index.sql).as_bytes());
        hasher.update([0]);
    }
    hex::encode(hasher.finalize())
}

fn normalize_sql(sql: &str) -> String {
    let mut normalized = String::with_capacity(sql.len());
    let mut characters = sql.chars().peekable();
    let mut quote_terminator = None;

    while let Some(character) = characters.next() {
        match quote_terminator {
            Some('\'') => {
                normalized.push(character);
                if character == '\'' {
                    if matches!(characters.peek(), Some(&'\'')) {
                        normalized.push(characters.next().expect("peeked escaped quote"));
                    } else {
                        quote_terminator = None;
                    }
                }
            }
            Some('"') => {
                if character == '"' {
                    if matches!(characters.peek(), Some(&'"')) {
                        characters.next();
                        normalized.push('"');
                    } else {
                        quote_terminator = None;
                    }
                } else {
                    normalized.extend(character.to_lowercase());
                }
            }
            Some('`') => {
                if character == '`' {
                    if matches!(characters.peek(), Some(&'`')) {
                        characters.next();
                        normalized.push('`');
                    } else {
                        quote_terminator = None;
                    }
                } else {
                    normalized.extend(character.to_lowercase());
                }
            }
            Some(']') => {
                if character == ']' {
                    quote_terminator = None;
                } else {
                    normalized.extend(character.to_lowercase());
                }
            }
            Some(_) => unreachable!("only SQL quote delimiters are stored"),
            None => match character {
                '\'' => {
                    normalized.push(character);
                    quote_terminator = Some(character);
                }
                '"' | '`' => {
                    quote_terminator = Some(character);
                }
                '[' => {
                    quote_terminator = Some(']');
                }
                _ if character.is_whitespace() => {}
                _ => normalized.extend(character.to_lowercase()),
            },
        }
    }

    while normalized.ends_with(';') {
        normalized.pop();
    }
    normalized
}
