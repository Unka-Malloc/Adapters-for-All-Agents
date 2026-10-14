//! Generic JSON-RPC lane for stdio/RPC agents.
//!
//! Hosts declare the agent's method surface as templates (`{prompt}`,
//! `{sessionId}`, `{model}`, `{effort}`, `{cwd}` are substituted into string
//! leaves of the params value). Inbound notifications map onto
//! [`AgentEvent`]: the configured `textEvents` method carries text, any other
//! notification arrives raw — nothing is dropped.
//!
//! Turn semantics: `session/open` (if configured) runs once at open;
//! `prompt` is awaited per turn; `cancel` (if configured) is sent as a
//! notification on explicit cancel only.

use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::Arc;
use tokio::sync::Mutex;

use crate::bounds;
use crate::env::SecretResolver;
use crate::error::AdapterError;
use crate::model::RpcCallSpec;
use crate::model::{
    AgentEvent, Capabilities, CommandSpec, RpcLaneSpec, SessionRequest, TurnOutcome,
};
use crate::observe::SharedObserver;
use crate::rpc::{CallFailure, JsonRpcPeer, RpcDelegate, RpcErrorObject};
use crate::supervision::SupervisedChild;
use crate::transport::stdio;

/// Host callbacks for an RPC session.
#[async_trait::async_trait]
pub trait RpcSessionDelegate: Send + Sync {
    async fn on_event(&self, _event: AgentEvent) {}
    /// Agent→host requests. Default: `method not found` — fail closed.
    async fn on_request(&self, method: &str, _params: Value) -> Result<Value, RpcErrorObject> {
        Err(RpcErrorObject::method_not_found(method))
    }
}

pub struct RpcLane {
    pub command: CommandSpec,
    pub spec: RpcLaneSpec,
    pub secrets: Arc<dyn SecretResolver>,
    pub observer: SharedObserver,
}

/// One open RPC session: child process + peer + negotiated session id.
pub struct RpcSession {
    peer: Arc<JsonRpcPeer>,
    child: Mutex<SupervisedChild>,
    spec: RpcLaneSpec,
    session_id: Option<String>,
    cwd: String,
    /// Serializes prompt turns.
    turn_lock: Mutex<()>,
}

impl RpcLane {
    pub async fn open(
        &self,
        request: SessionRequest,
        delegate: Arc<dyn RpcSessionDelegate>,
    ) -> Result<RpcSession, AdapterError> {
        let env = crate::env::resolve(&self.command.env, &request.env, &*self.secrets).await?;
        let mut command = SupervisedChild::command(&self.command, &env, request.cwd.as_deref());
        let mut child = SupervisedChild::spawn(&mut command, stdio::PROTOCOL_IO)?;
        let (stdin, stdout, stderr) = stdio::protocol_halves(&mut child)?;
        if let Some(stderr) = stderr {
            tokio::spawn(bounds::drain_stderr(stderr, self.observer.clone()));
        }
        let text_method = self.spec.text_event_method.clone();
        let observer = self.observer.clone();
        struct Inner {
            delegate: Arc<dyn RpcSessionDelegate>,
            text_method: Option<String>,
        }
        #[async_trait::async_trait]
        impl RpcDelegate for Inner {
            async fn on_request(
                &self,
                method: &str,
                params: Value,
            ) -> Result<Value, RpcErrorObject> {
                self.delegate.on_request(method, params).await
            }
            async fn on_notification(&self, method: &str, params: Value) {
                let event = if Some(method) == self.text_method.as_deref() {
                    let text = params
                        .get("text")
                        .or_else(|| params.get("content"))
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                        .or_else(|| params.as_str().map(str::to_owned));
                    text.map(|text| AgentEvent::MessageChunk { text })
                } else {
                    None
                };
                self.delegate
                    .on_event(event.unwrap_or(AgentEvent::Raw {
                        raw: serde_json::json!({"method": method, "params": params}),
                    }))
                    .await;
            }
        }
        let peer = JsonRpcPeer::spawn(
            stdout,
            stdin,
            Arc::new(Inner {
                delegate,
                text_method,
            }),
            observer,
            crate::framing::DEFAULT_MAX_MESSAGE_BYTES,
        );

        let cwd = request
            .cwd
            .clone()
            .or_else(|| self.command.cwd.clone())
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| std::env::temp_dir().display().to_string());

        let mut session_id = None;
        if let Some(open) = &self.spec.session_open {
            let result = call(&peer, open, &vars(None, &cwd, "", None, None)).await?;
            session_id = result
                .get("sessionId")
                .or_else(|| result.get("session_id"))
                .or_else(|| result.get("id"))
                .and_then(Value::as_str)
                .map(str::to_owned);
        }

        Ok(RpcSession {
            peer,
            child: Mutex::new(child),
            spec: self.spec.clone(),
            session_id,
            cwd,
            turn_lock: Mutex::new(()),
        })
    }
}

impl RpcSession {
    pub fn session_id(&self) -> Option<&str> {
        self.session_id.as_deref()
    }

    pub fn capabilities(&self) -> Capabilities {
        self.spec.capabilities()
    }

    /// Run one prompt turn. Serialized per session.
    pub async fn prompt(
        &self,
        text: &str,
        model: Option<&str>,
        effort: Option<&str>,
    ) -> Result<TurnOutcome, AdapterError> {
        let _guard = self.turn_lock.lock().await;
        let vars = vars(self.session_id.as_deref(), &self.cwd, text, model, effort);
        let result = call(&self.peer, &self.spec.prompt, &vars).await?;
        let stop_reason = ["stopReason", "stop_reason", "status"]
            .iter()
            .find_map(|key| result.get(*key).and_then(Value::as_str))
            .unwrap_or("end_turn")
            .to_owned();
        Ok(TurnOutcome {
            stop_reason,
            text: extract_text(&result, self.spec.result_text_path.as_deref())
                .unwrap_or_else(|| result.to_string()),
            truncated: false,
            exit_code: None,
        })
    }

    /// Explicit user cancellation: sends the configured notification.
    pub fn cancel(&self) -> Result<(), AdapterError> {
        let Some(cancel) = &self.spec.cancel else {
            return Err(AdapterError::permanent(
                "agent_cancel_unsupported",
                "session/cancel",
            ));
        };
        let vars = vars(self.session_id.as_deref(), &self.cwd, "", None, None);
        let params = substitute(&cancel.params, &vars);
        self.peer.notify(&cancel.method, params).map_err(|error| {
            AdapterError::uncertain("agent_cancel_failed", "session/cancel")
                .with_detail(format!("{error:?}"))
        })
    }

    /// Invoke a declared passthrough method verbatim.
    pub async fn call_passthrough(
        &self,
        method: &str,
        params: Value,
    ) -> Result<Value, AdapterError> {
        if !self.spec.passthrough.iter().any(|entry| entry == method) {
            return Err(AdapterError::permanent(
                "agent_method_not_admitted",
                "rpc/call",
            ));
        }
        self.peer.call(method, params).await.map_err(call_failure)
    }

    pub async fn close(self) -> Result<(), AdapterError> {
        if let Some(close) = &self.spec.session_close {
            let vars = vars(self.session_id.as_deref(), &self.cwd, "", None, None);
            let params = substitute(&close.params, &vars);
            let _ = self.peer.call(&close.method, params).await;
        }
        self.peer.close().await;
        let mut child = self.child.lock().await;
        let _ = child.terminate().await;
        Ok(())
    }
}

type Vars = BTreeMap<String, String>;

fn vars(
    session_id: Option<&str>,
    cwd: &str,
    prompt: &str,
    model: Option<&str>,
    effort: Option<&str>,
) -> Vars {
    let mut vars = BTreeMap::new();
    vars.insert("cwd".to_owned(), cwd.to_owned());
    vars.insert("prompt".to_owned(), prompt.to_owned());
    // Same convention as the cli lane: an unset placeholder substitutes as
    // the empty string, never leaks a literal `{model}` onto the wire.
    vars.insert("model".to_owned(), model.unwrap_or_default().to_owned());
    vars.insert("effort".to_owned(), effort.unwrap_or_default().to_owned());
    if let Some(session) = session_id {
        vars.insert("sessionId".to_owned(), session.to_owned());
    }
    vars
}

/// Recursively substitute `{var}` inside string leaves of a params template.
fn substitute(template: &Value, vars: &Vars) -> Value {
    match template {
        Value::String(text) => {
            let mut result = text.clone();
            for (key, value) in vars {
                result = result.replace(&format!("{{{key}}}"), value);
            }
            Value::String(result)
        }
        Value::Array(items) => {
            Value::Array(items.iter().map(|item| substitute(item, vars)).collect())
        }
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(key, value)| (key.clone(), substitute(value, vars)))
                .collect(),
        ),
        other => other.clone(),
    }
}

async fn call(peer: &JsonRpcPeer, spec: &RpcCallSpec, vars: &Vars) -> Result<Value, AdapterError> {
    if spec.method.is_empty() {
        return Err(AdapterError::permanent("agent_method_unset", "rpc/call"));
    }
    peer.call(&spec.method, substitute(&spec.params, vars))
        .await
        .map_err(call_failure)
}

fn call_failure(failure: CallFailure) -> AdapterError {
    match failure {
        CallFailure::Remote(error) => AdapterError::permanent("agent_remote_error", "rpc/call")
            .with_detail(format!("{} ({})", error.message, error.code)),
        CallFailure::Transport(reason) => {
            AdapterError::uncertain("agent_transport_failed", "rpc/call").with_detail(reason)
        }
    }
}

fn extract_text(result: &Value, path: Option<&str>) -> Option<String> {
    let mut cursor = result;
    for segment in path?.split('.') {
        cursor = cursor.get(segment)?;
    }
    cursor.as_str().map(str::to_owned)
}
