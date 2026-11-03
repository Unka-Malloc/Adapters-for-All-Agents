//! Host-neutral agent runtime contracts — the L4/L5 boundary a composing
//! host answers.
//!
//! Concrete process, CLI, RPC, and approval drivers remain private to the
//! composing host. This module owns the contracts that let those drivers be
//! selected and invoked without making the host depend on a particular
//! lifetime or transport implementation:
//!
//! * identity — [`ProviderId`]: the opaque catalog identifier one runtime
//!   provider answers to, never a command, path, endpoint, or account;
//! * capability evidence — [`SubagentCapabilities`], [`ReadinessEvidence`]
//!   and [`ExecutionAdmissionEvidence`], reduced by [`reduce_readiness`]
//!   and [`reduce_execution_admission`]: missing evidence is an explicit
//!   denial, never an optimistic fallback;
//! * session binding — [`NativeResumeIdentity`] and [`DurableNativeBinding`]:
//!   adapter-owned opaque bindings with no serialization implementation, so
//!   they cannot be projected into wire payloads;
//! * dispatch vocabulary — [`SubagentDispatchRequest`],
//!   [`RuntimeTransition`], [`RuntimeDispatchReceipt`] and
//!   [`RuntimeObservation`];
//! * ports — [`McpCallerIntegration`] (provider-owned caller registration
//!   behind a single-use [`RegistrationApproval`]) and
//!   [`SubagentRuntimeAdapter`] (the target runtime the host selects through
//!   its registry), plus the lane-erased [`RuntimeDriver`] /
//!   [`RuntimeDriverRegistry`] / [`PersistentTurnRuntime`] contracts.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fmt;

use crate::error::AdapterError;

/// Providers supported by the host-owned subagent mesh. The value is an
/// opaque catalog identifier, never a command, path, endpoint, or account.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ProviderId(String);

impl ProviderId {
    pub fn parse(value: impl Into<String>) -> Result<Self, AdapterFailure> {
        let value = value.into();
        if value.is_empty()
            || value.len() > 64
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        {
            return Err(AdapterFailure::permanent(
                "provider_identity_invalid",
                "identity/validate",
            ));
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ProviderId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Provider-declared delivery for generated runtime guidance. User Events are
/// never rewritten; this policy is applied only to the ephemeral driver call.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum InstructionPolicy {
    NativeDeveloperInstructions,
    NativePrivateInstructions,
    OrdinaryWirePrefix,
}

/// Explicit capability differences consumed by direct and Graph admission.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SubagentCapabilities {
    pub create: bool,
    pub exact_resume: bool,
    pub observe: bool,
    pub continue_turn: bool,
    pub active_cancel: bool,
    pub native_steer: bool,
    pub instruction_policy: InstructionPolicy,
}

/// Evidence used by the one readiness reducer. Missing evidence is an
/// explicit denial; callers never receive an optimistic fallback.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReadinessEvidence {
    pub provider_id: String,
    pub installed: bool,
    pub identity_verified: bool,
    pub transport_ready: bool,
    pub permission_ready: bool,
    pub capability_revision: String,
    #[serde(default)]
    pub blocker_code: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReadinessDecision {
    pub ready: bool,
    pub provider_id: String,
    pub capability_revision: String,
    #[serde(default)]
    pub blocker_code: Option<String>,
}

/// Canonical facts that decide whether a selected subagent runtime can be
/// attempted. Conversation readiness is deliberately absent: it is
/// observational evidence for `probe`, not execution permission.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExecutionAdmissionEvidence {
    pub provider_id: String,
    pub installed: bool,
    pub executable_message_send_route: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExecutionAdmissionDecision {
    pub admitted: bool,
    pub blocker_code: Option<&'static str>,
}

pub fn reduce_execution_admission(
    expected_provider: &ProviderId,
    evidence: &ExecutionAdmissionEvidence,
) -> ExecutionAdmissionDecision {
    let blocker_code = if evidence.provider_id != expected_provider.as_str() {
        Some("provider_identity_mismatch")
    } else if !evidence.installed {
        Some("provider_not_installed")
    } else if !evidence.executable_message_send_route {
        Some("provider_execution_route_unavailable")
    } else {
        None
    };
    ExecutionAdmissionDecision {
        admitted: blocker_code.is_none(),
        blocker_code,
    }
}

/// Reduce readiness identically for direct and Graph dispatch.
pub fn reduce_readiness(
    expected_provider: &ProviderId,
    evidence: &ReadinessEvidence,
) -> ReadinessDecision {
    let identity_matches = evidence.provider_id == expected_provider.as_str();
    let revision_valid = !evidence.capability_revision.is_empty()
        && evidence.capability_revision.len() <= 128
        && evidence.capability_revision.bytes().all(|byte| {
            byte.is_ascii_lowercase()
                || byte.is_ascii_digit()
                || matches!(byte, b'.' | b':' | b'_' | b'-')
        });
    let ready = identity_matches
        && revision_valid
        && evidence.installed
        && evidence.identity_verified
        && evidence.transport_ready
        && evidence.permission_ready
        && evidence.blocker_code.is_none();
    let blocker_code = if ready {
        None
    } else if !identity_matches {
        Some("provider_identity_mismatch".to_owned())
    } else if !revision_valid {
        Some("capability_revision_invalid".to_owned())
    } else {
        evidence.blocker_code.clone().or_else(|| {
            (!evidence.installed)
                .then_some("provider_not_installed")
                .or_else(|| (!evidence.identity_verified).then_some("provider_identity_unverified"))
                .or_else(|| (!evidence.transport_ready).then_some("provider_transport_unready"))
                .or_else(|| (!evidence.permission_ready).then_some("provider_permission_required"))
                .map(str::to_owned)
        })
    };
    ReadinessDecision {
        ready,
        provider_id: expected_provider.as_str().to_owned(),
        capability_revision: evidence.capability_revision.clone(),
        blocker_code,
    }
}

/// Adapter-owned native identity. Its fields are intentionally private and it
/// has no serialization implementation, preventing projection into MCP/Event
/// payloads. Only the adapter that resolved it should interpret the opaque
/// binding.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NativeResumeIdentity {
    provider: ProviderId,
    binding: String,
    native_location: Option<String>,
    working_directory: Option<String>,
}

/// Private durable binding read from the host's canonical conversation state.
/// It has no serialization implementation and must never enter an Event or
/// MCP receipt.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DurableNativeBinding {
    provider: ProviderId,
    native_session_id: String,
    native_location: Option<String>,
    working_directory: Option<String>,
}

impl DurableNativeBinding {
    pub fn new(
        provider: ProviderId,
        native_session_id: impl Into<String>,
        native_location: Option<String>,
        working_directory: Option<String>,
    ) -> Result<Self, AdapterFailure> {
        let native_session_id = native_session_id.into();
        if native_session_id.is_empty()
            || native_session_id.len() > 1024
            || native_session_id.chars().any(char::is_control)
            || native_location
                .as_deref()
                .is_some_and(|value| value.is_empty() || value.len() > 4096)
            || working_directory
                .as_deref()
                .is_some_and(|value| value.is_empty() || value.len() > 4096)
        {
            return Err(AdapterFailure::permanent(
                "durable_native_binding_invalid",
                "identity/resolve",
            ));
        }
        Ok(Self {
            provider,
            native_session_id,
            native_location,
            working_directory,
        })
    }

    pub fn provider(&self) -> &ProviderId {
        &self.provider
    }

    pub fn native_session_id(&self) -> &str {
        &self.native_session_id
    }

    pub fn native_location(&self) -> Option<&str> {
        self.native_location.as_deref()
    }

    pub fn working_directory(&self) -> Option<&str> {
        self.working_directory.as_deref()
    }
}

impl NativeResumeIdentity {
    pub fn new(provider: ProviderId, binding: impl Into<String>) -> Result<Self, AdapterFailure> {
        let binding = binding.into();
        if binding.is_empty() || binding.len() > 1024 || binding.chars().any(char::is_control) {
            return Err(AdapterFailure::permanent(
                "native_resume_identity_invalid",
                "identity/resolve",
            ));
        }
        Ok(Self {
            provider,
            binding,
            native_location: None,
            working_directory: None,
        })
    }

    pub fn from_durable(
        durable: &DurableNativeBinding,
        exact_binding: impl Into<String>,
    ) -> Result<Self, AdapterFailure> {
        let mut identity = Self::new(durable.provider.clone(), exact_binding)?;
        identity.native_location = durable.native_location.clone();
        identity.working_directory = durable.working_directory.clone();
        Ok(identity)
    }

    pub fn provider(&self) -> &ProviderId {
        &self.provider
    }

    pub fn binding_for_adapter(&self, provider: &ProviderId) -> Option<&str> {
        (provider == &self.provider).then_some(self.binding.as_str())
    }

    pub fn native_location_for_adapter(&self, provider: &ProviderId) -> Option<&str> {
        (provider == &self.provider)
            .then_some(self.native_location.as_deref())
            .flatten()
    }

    pub fn working_directory_for_adapter(&self, provider: &ProviderId) -> Option<&str> {
        (provider == &self.provider)
            .then_some(self.working_directory.as_deref())
            .flatten()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SubagentDispatchRequest {
    pub conversation_id: String,
    pub caller_membership_id: String,
    pub target_membership_id: String,
    pub dispatch_id: String,
    pub prompt: String,
    pub model: Option<String>,
    pub reasoning_effort: Option<String>,
    pub permission_mode: Option<String>,
    pub working_directory: Option<String>,
    pub task_type: Option<String>,
    pub timeout_ms: Option<u64>,
    pub timeout_unbounded: bool,
    pub max_stdout_bytes: Option<u64>,
    pub max_stderr_bytes: Option<u64>,
    pub generated_guidance: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SubagentContinueRequest {
    pub dispatch: SubagentDispatchRequest,
    pub identity: NativeResumeIdentity,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RuntimeTransition {
    Accepted,
    Processing,
    Responding,
    Completed,
    Failed,
    CancelRequested,
    Cancelled,
    ReconciliationRequired,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeEventPart {
    pub transition: RuntimeTransition,
    pub visible_text: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeDispatchReceipt {
    pub dispatch_id: String,
    pub transition: RuntimeTransition,
    pub identity: Option<NativeResumeIdentity>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeObservation {
    pub dispatch_id: String,
    pub transition: RuntimeTransition,
    pub parts: Vec<RuntimeEventPart>,
}

/// The failure vocabulary every runtime port reports through: the crate's
/// one [`AdapterError`], under the name this boundary has always carried.
pub type AdapterFailure = AdapterError;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CallerRegistrationPlan {
    pub provider_id: ProviderId,
    pub content_digest: String,
    pub requires_approval: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CallerRegistrationReceipt {
    pub provider_id: ProviderId,
    pub ready_for_fresh_sessions: bool,
}

/// Single-use approval bound to one immutable registration digest.
#[derive(Debug)]
pub struct RegistrationApproval {
    digest: String,
    consumed: bool,
}

impl RegistrationApproval {
    pub fn approve(
        plan: &CallerRegistrationPlan,
        confirmed: bool,
        digest: &str,
    ) -> Result<Self, AdapterFailure> {
        if !confirmed {
            return Err(AdapterFailure::permanent(
                "caller_registration_approval_required",
                "registration/approve",
            ));
        }
        if digest != plan.content_digest {
            return Err(AdapterFailure::permanent(
                "caller_registration_approval_mismatch",
                "registration/approve",
            ));
        }
        Ok(Self {
            digest: digest.to_owned(),
            consumed: false,
        })
    }

    pub fn claim(&mut self, digest: &str) -> Result<(), AdapterFailure> {
        if self.consumed {
            return Err(AdapterFailure::permanent(
                "caller_registration_approval_consumed",
                "registration/apply",
            ));
        }
        self.consumed = true;
        if self.digest != digest {
            return Err(AdapterFailure::permanent(
                "caller_registration_approval_mismatch",
                "registration/apply",
            ));
        }
        Ok(())
    }
}

/// Provider-owned caller registration/install/readiness/removal port.
pub trait McpCallerIntegration: Send + Sync {
    fn provider_id(&self) -> &ProviderId;
    fn plan_registration(&self) -> Result<CallerRegistrationPlan, AdapterFailure>;
    fn readiness(&self) -> ReadinessEvidence;
    fn apply_registration(
        &self,
        plan: &CallerRegistrationPlan,
        approval: &mut RegistrationApproval,
    ) -> Result<CallerRegistrationReceipt, AdapterFailure>;
    fn remove_registration(
        &self,
        plan: &CallerRegistrationPlan,
        approval: &mut RegistrationApproval,
    ) -> Result<(), AdapterFailure>;
}

/// Provider-owned target runtime port. Application code selects this trait
/// through the registry and contains no provider conditional.
pub trait SubagentRuntimeAdapter: Send + Sync {
    fn provider_id(&self) -> &ProviderId;
    fn capabilities(&self) -> SubagentCapabilities;
    fn execution_admission(&self) -> ExecutionAdmissionEvidence;
    fn readiness(&self) -> ReadinessEvidence;
    fn resolve_resume_identity(
        &self,
        durable_binding: &DurableNativeBinding,
    ) -> Result<NativeResumeIdentity, AdapterFailure>;
    fn send(
        &self,
        request: &SubagentDispatchRequest,
    ) -> Result<RuntimeDispatchReceipt, AdapterFailure>;
    fn continue_turn(
        &self,
        request: &SubagentContinueRequest,
    ) -> Result<RuntimeDispatchReceipt, AdapterFailure>;
    fn observe(&self, dispatch_id: &str) -> Result<RuntimeObservation, AdapterFailure>;
    fn cancel_active(
        &self,
        dispatch_id: &str,
        identity: &NativeResumeIdentity,
    ) -> Result<RuntimeDispatchReceipt, AdapterFailure>;
    fn cleanup(&self, identity: &NativeResumeIdentity) -> Result<(), AdapterFailure>;
}

/// Stable identity and capability surface of one runtime driver.
///
/// Identifiers are protocol/catalog names. Implementations must not expose an
/// executable path, process identifier, credential, or machine identity here.
pub trait RuntimeDriver: Send + Sync {
    type Error: std::error::Error + Send + Sync + 'static;

    fn id(&self) -> &'static str;
    fn capabilities(&self) -> Value;
    fn dispatch(&self, operation: &str, request: &Value) -> Result<Value, Self::Error>;
}

/// Read-only driver selection. The composing host owns registration and
/// process lifetime; callers receive no mutable process-global authority.
pub trait RuntimeDriverRegistry: Send + Sync {
    type Driver: RuntimeDriver + ?Sized;

    fn driver(&self, agent_id: &str) -> Option<&Self::Driver>;
}

/// Persistent-turn operations shared by desktop reconnect and mobile resume.
/// Conversation state and replay cursors are supplied by the durable store;
/// an implementation may cache them, but the cache is never authoritative.
pub trait PersistentTurnRuntime: Send + Sync {
    type Error: std::error::Error + Send + Sync + 'static;

    fn open_or_resume(&self, request: &Value) -> Result<Value, Self::Error>;
    fn send(&self, request: &Value) -> Result<Value, Self::Error>;
    fn attach(&self, request: &Value) -> Result<Value, Self::Error>;
    fn steer(&self, request: &Value) -> Result<Value, Self::Error>;
    fn cancel(&self, request: &Value) -> Result<Value, Self::Error>;
    fn cleanup(&self, request: &Value) -> Result<Value, Self::Error>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fmt;

    #[test]
    fn execution_admission_uses_only_exact_installed_message_send_route() {
        let provider = ProviderId::parse("cursor").unwrap();
        let admitted = reduce_execution_admission(
            &provider,
            &ExecutionAdmissionEvidence {
                provider_id: "cursor".into(),
                installed: true,
                executable_message_send_route: true,
            },
        );
        assert!(admitted.admitted);
        assert_eq!(admitted.blocker_code, None);

        for (evidence, blocker) in [
            (
                ExecutionAdmissionEvidence {
                    provider_id: "codex".into(),
                    installed: true,
                    executable_message_send_route: true,
                },
                "provider_identity_mismatch",
            ),
            (
                ExecutionAdmissionEvidence {
                    provider_id: "cursor".into(),
                    installed: false,
                    executable_message_send_route: true,
                },
                "provider_not_installed",
            ),
            (
                ExecutionAdmissionEvidence {
                    provider_id: "cursor".into(),
                    installed: true,
                    executable_message_send_route: false,
                },
                "provider_execution_route_unavailable",
            ),
        ] {
            let decision = reduce_execution_admission(&provider, &evidence);
            assert!(!decision.admitted);
            assert_eq!(decision.blocker_code, Some(blocker));
        }
    }

    #[derive(Debug)]
    struct FixtureError;

    impl fmt::Display for FixtureError {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("fixture")
        }
    }

    impl std::error::Error for FixtureError {}

    struct FixtureDriver;

    impl RuntimeDriver for FixtureDriver {
        type Error = FixtureError;

        fn id(&self) -> &'static str {
            "fixture"
        }

        fn capabilities(&self) -> Value {
            serde_json::json!({"streaming": true})
        }

        fn dispatch(&self, operation: &str, request: &Value) -> Result<Value, Self::Error> {
            Ok(serde_json::json!({"operation": operation, "request": request}))
        }
    }

    #[test]
    fn driver_contract_is_independent_of_host_process_types() {
        let driver = FixtureDriver;
        assert_eq!(driver.id(), "fixture");
        assert_eq!(driver.capabilities()["streaming"], true);
        assert_eq!(
            driver
                .dispatch("send", &serde_json::json!({"text": "hello"}))
                .unwrap()["operation"],
            "send"
        );
    }
}
