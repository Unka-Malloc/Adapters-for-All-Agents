//! Agent process environment policy.
//!
//! [`EnvPolicy::Sanitized`] is the default: a spec materializes an
//! allowlist-only environment and adopting it never exposes host
//! credentials. [`EnvPolicy::InheritAll`] — the user's real environment as
//! the source of truth — exists only through a host's explicit opt-in, and
//! [`INHERITED_ENV`] defines the inert host variable names an untrusted
//! sidecar caller may reference by name over the wire. Explicit `set`
//! entries layer resolved overrides (`Literal`, `HostEnv`, or
//! host-resolved `Secret`) on top of either base.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::ffi::OsString;

/// Default inert variables a sidecar client may reference by name over the wire
/// (or a [`EnvPolicy::Sanitized`] policy inherits by default): process
/// essentials, locale, and temp locations — never credentials.
pub const INHERITED_ENV: &[&str] = &[
    "PATH",
    "PATHEXT",
    "SYSTEMROOT",
    "WINDIR",
    "ComSpec",
    "TMPDIR",
    "TMP",
    "TEMP",
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
    "HOME",
    "USERPROFILE",
    "USER",
    "LOGNAME",
    "SHELL",
    "XDG_RUNTIME_DIR",
    "TERM_PROGRAM",
];

/// One environment entry in a spec.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum EnvValue {
    /// Plain literal value.
    Literal(String),
    /// Indirections: `{"hostEnv": "NAME"}` reads a host variable at spawn
    /// time; `{"secret": "ref"}` asks the host's [`SecretResolver`]. A missing
    /// host variable or unresolved secret fails the spawn closed.
    Source(EnvSource),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub enum EnvSource {
    HostEnv { host_env: String },
    Secret { secret: String },
}

/// Environment policy for an agent process.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub enum EnvPolicy {
    /// Explicit allowlist inheritance plus `set` entries.
    Sanitized {
        /// Additional host variables to inherit beyond [`INHERITED_ENV`].
        #[serde(default)]
        inherit: Vec<String>,
        /// Explicit entries, resolved at spawn.
        #[serde(default)]
        set: BTreeMap<String, EnvValue>,
    },
    /// Full host-environment inheritance — the fidelity path host-declared
    /// cards and trusted local custody opt into explicitly.
    InheritAll {
        #[serde(default)]
        set: BTreeMap<String, EnvValue>,
    },
}

impl Default for EnvPolicy {
    /// An unspecified policy sanitizes: full host inheritance is an
    /// explicit declaration a host makes per spec — cards and trusted
    /// local custody paths write `InheritAll` deliberately, never
    /// implicitly.
    fn default() -> Self {
        Self::Sanitized {
            inherit: Vec::new(),
            set: BTreeMap::new(),
        }
    }
}

/// Temp-directory variable names a verbatim inherit may carry.
const TEMP_DIR_VARS: &[&str] = &["TMPDIR", "TMP", "TEMP"];

/// A `/dev/fd/N` (or `/proc/.../fd/N`) spelling names a descriptor inside the
/// *parent's* fd table, not a traversable directory. Runtimes that open
/// `$TMPDIR/child` fail on platforms without fd-relative path resolution
/// (e.g. Darwin fdescfs), and a runtime that happens to hold the same fd
/// number resolves it against an unrelated object. Mechanical inheritance
/// drops the spelling instead of propagating a value the child cannot use;
/// a host that genuinely means it declares an explicit `set` entry.
fn inheritable_temp_dir(key: &str, value: &OsString) -> bool {
    if !TEMP_DIR_VARS.contains(&key) {
        return true;
    }
    let value = value.to_string_lossy();
    !(value.starts_with("/dev/fd/") || value.starts_with("/proc/self/fd/"))
}

/// Variables that bind a spawned process's lifetime to a foreign host pid —
/// IDE integrations export their extension-host pid so processes *they*
/// spawned can shut down when the host dies. A custody-spawned agent is not
/// that host's child: inheriting the binding lets an unrelated editor
/// restart kill every live session. Mechanical inheritance drops the
/// binding like descriptor TMPDIR spellings; a host that genuinely means it
/// declares an explicit `set` entry.
fn inheritable_host_binding(key: &str) -> bool {
    !key.ends_with("_EXT_HOST_PID")
}

/// Resolves `{"secret": "ref"}` environment sources. Implemented by the host
/// (keychain, vault, encrypted store); the adapter never reads secret stores
/// itself.
#[async_trait::async_trait]
pub trait SecretResolver: Send + Sync {
    async fn resolve(&self, reference: &str) -> Option<String>;
}

/// A resolver that resolves nothing — the fail-closed default.
pub struct NoSecrets;

#[async_trait::async_trait]
impl SecretResolver for NoSecrets {
    async fn resolve(&self, _reference: &str) -> Option<String> {
        None
    }
}

/// Resolve a policy into concrete child-process variables.
pub async fn resolve(
    policy: &EnvPolicy,
    extra: &BTreeMap<String, String>,
    secrets: &dyn SecretResolver,
) -> crate::Result<BTreeMap<String, OsString>> {
    let mut resolved: BTreeMap<String, OsString> = BTreeMap::new();
    match policy {
        EnvPolicy::Sanitized { inherit, set } => {
            for key in INHERITED_ENV
                .iter()
                .copied()
                .chain(inherit.iter().map(String::as_str))
            {
                if let Some(value) = std::env::var_os(key)
                    .filter(|v| !v.is_empty())
                    .filter(|v| inheritable_temp_dir(key, v))
                {
                    resolved.insert(key.to_owned(), value);
                }
            }
            resolved.insert("TERM".into(), OsString::from("dumb"));
            resolved.insert("NO_COLOR".into(), OsString::from("1"));
            apply_set(&mut resolved, set, secrets).await?;
        }
        EnvPolicy::InheritAll { set } => {
            for (key, value) in std::env::vars_os() {
                let key = key.into_string().unwrap_or_default();
                if inheritable_temp_dir(&key, &value) && inheritable_host_binding(&key) {
                    resolved.insert(key, value);
                }
            }
            apply_set(&mut resolved, set, secrets).await?;
        }
    }
    for (key, value) in extra {
        resolved.insert(key.clone(), OsString::from(value));
    }
    Ok(resolved)
}

async fn apply_set(
    resolved: &mut BTreeMap<String, OsString>,
    set: &BTreeMap<String, EnvValue>,
    secrets: &dyn SecretResolver,
) -> crate::Result<()> {
    for (name, value) in set {
        let value = match value {
            EnvValue::Literal(v) => Some(v.clone()),
            EnvValue::Source(EnvSource::HostEnv { host_env }) => std::env::var(host_env).ok(),
            EnvValue::Source(EnvSource::Secret { secret }) => secrets.resolve(secret).await,
        }
        .ok_or_else(|| {
            crate::error::AdapterError::permanent("agent_env_source_unavailable", "env/resolve")
                .with_detail(format!("environment entry `{name}` could not be resolved"))
        })?;
        resolved.insert(name.clone(), OsString::from(value));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Resolver(Option<String>);
    #[async_trait::async_trait]
    impl SecretResolver for Resolver {
        async fn resolve(&self, _reference: &str) -> Option<String> {
            self.0.clone()
        }
    }

    fn spec_env(test_name: &str, values: &[(&str, &str)]) -> bool {
        // SAFETY: environment overrides apply only to an isolated test process.
        if std::env::var("A4AA_ENV_TEST_CHILD").ok().as_deref() == Some(test_name) {
            return false;
        }
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", test_name, "--test-threads=1"])
            .env("A4AA_ENV_TEST_CHILD", test_name)
            .envs(values.iter().copied())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "isolated environment test failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed; 0 failed"));
        true
    }

    #[tokio::test]
    async fn default_policy_excludes_unlisted_credential_markers() {
        if spec_env(
            "env::tests::default_policy_excludes_unlisted_credential_markers",
            &[("A4AA_TEST_INHERIT_MARKER", "hidden")],
        ) {
            return;
        }
        let resolved = resolve(&EnvPolicy::default(), &BTreeMap::new(), &NoSecrets)
            .await
            .unwrap();
        assert!(!resolved.contains_key("A4AA_TEST_INHERIT_MARKER"));
        assert!(resolved.contains_key("PATH") || resolved.contains_key("HOME"));
    }

    #[tokio::test]
    async fn sanitized_never_inherits_unlisted_host_variables() {
        if spec_env(
            "env::tests::sanitized_never_inherits_unlisted_host_variables",
            &[("A4AA_TEST_SECRET_MARKER", "leak-check")],
        ) {
            return;
        }
        let policy = EnvPolicy::Sanitized {
            inherit: Vec::new(),
            set: BTreeMap::new(),
        };
        let resolved = resolve(&policy, &BTreeMap::new(), &NoSecrets)
            .await
            .unwrap();
        assert!(!resolved.contains_key("A4AA_TEST_SECRET_MARKER"));
    }

    #[tokio::test]
    async fn inherit_all_drops_foreign_host_pid_binding() {
        if spec_env(
            "env::tests::inherit_all_drops_foreign_host_pid_binding",
            &[
                ("WINDSURF_EXT_HOST_PID", "4242"),
                ("A4AA_TEST_HOST_MARKER", "kept"),
            ],
        ) {
            return;
        }
        let resolved = resolve(
            &EnvPolicy::InheritAll {
                set: BTreeMap::new(),
            },
            &BTreeMap::new(),
            &NoSecrets,
        )
        .await
        .unwrap();
        assert!(!resolved.contains_key("WINDSURF_EXT_HOST_PID"));
        assert_eq!(resolved.get("A4AA_TEST_HOST_MARKER").unwrap(), "kept");
    }

    #[tokio::test]
    async fn explicit_set_can_still_declare_host_pid_binding() {
        if spec_env(
            "env::tests::explicit_set_can_still_declare_host_pid_binding",
            &[("WINDSURF_EXT_HOST_PID", "4242")],
        ) {
            return;
        }
        let mut set = BTreeMap::new();
        set.insert(
            "WINDSURF_EXT_HOST_PID".to_owned(),
            EnvValue::Literal("4242".into()),
        );
        let resolved = resolve(&EnvPolicy::InheritAll { set }, &BTreeMap::new(), &NoSecrets)
            .await
            .unwrap();
        assert_eq!(resolved.get("WINDSURF_EXT_HOST_PID").unwrap(), "4242");
    }

    #[tokio::test]
    async fn descriptor_temp_spelling_is_not_inherited() {
        if spec_env(
            "env::tests::descriptor_temp_spelling_is_not_inherited",
            &[
                ("TMPDIR", "/dev/fd/300"),
                ("A4AA_TEST_TMPDIR_SENTINEL", "/var/tmp/x"),
            ],
        ) {
            return;
        }
        let resolved = resolve(&EnvPolicy::default(), &BTreeMap::new(), &NoSecrets)
            .await
            .unwrap();
        assert!(!resolved.contains_key("TMPDIR"));
    }

    #[tokio::test]
    async fn explicit_set_still_injects_literal_tmpdir() {
        if spec_env(
            "env::tests::explicit_set_still_injects_literal_tmpdir",
            &[("TMPDIR", "/dev/fd/300")],
        ) {
            return;
        }
        let mut set = BTreeMap::new();
        set.insert("TMPDIR".to_owned(), EnvValue::Literal("/dev/fd/300".into()));
        let policy = EnvPolicy::Sanitized {
            inherit: Vec::new(),
            set,
        };
        let resolved = resolve(&policy, &BTreeMap::new(), &NoSecrets)
            .await
            .unwrap();
        assert_eq!(resolved.get("TMPDIR").unwrap(), "/dev/fd/300");
    }

    #[tokio::test]
    async fn unresolved_secret_fails_closed() {
        let mut set = BTreeMap::new();
        set.insert(
            "AGENT_TOKEN".to_owned(),
            EnvValue::Source(EnvSource::Secret {
                secret: "missing-ref".into(),
            }),
        );
        let policy = EnvPolicy::Sanitized {
            inherit: Vec::new(),
            set,
        };
        let error = resolve(&policy, &BTreeMap::new(), &NoSecrets)
            .await
            .unwrap_err();
        assert_eq!(error.code, "agent_env_source_unavailable");
    }

    #[tokio::test]
    async fn resolved_secret_is_injected() {
        let mut set = BTreeMap::new();
        set.insert(
            "AGENT_TOKEN".to_owned(),
            EnvValue::Source(EnvSource::Secret {
                secret: "token-ref".into(),
            }),
        );
        let policy = EnvPolicy::Sanitized {
            inherit: Vec::new(),
            set,
        };
        let resolved = resolve(&policy, &BTreeMap::new(), &Resolver(Some("s3cr3t".into())))
            .await
            .unwrap();
        assert_eq!(resolved.get("AGENT_TOKEN").unwrap(), "s3cr3t");
    }
}
