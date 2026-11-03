//! Capability evidence reduction and immutable protocol selection.
//!
//! A host gathers one [`CapabilityEvidence`] per (adapter, protocol) pair,
//! mints a revision-digested [`CapabilitySnapshot`], and pins one attempt to
//! exactly one protocol through [`select_pinned_protocol`]: the issued
//! [`PinnedProtocol`] is content-bound to the attempt, the evidence revision
//! and the session binding, so it cannot be transplanted onto another
//! attempt or replayed after the evidence moved on.

use serde::{de, Deserialize, Deserializer, Serialize, Serializer};
use sha2::{Digest, Sha256};
use std::collections::{BTreeSet, HashMap};

const MAX_IDENTIFIER_BYTES: usize = 256;
const MAX_BINDING_BYTES: usize = 512;
const MAX_EVIDENCE: usize = 128;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct BoundedText<const N: usize> {
    bytes: [u8; N],
    len: u16,
}

impl<const N: usize> BoundedText<N> {
    fn new(value: &str) -> Self {
        let mut bytes = [0; N];
        if value.len() > N || value.len() > u16::MAX as usize {
            return Self {
                bytes,
                len: u16::MAX,
            };
        }
        bytes[..value.len()].copy_from_slice(value.as_bytes());
        Self {
            bytes,
            len: value.len() as u16,
        }
    }

    fn as_str(&self) -> &str {
        if self.len == u16::MAX {
            return "";
        }
        std::str::from_utf8(&self.bytes[..self.len as usize]).unwrap_or_default()
    }

    fn valid(&self) -> bool {
        self.len != u16::MAX
    }
}

impl<const N: usize> Serialize for BoundedText<N> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        if !self.valid() {
            return Err(serde::ser::Error::custom("bounded_text_invalid"));
        }
        serializer.serialize_str(self.as_str())
    }
}

impl<'de, const N: usize> Deserialize<'de> for BoundedText<N> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        if value.len() > N {
            return Err(de::Error::custom("bounded_text_limit"));
        }
        Ok(Self::new(&value))
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProtocolKind {
    Acp,
    Native,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum AuthenticationEvidence {
    Supported(bool),
    Unsupported,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AuthenticationStatus {
    Authenticated,
    Unauthenticated,
    Skipped,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct CapabilityRequirement {
    pub streaming: bool,
    pub semantic_completion: bool,
    pub exact_resume: bool,
    pub cancellation: bool,
    pub cleanup: bool,
}

impl CapabilityRequirement {
    fn satisfies(self, required: Self) -> bool {
        (!required.streaming || self.streaming)
            && (!required.semantic_completion || self.semantic_completion)
            && (!required.exact_resume || self.exact_resume)
            && (!required.cancellation || self.cancellation)
            && (!required.cleanup || self.cleanup)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CapabilityEvidence {
    adapter_id: BoundedText<MAX_IDENTIFIER_BYTES>,
    driver_id: BoundedText<MAX_IDENTIFIER_BYTES>,
    protocol: ProtocolKind,
    executable_binding: BoundedText<MAX_BINDING_BYTES>,
    installed: bool,
    executable: bool,
    authentication: AuthenticationEvidence,
    protocol_capable: bool,
    send_probe_succeeded: bool,
    operations: CapabilityRequirement,
}

impl CapabilityEvidence {
    pub fn unverified(
        adapter_id: String,
        driver_id: String,
        protocol: ProtocolKind,
        executable_binding: String,
    ) -> Self {
        Self {
            adapter_id: BoundedText::new(&adapter_id),
            driver_id: BoundedText::new(&driver_id),
            protocol,
            executable_binding: BoundedText::new(&executable_binding),
            installed: false,
            executable: false,
            authentication: AuthenticationEvidence::Supported(false),
            protocol_capable: false,
            send_probe_succeeded: false,
            operations: CapabilityRequirement::default(),
        }
    }

    fn sort_key(&self) -> (&str, ProtocolKind, &str, &str) {
        (
            self.adapter_id.as_str(),
            self.protocol,
            self.driver_id.as_str(),
            self.executable_binding.as_str(),
        )
    }

    fn execution_path_available_for(&self, required: CapabilityRequirement) -> bool {
        self.installed
            && self.executable
            && self.protocol_capable
            && self.operations.satisfies(required)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CapabilityEvidenceUpdate {
    Installed(bool),
    Executable(bool),
    Authentication(AuthenticationEvidence),
    ProtocolCapable(bool),
    SendProbeSucceeded(bool),
    Operations(CapabilityRequirement),
}

pub fn reduce_capability_evidence(
    evidence: &CapabilityEvidence,
    update: CapabilityEvidenceUpdate,
) -> CapabilityEvidence {
    let mut next = *evidence;
    match update {
        CapabilityEvidenceUpdate::Installed(value) => next.installed = value,
        CapabilityEvidenceUpdate::Executable(value) => next.executable = value,
        CapabilityEvidenceUpdate::Authentication(value) => next.authentication = value,
        CapabilityEvidenceUpdate::ProtocolCapable(value) => next.protocol_capable = value,
        CapabilityEvidenceUpdate::SendProbeSucceeded(value) => next.send_probe_succeeded = value,
        CapabilityEvidenceUpdate::Operations(value) => next.operations = value,
    }
    next
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EvidenceError {
    InvalidAuthenticationProjection,
    InvalidEvidence,
    CapabilityRevisionMismatch,
}

pub fn project_authentication_evidence(
    probe_supported: bool,
    status: AuthenticationStatus,
) -> Result<AuthenticationEvidence, EvidenceError> {
    match (probe_supported, status) {
        (true, AuthenticationStatus::Authenticated) => Ok(AuthenticationEvidence::Supported(true)),
        (true, AuthenticationStatus::Unauthenticated) => {
            Ok(AuthenticationEvidence::Supported(false))
        }
        (false, AuthenticationStatus::Skipped) => Ok(AuthenticationEvidence::Unsupported),
        _ => Err(EvidenceError::InvalidAuthenticationProjection),
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PersistedCapabilitySnapshot {
    pub evidence: Vec<CapabilityEvidence>,
    pub revision: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CapabilitySnapshot {
    evidence: Vec<CapabilityEvidence>,
    revision: String,
    index: HashMap<(String, ProtocolKind), usize>,
}

impl CapabilitySnapshot {
    pub fn mint(evidence: Vec<CapabilityEvidence>) -> Result<Self, EvidenceError> {
        let evidence = canonical_evidence(evidence)?;
        let revision = evidence_revision(&evidence)?;
        let index = evidence
            .iter()
            .enumerate()
            .map(|(position, item)| {
                (
                    (item.adapter_id.as_str().to_owned(), item.protocol),
                    position,
                )
            })
            .collect();
        Ok(Self {
            evidence,
            revision,
            index,
        })
    }

    pub fn advance(&self, evidence: Vec<CapabilityEvidence>) -> Result<Self, EvidenceError> {
        Self::mint(evidence)
    }

    pub fn persisted(&self) -> PersistedCapabilitySnapshot {
        PersistedCapabilitySnapshot {
            evidence: self.evidence.clone(),
            revision: self.revision.clone(),
        }
    }

    pub fn restore(persisted: PersistedCapabilitySnapshot) -> Result<Self, EvidenceError> {
        let reminted = Self::mint(persisted.evidence)?;
        if reminted.revision != persisted.revision {
            return Err(EvidenceError::CapabilityRevisionMismatch);
        }
        Ok(reminted)
    }

    pub fn revision(&self) -> &str {
        &self.revision
    }

    #[doc(hidden)]
    pub fn contains_pin(&self, pin: &PinnedProtocol) -> bool {
        self.revision == pin.capability_revision
            && self
                .index
                .get(&(pin.adapter_id.clone(), pin.protocol))
                .and_then(|position| self.evidence.get(*position))
                .is_some_and(|item| {
                    item.adapter_id.as_str() == pin.adapter_id
                        && item.driver_id.as_str() == pin.driver_id
                        && item.protocol == pin.protocol
                        && item.executable_binding.as_str() == pin.executable_binding
                })
    }
}

fn canonical_evidence(
    mut evidence: Vec<CapabilityEvidence>,
) -> Result<Vec<CapabilityEvidence>, EvidenceError> {
    if evidence.len() > MAX_EVIDENCE || evidence.iter().any(|item| !valid_evidence(item)) {
        return Err(EvidenceError::InvalidEvidence);
    }
    evidence.sort_by(|left, right| left.sort_key().cmp(&right.sort_key()));
    if evidence.windows(2).any(|pair| {
        pair[0].adapter_id == pair[1].adapter_id && pair[0].protocol == pair[1].protocol
    }) {
        return Err(EvidenceError::InvalidEvidence);
    }
    Ok(evidence)
}

fn evidence_revision(evidence: &[CapabilityEvidence]) -> Result<String, EvidenceError> {
    let bytes = serde_json::to_vec(evidence).map_err(|_| EvidenceError::InvalidEvidence)?;
    let digest = Sha256::digest(bytes);
    Ok(format!("sha256:{digest:x}"))
}

fn valid_evidence(value: &CapabilityEvidence) -> bool {
    value.adapter_id.valid()
        && value.driver_id.valid()
        && value.executable_binding.valid()
        && valid_identifier(value.adapter_id.as_str())
        && valid_identifier(value.driver_id.as_str())
        && !value.executable_binding.as_str().is_empty()
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TargetProtocolRequest {
    pub attempt_id: String,
    pub adapter_id: String,
    pub configured_protocols: Vec<ProtocolKind>,
    pub session_binding: String,
    pub required: CapabilityRequirement,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProtocolPolicy {
    pub allow_acp: bool,
    pub native_allowlist: BTreeSet<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct PinnedProtocol {
    pub attempt_id: String,
    pub adapter_id: String,
    pub driver_id: String,
    pub protocol: ProtocolKind,
    pub executable_binding: String,
    pub session_binding: String,
    pub capability_revision: String,
    /// Content binding for every field that defines the immutable attempt.
    ///
    /// Keeping this separate from the capability revision lets a capability
    /// snapshot remain reusable while preventing a caller from transplanting
    /// one field (notably `attempt_id`) onto a pin issued for another attempt.
    pub binding_digest: String,
}

impl<'de> Deserialize<'de> for PinnedProtocol {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct WirePin {
            attempt_id: String,
            adapter_id: String,
            driver_id: String,
            protocol: ProtocolKind,
            executable_binding: String,
            session_binding: String,
            capability_revision: String,
            binding_digest: String,
        }

        let wire = WirePin::deserialize(deserializer)?;
        let pin = Self {
            attempt_id: wire.attempt_id,
            adapter_id: wire.adapter_id,
            driver_id: wire.driver_id,
            protocol: wire.protocol,
            executable_binding: wire.executable_binding,
            session_binding: wire.session_binding,
            capability_revision: wire.capability_revision,
            binding_digest: wire.binding_digest,
        };
        if !valid_pin(&pin) {
            return Err(de::Error::custom("invalid_pinned_protocol"));
        }
        Ok(pin)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SelectionError {
    NoAvailableProtocol,
    InvalidOpaqueBinding,
}

pub fn select_pinned_protocol(
    target: &TargetProtocolRequest,
    capabilities: &CapabilitySnapshot,
    policy: &ProtocolPolicy,
) -> Result<PinnedProtocol, SelectionError> {
    if !valid_identifier(&target.attempt_id)
        || !valid_identifier(&target.adapter_id)
        || !valid_binding(&target.session_binding)
        || target.configured_protocols.is_empty()
        || target.configured_protocols.len() > 2
        || target
            .configured_protocols
            .iter()
            .collect::<BTreeSet<_>>()
            .len()
            != target.configured_protocols.len()
    {
        return Err(SelectionError::InvalidOpaqueBinding);
    }

    let configured = |protocol| target.configured_protocols.contains(&protocol);
    let selected = [ProtocolKind::Acp, ProtocolKind::Native]
        .into_iter()
        .filter(|protocol| configured(*protocol))
        .filter(|protocol| match protocol {
            ProtocolKind::Acp => policy.allow_acp,
            ProtocolKind::Native => policy.native_allowlist.contains(&target.adapter_id),
        })
        .find_map(|protocol| {
            capabilities
                .index
                .get(&(target.adapter_id.clone(), protocol))
                .and_then(|position| capabilities.evidence.get(*position))
                .filter(|item| item.execution_path_available_for(target.required))
        })
        .ok_or(SelectionError::NoAvailableProtocol)?;

    if !valid_binding(selected.executable_binding.as_str()) {
        return Err(SelectionError::InvalidOpaqueBinding);
    }
    let mut pin = PinnedProtocol {
        attempt_id: target.attempt_id.clone(),
        adapter_id: selected.adapter_id.as_str().to_owned(),
        driver_id: selected.driver_id.as_str().to_owned(),
        protocol: selected.protocol,
        executable_binding: selected.executable_binding.as_str().to_owned(),
        session_binding: target.session_binding.clone(),
        capability_revision: capabilities.revision.clone(),
        binding_digest: String::new(),
    };
    pin.binding_digest = pin_binding_digest(&pin);
    Ok(pin)
}

fn valid_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_IDENTIFIER_BYTES
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
}

fn valid_binding(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_BINDING_BYTES
        && !value.contains('/')
        && !value.contains('\\')
        && !looks_sensitive(value)
        && value.bytes().all(|byte| byte.is_ascii_graphic())
}

#[doc(hidden)]
pub fn valid_opaque_evidence(value: &str) -> bool {
    valid_binding(value)
}

#[doc(hidden)]
pub fn valid_pin(pin: &PinnedProtocol) -> bool {
    valid_identifier(&pin.attempt_id)
        && valid_identifier(&pin.adapter_id)
        && valid_identifier(&pin.driver_id)
        && valid_binding(&pin.executable_binding)
        && valid_binding(&pin.session_binding)
        && pin.capability_revision.starts_with("sha256:")
        && valid_binding(&pin.capability_revision)
        && pin.binding_digest.starts_with("sha256:")
        && valid_binding(&pin.binding_digest)
        && pin.binding_digest == pin_binding_digest(pin)
}

fn pin_binding_digest(pin: &PinnedProtocol) -> String {
    let mut digest = Sha256::new();
    for value in [
        pin.attempt_id.as_bytes(),
        pin.adapter_id.as_bytes(),
        pin.driver_id.as_bytes(),
        match pin.protocol {
            ProtocolKind::Acp => b"acp".as_slice(),
            ProtocolKind::Native => b"native".as_slice(),
        },
        pin.executable_binding.as_bytes(),
        pin.session_binding.as_bytes(),
        pin.capability_revision.as_bytes(),
    ] {
        digest.update((value.len() as u64).to_be_bytes());
        digest.update(value);
    }
    format!("sha256:{:x}", digest.finalize())
}

fn looks_sensitive(value: &str) -> bool {
    let normalized = value.to_ascii_lowercase().replace(['_', '-'], " ");
    [
        "raw prompt",
        "raw provider output",
        "native session id",
        "credential",
        "private path",
        "prompt canary",
    ]
    .iter()
    .any(|needle| normalized.contains(needle))
}
