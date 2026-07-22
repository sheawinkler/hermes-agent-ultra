use std::collections::HashMap;
use std::sync::Arc;

use serde::Serialize;
use sha2::{Digest, Sha256};

use super::error::{LedgerError, LedgerResult};
use super::types::{
    CanonicalJson, DeliveryFailureReason, Destination, DownstreamIdempotencyKey,
    DurableDeliveryReceipt, EffectIntent, EventId, OutboxLease, SealedArtifactRef,
};

const IDEMPOTENCY_DOMAIN: &[u8] = b"hermes.session-run.outbox-idempotency/v1\0";

#[derive(Debug, Clone, Copy, Default)]
pub struct DispatchPolicyV1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestEntry {
    pub order: u32,
    pub destination: Destination,
    pub required: bool,
    pub idempotency_key: DownstreamIdempotencyKey,
    pub public_payload: CanonicalJson,
    pub sealed_artifact_ref: SealedArtifactRef,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeliveryManifest {
    pub entries: Vec<ManifestEntry>,
    pub sealed_artifact_ref: Option<SealedArtifactRef>,
    pub canonical_json: CanonicalJson,
    pub hash: String,
}

impl DispatchPolicyV1 {
    /// Derive the entire delivery manifest from a closed intent. Callers cannot
    /// submit a destination or partial manifest. Every effect-capable intent is
    /// bound to the already-published sealed artifact reference.
    pub fn derive(
        event_id: EventId,
        effect_intent: EffectIntent,
        public_payload: &CanonicalJson,
        sealed_artifact_ref: Option<&SealedArtifactRef>,
    ) -> LedgerResult<DeliveryManifest> {
        let entries = match effect_intent {
            EffectIntent::None => Vec::new(),
            EffectIntent::LocalOneShotV1 => Vec::new(),
            EffectIntent::AlphaRuntimeV1 => {
                let destination = Destination::AlphaRuntimeV1;
                let sealed_artifact_ref = sealed_artifact_ref.cloned().ok_or_else(|| {
                    LedgerError::IntegrityViolation(
                        "effect-capable manifest lacks a published sealed artifact reference"
                            .to_string(),
                    )
                })?;
                validate_payload(destination, public_payload)?;
                vec![ManifestEntry {
                    order: 0,
                    destination,
                    required: true,
                    idempotency_key: derive_idempotency_key(event_id, destination, 0)?,
                    public_payload: public_payload.clone(),
                    sealed_artifact_ref,
                }]
            }
        };
        canonicalize_manifest(&entries, sealed_artifact_ref.cloned())
    }
}

/// The delivery operation itself is intentionally public so an in-crate
/// production adapter can be tested independently. It is not a registration
/// capability: an arbitrary implementation cannot enter the outbox registry.
pub trait DurableOutboxDestinationHandler: Send + Sync {
    fn destination(&self) -> Destination;

    /// Implementors must make this key a durable downstream dedupe boundary
    /// before returning a receipt. This legacy-shaped entry point remains for
    /// test fixtures; certified production routes receive the full lease below.
    fn deliver(
        &self,
        idempotency_key: &DownstreamIdempotencyKey,
        public_payload: &CanonicalJson,
    ) -> Result<DurableDeliveryReceipt, DeliveryFailureReason>;

    /// A certified route receives the exact leased sealed reference, not merely
    /// a copied public envelope. The default preserves the test-only handler
    /// contract; production Alpha overrides it and exact-unseals this lease.
    fn deliver_leased(
        &self,
        lease: &OutboxLease,
    ) -> Result<DurableDeliveryReceipt, DeliveryFailureReason> {
        self.deliver(&lease.idempotency_key, &lease.public_payload)
    }
}

/// Non-forgeable registration capability. Its constructor is crate-private and
/// intentionally limited to the vetted alpha runtime adapter path; external
/// implementors of `DurableOutboxDestinationHandler` cannot register merely by
/// claiming that they dedupe.
pub struct CertifiedOutboxDestinationHandler {
    destination: Destination,
    handler: Arc<dyn DurableOutboxDestinationHandler>,
}

impl CertifiedOutboxDestinationHandler {
    pub(crate) fn alpha_runtime_v1(
        handler: Arc<dyn DurableOutboxDestinationHandler>,
    ) -> LedgerResult<Self> {
        if handler.destination() != Destination::AlphaRuntimeV1 {
            return Err(LedgerError::UncertifiedOutboxDestination {
                destination: handler.destination().as_str().to_string(),
            });
        }
        Ok(Self {
            destination: Destination::AlphaRuntimeV1,
            handler,
        })
    }
}


#[derive(Default)]
pub struct OutboxRouteRegistry {
    handlers: HashMap<Destination, Arc<dyn DurableOutboxDestinationHandler>>,
}

impl OutboxRouteRegistry {
    /// Explicitly fail closed for the former forgeable registration path.
    /// Production code must obtain the crate-private certified capability from
    /// the vetted adapter constructor below.
    pub fn register(
        &mut self,
        handler: Arc<dyn DurableOutboxDestinationHandler>,
    ) -> LedgerResult<()> {
        Err(LedgerError::UncertifiedOutboxDestination {
            destination: handler.destination().as_str().to_string(),
        })
    }

    pub(crate) fn register_certified(
        &mut self,
        certified: CertifiedOutboxDestinationHandler,
    ) -> LedgerResult<()> {
        if self.handlers.contains_key(&certified.destination) {
            return Err(LedgerError::DuplicateOutboxDestination {
                destination: certified.destination.as_str().to_string(),
            });
        }
        self.handlers
            .insert(certified.destination, certified.handler);
        Ok(())
    }

    pub fn handler_for(
        &self,
        destination: Destination,
    ) -> Option<&Arc<dyn DurableOutboxDestinationHandler>> {
        self.handlers.get(&destination)
    }

    pub(crate) fn contains(&self, destination: Destination) -> bool {
        self.handlers.contains_key(&destination)
    }

    pub(crate) fn deliver_leased(
        &self,
        lease: &OutboxLease,
    ) -> Result<DurableDeliveryReceipt, DeliveryFailureReason> {
        self.handlers
            .get(&lease.destination)
            .ok_or(DeliveryFailureReason::MissingRoute)?
            .deliver_leased(lease)
    }
}

pub(crate) fn validate_payload(
    destination: Destination,
    public_payload: &CanonicalJson,
) -> LedgerResult<()> {
    let value = public_payload
        .as_value()
        .map_err(|_| LedgerError::InvalidOutboxPayload {
            destination: destination.as_str().to_string(),
        })?;
    if !value.is_object() {
        return Err(LedgerError::InvalidOutboxPayload {
            destination: destination.as_str().to_string(),
        });
    }
    Ok(())
}

fn derive_idempotency_key(
    event_id: EventId,
    destination: Destination,
    order: u32,
) -> LedgerResult<DownstreamIdempotencyKey> {
    let mut hasher = Sha256::new();
    hasher.update(IDEMPOTENCY_DOMAIN);
    hasher.update(event_id.to_string().as_bytes());
    hasher.update([0]);
    hasher.update(destination.as_str().as_bytes());
    hasher.update([0]);
    hasher.update(order.to_be_bytes());
    DownstreamIdempotencyKey::from_sha256_hex(hex::encode(hasher.finalize()))
}

fn canonicalize_manifest(
    entries: &[ManifestEntry],
    sealed_artifact_ref: Option<SealedArtifactRef>,
) -> LedgerResult<DeliveryManifest> {
    #[derive(Serialize)]
    struct Manifest<'a> {
        canonicalization_algorithm: &'static str,
        canonicalization_version: u8,
        sealed_artifact_ref: Option<&'a SealedArtifactRef>,
        entries: Vec<ManifestEntryForCanonicalization<'a>>,
    }

    #[derive(Serialize)]
    struct ManifestEntryForCanonicalization<'a> {
        order: u32,
        destination: &'static str,
        required: bool,
        idempotency_key: &'a str,
        public_payload: serde_json::Value,
        sealed_artifact_ref: &'a SealedArtifactRef,
    }

    let entries_for_json = entries
        .iter()
        .map(|entry| {
            Ok(ManifestEntryForCanonicalization {
                order: entry.order,
                destination: entry.destination.as_str(),
                required: entry.required,
                idempotency_key: entry.idempotency_key.as_str(),
                public_payload: entry.public_payload.as_value()?,
                sealed_artifact_ref: &entry.sealed_artifact_ref,
            })
        })
        .collect::<LedgerResult<Vec<_>>>()?;
    let canonical_json = CanonicalJson::from_value(&Manifest {
        canonicalization_algorithm: super::types::CANONICALIZATION_ALGORITHM,
        canonicalization_version: super::types::CANONICALIZATION_VERSION,
        sealed_artifact_ref: sealed_artifact_ref.as_ref(),
        entries: entries_for_json,
    })?;
    let hash = sha256_hex(canonical_json.as_str().as_bytes());
    Ok(DeliveryManifest {
        entries: entries.to_vec(),
        sealed_artifact_ref,
        canonical_json,
        hash,
    })
}

pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}
