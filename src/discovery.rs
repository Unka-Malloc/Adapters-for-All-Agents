//! Allowlisted agent discovery on the host machine.
//!
//! Cards declare where an agent lives (`detect`, `history`); a host that scans
//! for installed agents feeds those declarations — or its own broader
//! manifest — through this engine. [`ScanManifest::from_cards`] projects the
//! published cards directly, so the card stays the single data authority and
//! no parallel per-agent catalog is needed; a host with agents beyond the
//! cards (history-only formats, its own first-party agent) supplies a
//! [`ScanManifest`] document of its own in the same shape.
//!
//! The manifest is the only automatic search space: PATH, personal library
//! roots and network volumes are never walked. Classification is lexical;
//! a location the manifest's deny spec excludes is never stated, and the
//! probe helpers refuse locations a symlink chain would steer into a denied
//! one. Discovery never authorizes automatic execution: a discovered binary
//! may run only when the caller explicitly opted into a probe and the path
//! is not a denied personal or network location.

use crate::card::AgentCard;
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::env;
use std::ffi::OsString;
use std::fs;
use std::path::{Component, Path, PathBuf};

/// The host roots a `{token}` path template resolves against.
///
/// The standard tokens (`{home}`, `{appdata}`, `{local_appdata}`,
/// `{xdg_config}`, `{xdg_data}`, `{program_data}`, `{program_files}`,
/// `{program_files_x86}`) map to the named fields; [`HostRoots::extra`]
/// carries host-declared tokens beyond the standard set (a portable data
/// root, for example), so a manifest's template vocabulary stays open
/// without changing this type.
#[derive(Clone, Debug, Default)]
pub struct HostRoots {
    pub home: Option<PathBuf>,
    pub appdata: Option<PathBuf>,
    pub local_appdata: Option<PathBuf>,
    pub xdg_config: Option<PathBuf>,
    pub xdg_data: Option<PathBuf>,
    pub program_data: Option<PathBuf>,
    pub program_files: Option<PathBuf>,
    pub program_files_x86: Option<PathBuf>,
    /// Host-declared root tokens a manifest may use beyond the standard set.
    pub extra: BTreeMap<String, PathBuf>,
}

impl HostRoots {
    /// The roots this machine's environment declares.
    pub fn from_environment() -> Self {
        let home = user_home_from_env();
        Self {
            appdata: env_path("APPDATA").or_else(|| {
                home.as_ref().map(|home| {
                    if cfg!(windows) {
                        home.join("AppData").join("Roaming")
                    } else {
                        home.join(".config")
                    }
                })
            }),
            local_appdata: env_path("LOCALAPPDATA").or_else(|| {
                home.as_ref().map(|home| {
                    if cfg!(windows) {
                        home.join("AppData").join("Local")
                    } else {
                        home.join(".local").join("share")
                    }
                })
            }),
            xdg_config: env_path("XDG_CONFIG_HOME")
                .or_else(|| home.as_ref().map(|home| home.join(".config"))),
            xdg_data: env_path("XDG_DATA_HOME")
                .or_else(|| home.as_ref().map(|home| home.join(".local").join("share"))),
            program_data: env_path("ProgramData"),
            program_files: env_path("ProgramFiles"),
            program_files_x86: env_path("ProgramFiles(x86)"),
            home,
            extra: BTreeMap::new(),
        }
    }

    /// The standard roots derived from one home directory, for scans that
    /// run against a stated home rather than the live environment.
    pub fn from_home(home: &Path) -> Self {
        Self {
            home: Some(home.to_path_buf()),
            appdata: Some(if cfg!(windows) {
                home.join("AppData").join("Roaming")
            } else {
                home.join(".config")
            }),
            local_appdata: Some(if cfg!(windows) {
                home.join("AppData").join("Local")
            } else {
                home.join(".local").join("share")
            }),
            xdg_config: Some(home.join(".config")),
            xdg_data: Some(home.join(".local").join("share")),
            program_data: None,
            program_files: None,
            program_files_x86: None,
            extra: BTreeMap::new(),
        }
    }
}

/// The deny spec every scan honors: locations automatic discovery never
/// states. A manifest may declare its own; [`DenySpec::standard`] is the
/// floor a projected or sparse manifest gets.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
pub struct DenySpec {
    /// Absolute prefixes naming network volumes — `/Volumes` on macOS.
    #[serde(default)]
    pub network_prefixes: Vec<String>,
    /// Directory names directly under the user home that hold personal
    /// content — Desktop, Documents, Downloads, Pictures, Music, Movies,
    /// Videos, Public.
    #[serde(default)]
    pub home_roots: Vec<String>,
    /// Bundled media-library extensions that are never scan targets.
    #[serde(default)]
    pub media_library_extensions: Vec<String>,
    /// Home-relative prefixes belonging to other applications' containers;
    /// automatic probes skip them, an explicitly selected agent may still
    /// read its own store inside one.
    #[serde(default)]
    pub other_app_home_prefixes: Vec<String>,
}

impl DenySpec {
    /// The conservative floor: network volumes, the personal document roots,
    /// bundled media libraries and the other-app container prefixes.
    pub fn standard() -> Self {
        Self {
            network_prefixes: vec!["/Volumes".to_string()],
            home_roots: [
                "Desktop",
                "Documents",
                "Downloads",
                "Pictures",
                "Music",
                "Movies",
                "Videos",
                "Public",
            ]
            .iter()
            .map(|name| (*name).to_string())
            .collect(),
            media_library_extensions: [
                "photoslibrary",
                "photolibrary",
                "imovielibrary",
                "tvlibrary",
                "theater",
            ]
            .iter()
            .map(|name| (*name).to_string())
            .collect(),
            other_app_home_prefixes: [
                "Library/Application Support",
                "Library/Logs",
                "Library/Containers",
                "Library/Group Containers",
                "Library/Mobile Documents",
                "Library/CloudStorage",
            ]
            .iter()
            .map(|name| (*name).to_string())
            .collect(),
        }
    }
}

/// One OS-filtered group of shared binary search directories.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct BinaryDirGroup {
    #[serde(default)]
    pub oses: Vec<String>,
    #[serde(default)]
    pub dirs: Vec<String>,
}

/// A `{token}`-rooted path template valid only on the listed platforms; an
/// empty `oses` means all.
#[derive(Clone, Debug, Deserialize)]
pub struct PathEntry {
    pub path: String,
    #[serde(default)]
    pub oses: Vec<String>,
}

/// One persisted history/store location an agent owns.
#[derive(Clone, Debug, Deserialize)]
pub struct HistoryEntry {
    pub path: String,
    #[serde(default)]
    pub kind: String,
}

/// The locations one agent occupies on a host.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct AgentScanPaths {
    pub id: String,
    /// Executable names to resolve inside the binary search dirs.
    #[serde(default)]
    pub binary_names: Vec<String>,
    /// Process image names observed while the agent runs.
    #[serde(default)]
    pub process_names: Vec<String>,
    /// Directories hosting a bundled executable outside PATH.
    #[serde(default)]
    pub binaries: Vec<PathEntry>,
    /// Desktop application executables that bundle this agent.
    #[serde(default)]
    pub apps: Vec<PathEntry>,
    /// Editor/extension directories that may bundle this agent.
    #[serde(default)]
    pub extension_roots: Vec<PathEntry>,
    /// Configuration files whose presence marks the agent configured.
    #[serde(default)]
    pub config: Vec<PathEntry>,
    /// Marker paths proving prior use.
    #[serde(default)]
    pub detection: Vec<PathEntry>,
    /// Persisted history/store locations.
    #[serde(default)]
    pub history: Vec<HistoryEntry>,
}

/// The complete scan space one host declares: the deny spec, the shared
/// binary dirs and every agent's locations.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct ScanManifest {
    /// The document's self-declared schema version; a projected manifest
    /// carries none.
    #[serde(default)]
    pub schema_version: String,
    /// Locations automatic discovery never states; absent, the standard
    /// floor applies.
    #[serde(default = "DenySpec::standard")]
    pub deny: DenySpec,
    #[serde(default)]
    pub binaries: Vec<BinaryDirGroup>,
    #[serde(default)]
    pub agents: Vec<AgentScanPaths>,
}

/// One history root expanded to a concrete path.
#[derive(Clone, Debug)]
pub struct HistoryScanRoot {
    pub path: PathBuf,
    pub kind: String,
}

impl ScanManifest {
    /// Project the published cards into a scan manifest: each card's
    /// `detect` and `history` surface becomes its agent's locations, so a
    /// host that scans carded agents needs no second catalog.
    ///
    /// The shared binary-dir groups stay empty — PATH-order resolution is
    /// the host's (`hostenv::search_path_dirs`); the deny spec is the
    /// standard floor.
    pub fn from_cards(cards: &[&'static AgentCard]) -> Self {
        let entry = |path: &'static str, oses: &'static [&'static str]| PathEntry {
            path: path.to_string(),
            oses: oses.iter().map(|os| (*os).to_string()).collect(),
        };
        Self {
            schema_version: String::new(),
            deny: DenySpec::standard(),
            binaries: Vec::new(),
            agents: cards
                .iter()
                .map(|card| AgentScanPaths {
                    id: card.id.to_string(),
                    binary_names: card
                        .detect
                        .binaries
                        .iter()
                        .map(|name| (*name).to_string())
                        .collect(),
                    process_names: card
                        .detect
                        .processes
                        .iter()
                        .map(|name| (*name).to_string())
                        .collect(),
                    binaries: card
                        .detect
                        .binary_dirs
                        .iter()
                        .map(|dir| entry(dir.path, dir.oses))
                        .collect(),
                    apps: card
                        .detect
                        .apps
                        .iter()
                        .map(|app| entry(app.path, app.oses))
                        .collect(),
                    extension_roots: Vec::new(),
                    config: card
                        .detect
                        .config
                        .iter()
                        .map(|path| entry(path, &[]))
                        .collect(),
                    detection: card
                        .detect
                        .detection
                        .iter()
                        .map(|path| entry(path, &[]))
                        .collect(),
                    history: card
                        .history
                        .iter()
                        .map(|root| HistoryEntry {
                            path: root.path.to_string(),
                            kind: root.kind.to_string(),
                        })
                        .collect(),
                })
                .collect(),
        }
    }

    fn agent(&self, id: &str) -> Option<&AgentScanPaths> {
        self.agents.iter().find(|agent| agent.id == id)
    }

    /// The binary search dirs the manifest declares for `os`: shared groups
    /// plus every agent's bundled-binary dirs, minus denied locations.
    /// [`ScanManifest::binary_dirs`] adds the nvm default alias on top.
    pub fn declared_binary_dirs(&self, os: &str, roots: &HostRoots) -> Vec<PathBuf> {
        let mut dirs = Vec::new();
        for group in &self.binaries {
            if !os_matches(&group.oses, os) {
                continue;
            }
            dirs.extend(expand_all(&group.dirs, roots));
        }
        for agent in &self.agents {
            dirs.extend(expand_os_paths(&agent.binaries, os, roots));
        }
        dedupe(dirs)
            .into_iter()
            .filter(|path| !self.denied(path, roots.home.as_deref()))
            .collect()
    }

    /// The nvm default Node bin dir under `home`: the `default` alias chain
    /// is resolved in code so discovery never lists every installed Node
    /// version, and a hostile alias cannot redirect the scan outside the
    /// allowlist.
    pub fn nvm_default_bin_dirs(&self, home: &Path) -> Vec<PathBuf> {
        nvm_default_version(self, home)
            .map(|version| {
                home.join(".nvm")
                    .join("versions")
                    .join("node")
                    .join(version)
                    .join("bin")
            })
            .into_iter()
            .collect()
    }

    /// The full binary search space for `os`: the declared dirs plus the
    /// nvm default alias.
    pub fn binary_dirs(&self, os: &str, roots: &HostRoots) -> Vec<PathBuf> {
        let mut dirs = self.declared_binary_dirs(os, roots);
        if let Some(home) = roots.home.as_deref() {
            dirs.extend(self.nvm_default_bin_dirs(home));
        }
        dedupe(dirs)
            .into_iter()
            .filter(|path| !self.denied(path, roots.home.as_deref()))
            .collect()
    }

    /// One agent's bundled-binary dirs for `os`.
    pub fn agent_binary_dirs(&self, agent_id: &str, os: &str, roots: &HostRoots) -> Vec<PathBuf> {
        self.agent(agent_id)
            .map(|agent| expand_os_paths(&agent.binaries, os, roots))
            .unwrap_or_default()
    }

    /// The first declared config path for one agent on `os`.
    pub fn config_path(&self, agent_id: &str, os: &str, roots: &HostRoots) -> Option<PathBuf> {
        self.agent(agent_id).and_then(|agent| {
            agent.config.iter().find_map(|entry| {
                if os_matches(&entry.oses, os) {
                    expand_template(&entry.path, roots)
                } else {
                    None
                }
            })
        })
    }

    /// One agent's detection markers for `os`.
    pub fn detection_paths(&self, agent_id: &str, os: &str, roots: &HostRoots) -> Vec<PathBuf> {
        let Some(agent) = self.agent(agent_id) else {
            return Vec::new();
        };
        agent
            .detection
            .iter()
            .filter(|entry| os_matches(&entry.oses, os))
            .filter_map(|entry| expand_template(&entry.path, roots))
            .collect()
    }

    /// One agent's persisted history locations.
    pub fn history_roots(&self, agent_id: &str, roots: &HostRoots) -> Vec<HistoryScanRoot> {
        let Some(agent) = self.agent(agent_id) else {
            return Vec::new();
        };
        agent
            .history
            .iter()
            .filter_map(|entry| {
                Some(HistoryScanRoot {
                    path: expand_template(&entry.path, roots)?,
                    kind: entry.kind.clone(),
                })
            })
            .collect()
    }

    /// One agent's extension roots for `os`.
    pub fn extension_roots(&self, agent_id: &str, os: &str, roots: &HostRoots) -> Vec<PathBuf> {
        self.agent(agent_id)
            .map(|agent| expand_os_paths(&agent.extension_roots, os, roots))
            .unwrap_or_default()
    }

    /// One agent's desktop bundle executables for `os`.
    pub fn app_executables(&self, agent_id: &str, os: &str, roots: &HostRoots) -> Vec<PathBuf> {
        self.agent(agent_id)
            .map(|agent| expand_os_paths(&agent.apps, os, roots))
            .unwrap_or_default()
    }

    /// Every location the manifest covers, used as the admission allowlist:
    /// a path an expanded prefix covers is a location discovery may state.
    pub fn allow_prefixes(&self, os: &str, roots: &HostRoots) -> Vec<PathBuf> {
        let mut prefixes = Vec::new();
        for group in &self.binaries {
            if os_matches(&group.oses, os) {
                prefixes.extend(expand_all(&group.dirs, roots));
            }
        }
        for agent in &self.agents {
            prefixes.extend(expand_os_paths(&agent.binaries, os, roots));
            prefixes.extend(expand_os_paths(&agent.apps, os, roots));
            prefixes.extend(expand_os_paths(&agent.extension_roots, os, roots));
            for entry in agent
                .config
                .iter()
                .chain(agent.detection.iter())
                .filter(|entry| os_matches(&entry.oses, os))
            {
                if let Some(path) = expand_template(&entry.path, roots) {
                    prefixes.push(path);
                }
            }
            for entry in &agent.history {
                if let Some(path) = expand_template(&entry.path, roots) {
                    prefixes.push(path);
                }
            }
        }
        if let Some(home) = roots.home.as_deref() {
            prefixes.extend(self.nvm_default_bin_dirs(home));
        }
        dedupe(prefixes)
            .into_iter()
            .filter(|path| !self.denied(path, roots.home.as_deref()))
            .collect()
    }

    /// Whether discovery admits `path` on `os`: a location whose own scan
    /// prefixes cover it and that the deny spec does not exclude.
    pub fn admitted_scan_path(&self, path: &Path, os: &str, roots: &HostRoots) -> bool {
        let normalized = lexical(path);
        if self.denied(&normalized, roots.home.as_deref()) {
            return false;
        }
        self.allow_prefixes(os, roots).iter().any(|prefix| {
            let prefix = lexical(prefix);
            normalized == prefix || normalized.starts_with(&prefix)
        })
    }

    /// The full automatic-probe admission: allowlisted, not inside another
    /// app's container, and not escaping through a symlink chain.
    pub fn automatic_probe_admitted(&self, path: &Path, os: &str, roots: &HostRoots) -> bool {
        self.admitted_scan_path(path, os, roots)
            && !self.is_other_app_container(path, roots)
            && !self.symlink_escapes_denied_location(path, roots)
    }

    /// Whether an admitted path exists on disk.
    pub fn probe_exists(&self, path: &Path, os: &str, roots: &HostRoots) -> bool {
        self.automatic_probe_admitted(path, os, roots) && path.exists()
    }

    /// Whether an admitted path is a file on disk.
    pub fn probe_is_file(&self, path: &Path, os: &str, roots: &HostRoots) -> bool {
        self.automatic_probe_admitted(path, os, roots) && path.is_file()
    }

    /// Whether an admitted path is a directory on disk.
    pub fn probe_is_dir(&self, path: &Path, os: &str, roots: &HostRoots) -> bool {
        self.automatic_probe_admitted(path, os, roots) && path.is_dir()
    }

    /// Exact files constructed under a caller-supplied catalog home may be
    /// stated when they are not denied personal or network locations. This
    /// is not unused-agent discovery: the caller already named the home and
    /// the file.
    pub fn probe_exists_under_home(&self, path: &Path, home: &Path) -> bool {
        let normalized = lexical(path);
        let home = lexical(home);
        let real_home = user_home_from_env();
        if self.catalog_home_denied(&home, real_home.as_deref())
            || self.denied(&normalized, real_home.as_deref())
            || self.denied(&normalized, Some(&home))
        {
            return false;
        }
        if self.symlink_escapes_denied_location(path, &HostRoots::from_environment())
            || self.symlink_escapes_denied_location(path, &HostRoots::from_home(&home))
        {
            return false;
        }
        if !(normalized == home || normalized.starts_with(&home)) {
            return self.probe_exists(path, std::env::consts::OS, &HostRoots::from_environment());
        }
        path.exists()
    }

    /// Named files inside another app's container. Unused-agent scans never
    /// stat these; a selected agent may `exists` and read its own store.
    pub fn selected_agent_named_store_exists(
        &self,
        path: &Path,
        catalog_home: &Path,
        selected: bool,
    ) -> bool {
        let real_home = user_home_from_env();
        if self.denied(path, Some(catalog_home)) || self.denied(path, real_home.as_deref()) {
            return false;
        }
        if !selected && self.is_other_app_container(path, &HostRoots::from_home(catalog_home)) {
            return false;
        }
        path.exists()
    }

    fn catalog_home_denied(&self, home: &Path, real_home: Option<&Path>) -> bool {
        if self.denied(home, None) {
            return true;
        }
        if real_home.is_some_and(|real_home| lexical(home) != lexical(real_home))
            && self.denied(home, real_home)
        {
            return true;
        }
        home.file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| self.deny.home_roots.iter().any(|root| root == name))
    }

    /// Other-app containers from the deny spec. Lexical; does not stat.
    /// Automatic unused-agent probes skip these; a selected agent may still
    /// read its own store inside one.
    pub fn is_other_app_container(&self, path: &Path, roots: &HostRoots) -> bool {
        let Some(home) = roots
            .home
            .as_deref()
            .map(|home| strip_macos_data_volume(&lexical(home)))
        else {
            return false;
        };
        let normalized = strip_macos_data_volume(&lexical(path));
        self.deny.other_app_home_prefixes.iter().any(|relative| {
            let root = home.join(relative);
            normalized == root || normalized.starts_with(&root)
        })
    }

    /// Whether `path` lands in a denied location: a network volume, a media
    /// library, a personal document root under `home`, or `home` itself.
    pub fn denied(&self, path: &Path, home: Option<&Path>) -> bool {
        if is_windows_network_path(path) {
            return true;
        }
        let normalized = strip_macos_data_volume(&lexical(path));
        if self.deny.network_prefixes.iter().any(|prefix| {
            let prefix = Path::new(prefix);
            normalized == prefix || normalized.starts_with(prefix)
        }) {
            return true;
        }
        if is_media_library(&normalized, &self.deny.media_library_extensions) {
            return true;
        }
        let Some(home) = home.map(|home| strip_macos_data_volume(&lexical(home))) else {
            return false;
        };
        if home.starts_with(&normalized) {
            return true;
        }
        self.deny.home_roots.iter().any(|name| {
            let root = home.join(name);
            normalized == root || normalized.starts_with(&root)
        })
    }

    /// Resolve links one path component at a time. Inspecting only the leaf
    /// misses a linked parent and would let the later filesystem probe
    /// follow that parent into a denied location.
    pub fn symlink_escapes_denied_location(&self, path: &Path, roots: &HostRoots) -> bool {
        let mut pending = components_of(path);
        let mut resolved = PathBuf::new();
        let mut seen_links = BTreeSet::new();

        while let Some(component) = pending.pop_front() {
            resolved.push(component);
            let metadata = match fs::symlink_metadata(&resolved) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return false,
                Err(_) => return true,
            };
            if !metadata.file_type().is_symlink() {
                continue;
            }
            if seen_links.len() >= 40 || !seen_links.insert(lexical(&resolved)) {
                return true;
            }

            let Ok(target) = fs::read_link(&resolved) else {
                return true;
            };
            let target = if target.is_absolute() {
                lexical(&target)
            } else {
                lexical(
                    &resolved
                        .parent()
                        .map(|parent| parent.join(&target))
                        .unwrap_or(target),
                )
            };
            if self.denied(&target, roots.home.as_deref())
                || self.is_other_app_container(&target, roots)
            {
                return true;
            }

            let mut expanded = components_of(&target);
            expanded.extend(pending);
            pending = expanded;
            resolved.clear();
        }
        false
    }

    /// A user-triggered capability or model-catalog probe may execute a
    /// discovered binary only when the caller opted in and the path is not a
    /// denied personal or network location.
    pub fn discovered_agent_may_execute(&self, path: &Path, execution_requested: bool) -> bool {
        execution_requested
            && !automatic_agent_execution_admitted()
            && !self.denied(path, user_home_from_env().as_deref())
    }

    /// A file probe unconstrained by the allowlist — used by callers that
    /// already named the exact file (a user-selected path). Symlinks are
    /// rejected so the selected identity cannot change after validation.
    pub fn automatic_path_is_file(&self, path: &Path) -> bool {
        !self.symlink_escapes_denied_location(path, &HostRoots::from_environment())
            && path.is_file()
    }

    /// The directory counterpart of [`ScanManifest::automatic_path_is_file`].
    pub fn automatic_path_is_dir(&self, path: &Path) -> bool {
        !self.symlink_escapes_denied_location(path, &HostRoots::from_environment()) && path.is_dir()
    }

    /// Resolve one of `names` to an executable file inside `dirs`, in order.
    /// Candidates in denied locations are skipped, a symlink chain escaping
    /// into one is never stated, and on Windows the PATHEXT spellings are
    /// tried for names without an extension.
    pub fn find_binary_in_dirs(&self, names: &[&str], dirs: &[PathBuf]) -> Option<PathBuf> {
        let home = user_home_from_env();
        for dir in dirs {
            for name in names {
                for candidate in binary_candidate_paths(dir, name) {
                    if self.denied(&candidate, home.as_deref()) {
                        continue;
                    }
                    if self.automatic_path_is_file(&candidate) {
                        return Some(candidate);
                    }
                }
            }
        }
        None
    }
}

/// Allowlisted paths may be stated. They may never be executed during
/// unused-agent discovery: no manifest entry can opt a binary into
/// launch-time execution.
pub fn automatic_agent_execution_admitted() -> bool {
    false
}

/// A file chosen explicitly by the user is not constrained to automatic scan
/// allowlists. Symlinks are rejected so the selected identity cannot change
/// after validation.
pub fn explicit_file_exists(path: &Path) -> bool {
    fs::symlink_metadata(path)
        .is_ok_and(|metadata| metadata.file_type().is_file() && !metadata.file_type().is_symlink())
}

/// Expand one `{token}`-rooted path template against the host roots. A
/// template without a leading `{token}` is joined to no root — it expands
/// to itself verbatim.
pub fn expand_template(template: &str, roots: &HostRoots) -> Option<PathBuf> {
    let mut remaining = template;
    let mut path = PathBuf::new();
    if let Some(token) = remaining.strip_prefix('{') {
        let (name, rest) = token.split_once('}')?;
        let base = match name {
            "home" => roots.home.as_ref()?,
            "appdata" => roots.appdata.as_ref()?,
            "local_appdata" => roots.local_appdata.as_ref()?,
            "xdg_config" => roots.xdg_config.as_ref()?,
            "xdg_data" => roots.xdg_data.as_ref()?,
            "program_data" => roots.program_data.as_ref()?,
            "program_files" => roots.program_files.as_ref()?,
            "program_files_x86" => roots.program_files_x86.as_ref()?,
            other => roots.extra.get(other)?,
        };
        path.push(base);
        remaining = rest.trim_start_matches(['/', '\\']);
    }
    if !remaining.is_empty() {
        path.push(remaining);
    }
    Some(path)
}

/// The user home directory, resolved lexically from the environment:
/// `HOME`, then `USERPROFILE`, then `HOMEDRIVE`+`HOMEPATH`. Never assembled
/// by joining `$HOME` to per-user document dirs.
pub fn user_home_from_env() -> Option<PathBuf> {
    if let Some(home) = env_path("HOME") {
        return Some(home);
    }
    if let Some(home) = env_path("USERPROFILE") {
        return Some(home);
    }
    let drive = env::var_os("HOMEDRIVE").filter(|value| !value.is_empty())?;
    let path = env::var_os("HOMEPATH").filter(|value| !value.is_empty())?;
    let mut combined = drive;
    combined.push(path);
    Some(PathBuf::from(combined))
}

/// Drop the macOS data-volume firmlink prefix so a home-relative path and
/// the same path under that prefix classify as the same location.
/// Lexical only; does not stat.
pub fn strip_macos_data_volume(path: &Path) -> PathBuf {
    let prefix = Path::new("/").join("System").join("Volumes").join("Data");
    match path.strip_prefix(prefix) {
        Ok(rest) if rest.as_os_str().is_empty() => PathBuf::from("/"),
        Ok(rest) => Path::new("/").join(rest),
        Err(_) => path.to_path_buf(),
    }
}

fn env_path(name: &str) -> Option<PathBuf> {
    env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

fn os_matches(oses: &[String], os: &str) -> bool {
    oses.is_empty() || oses.iter().any(|value| value == os)
}

fn expand_all(templates: &[String], roots: &HostRoots) -> Vec<PathBuf> {
    templates
        .iter()
        .filter_map(|template| expand_template(template, roots))
        .collect()
}

fn expand_os_paths(entries: &[PathEntry], os: &str, roots: &HostRoots) -> Vec<PathBuf> {
    entries
        .iter()
        .filter(|entry| os_matches(&entry.oses, os))
        .filter_map(|entry| expand_template(&entry.path, roots))
        .collect()
}

fn nvm_default_version(manifest: &ScanManifest, home: &Path) -> Option<String> {
    let nvm = home.join(".nvm");
    let mut seen = BTreeSet::new();
    let mut current = String::from("default");
    for _ in 0..8 {
        if !seen.insert(current.clone()) {
            return None;
        }
        if let Some(version) = nvm_node_version_dir_name(&current) {
            return Some(version);
        }
        if current.contains("..") || Path::new(&current).is_absolute() {
            return None;
        }
        let alias = nvm.join("alias").join(&current);
        if manifest.denied(&alias, Some(home)) {
            return None;
        }
        let text = fs::read_to_string(alias).ok()?;
        current = text.trim().to_string();
        if current.is_empty() || current.len() > 64 {
            return None;
        }
    }
    None
}

fn nvm_node_version_dir_name(value: &str) -> Option<String> {
    let trimmed = value.trim().trim_start_matches('v');
    let mut parts = trimmed.split('.');
    let major = parts.next()?;
    let minor = parts.next()?;
    if major.is_empty()
        || !major.chars().all(|item| item.is_ascii_digit())
        || minor.is_empty()
        || !minor.chars().next()?.is_ascii_digit()
    {
        return None;
    }
    Some(format!("v{trimmed}"))
}

fn is_windows_network_path(path: &Path) -> bool {
    let raw = path.as_os_str().to_string_lossy();
    let normalized = raw.replace('\\', "/");
    let Some(root) = normalized.strip_prefix("//") else {
        return false;
    };
    root.strip_prefix("?/")
        .or_else(|| root.strip_prefix("./"))
        .is_none_or(|verbatim| {
            verbatim
                .get(..4)
                .is_some_and(|prefix| prefix.eq_ignore_ascii_case("UNC/"))
        })
}

fn is_media_library(path: &Path, extensions: &[String]) -> bool {
    path.extension()
        .and_then(|value| value.to_str())
        .map(str::to_ascii_lowercase)
        .is_some_and(|extension| extensions.iter().any(|item| item == &extension))
}

fn lexical(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    if normalized.as_os_str().is_empty() {
        path.to_path_buf()
    } else {
        normalized
    }
}

fn dedupe(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut seen = BTreeSet::<String>::new();
    let mut out = Vec::new();
    for path in paths {
        let key = path.to_string_lossy().to_ascii_lowercase();
        if seen.insert(key) {
            out.push(path);
        }
    }
    out
}

fn components_of(path: &Path) -> VecDeque<OsString> {
    lexical(path)
        .components()
        .map(|component| component.as_os_str().to_os_string())
        .collect()
}

fn binary_candidate_paths(dir: &Path, name: &str) -> Vec<PathBuf> {
    #[cfg(not(target_os = "windows"))]
    let candidates = vec![dir.join(name)];

    #[cfg(target_os = "windows")]
    let mut candidates = vec![dir.join(name)];
    #[cfg(target_os = "windows")]
    {
        if Path::new(name).extension().is_none() {
            for extension in windows_binary_extensions() {
                candidates.push(dir.join(format!("{}{}", name, extension)));
            }
        }
    }
    dedupe(candidates)
}

#[cfg(target_os = "windows")]
fn windows_binary_extensions() -> Vec<String> {
    let mut extensions = env::var("PATHEXT")
        .unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".to_string())
        .split(';')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| {
            if value.starts_with('.') {
                value.to_string()
            } else {
                format!(".{}", value)
            }
        })
        .collect::<Vec<_>>();
    for extension in [".exe", ".cmd", ".bat", ".com"] {
        if !extensions
            .iter()
            .any(|value| value.eq_ignore_ascii_case(extension))
        {
            extensions.push(extension.to_string());
        }
    }
    extensions
}

#[cfg(test)]
mod tests {
    use super::*;

    fn posix_absolute(parts: &[&str]) -> PathBuf {
        PathBuf::from(format!("/{}", parts.join("/")))
    }

    fn fixture_roots() -> HostRoots {
        let home = PathBuf::from("/profile");
        HostRoots {
            home: Some(home.clone()),
            appdata: Some(PathBuf::from("/app-data")),
            local_appdata: Some(PathBuf::from("/local-app-data")),
            xdg_config: Some(home.join(".config")),
            xdg_data: Some(home.join(".local/share")),
            program_data: Some(PathBuf::from("/program-data")),
            program_files: Some(PathBuf::from("/program-files")),
            program_files_x86: None,
            extra: BTreeMap::from([("portable".to_string(), PathBuf::from("/portable"))]),
        }
    }

    fn fixture_manifest() -> ScanManifest {
        ScanManifest {
            schema_version: "fixture.v1".to_string(),
            deny: DenySpec::standard(),
            binaries: vec![
                BinaryDirGroup {
                    oses: vec!["macos".to_string()],
                    dirs: vec![
                        "/opt/homebrew/bin".to_string(),
                        "{home}/.local/bin".to_string(),
                    ],
                },
                BinaryDirGroup {
                    oses: vec!["windows".to_string()],
                    dirs: vec![
                        "{local_appdata}/Microsoft/WindowsApps".to_string(),
                        "{home}/scoop/shims".to_string(),
                        "{program_data}/chocolatey/bin".to_string(),
                    ],
                },
            ],
            agents: vec![
                AgentScanPaths {
                    id: "claude".to_string(),
                    binaries: vec![
                        PathEntry {
                            path: "{home}/.claude/local".to_string(),
                            oses: vec![],
                        },
                        PathEntry {
                            path: "{home}/.claude/local/node_modules/.bin".to_string(),
                            oses: vec![],
                        },
                    ],
                    config: vec![
                        PathEntry {
                            path: "{home}/.claude/settings.json".to_string(),
                            oses: vec![],
                        },
                        PathEntry {
                            path: "{home}/.claude.json".to_string(),
                            oses: vec![],
                        },
                    ],
                    detection: vec![PathEntry {
                        path: "{home}/.claude".to_string(),
                        oses: vec![],
                    }],
                    history: vec![
                        HistoryEntry {
                            path: "{home}/.claude/agents".to_string(),
                            kind: "claude-agents".to_string(),
                        },
                        HistoryEntry {
                            path: "{home}/.claude/projects".to_string(),
                            kind: "claude-project-transcripts".to_string(),
                        },
                    ],
                    ..AgentScanPaths::default()
                },
                AgentScanPaths {
                    id: "codex".to_string(),
                    binaries: vec![
                        PathEntry {
                            path: "/Applications/ChatGPT.app/Contents/Resources/codex-cli/bin"
                                .to_string(),
                            oses: vec!["macos".to_string()],
                        },
                        PathEntry {
                            path:
                                "{home}/Applications/ChatGPT.app/Contents/Resources/codex-cli/bin"
                                    .to_string(),
                            oses: vec!["macos".to_string()],
                        },
                    ],
                    history: vec![HistoryEntry {
                        path: "{portable}/client-state/codex/sessions".to_string(),
                        kind: "fixture-store".to_string(),
                    }],
                    ..AgentScanPaths::default()
                },
            ],
        }
    }

    fn unc_path(host: &str, rest: &str) -> PathBuf {
        PathBuf::from(format!(r"\\{host}\{rest}"))
    }

    #[test]
    fn personal_and_network_locations_are_denied_without_stating() {
        let manifest = fixture_manifest();
        let home = PathBuf::from("/profile");
        for denied_path in [
            PathBuf::from("/profile/Downloads/bin"),
            PathBuf::from("/profile/Desktop/tools"),
            PathBuf::from("/profile/Documents/scripts"),
            PathBuf::from("/profile/Pictures"),
            PathBuf::from("/profile/Music/library"),
            PathBuf::from("/profile/Pictures/Personal.photoslibrary"),
            posix_absolute(&["Volumes", "team-share", "bin"]),
            posix_absolute(&["System", "Volumes", "Data", "profile", "Desktop", "tools"]),
            posix_absolute(&["System", "Volumes", "Data", "profile", "Pictures"]),
            posix_absolute(&["System", "Volumes", "Data", "profile", "Music", "library"]),
        ] {
            assert!(
                manifest.denied(&denied_path, Some(&home)),
                "{denied_path:?}"
            );
        }
        assert!(manifest.denied(
            &PathBuf::from("/profile/Desktop/tools"),
            Some(&posix_absolute(&["System", "Volumes", "Data", "profile"])),
        ));
        assert!(manifest.denied(
            &unc_path("server", r"redirected-profile\AppData"),
            Some(&home),
        ));
        assert!(manifest.denied(
            Path::new(r"\\?\UNC\server\redirected-profile\AppData"),
            Some(&home),
        ));
        assert!(!manifest.denied(Path::new(r"\\?\C:\Profile\lico\AppData"), Some(&home),));
    }

    #[test]
    fn agent_store_locations_are_admitted() {
        let manifest = fixture_manifest();
        let roots = fixture_roots();
        let claude = PathBuf::from("/profile/.claude");
        let homebrew = PathBuf::from("/opt/homebrew/bin");
        assert!(!manifest.denied(&claude, roots.home.as_deref()));
        assert!(!manifest.denied(&homebrew, roots.home.as_deref()));
        assert!(manifest.admitted_scan_path(&claude, "macos", &roots));
        assert!(manifest.denied(
            &PathBuf::from("/profile/.local/bin/../../Desktop/tool"),
            roots.home.as_deref()
        ));
        assert!(!manifest.admitted_scan_path(
            &PathBuf::from("/opt/homebrew/bin/../Desktop/tool"),
            "macos",
            &roots
        ));
    }

    #[test]
    fn agent_specific_paths_apply_os_filters_and_extra_roots() {
        let manifest = fixture_manifest();
        let roots = fixture_roots();
        let codex_macos = manifest.agent_binary_dirs("codex", "macos", &roots);
        assert_eq!(
            codex_macos,
            vec![
                PathBuf::from("/Applications/ChatGPT.app/Contents/Resources/codex-cli/bin"),
                PathBuf::from("/profile/Applications/ChatGPT.app/Contents/Resources/codex-cli/bin"),
            ]
        );
        assert!(manifest
            .agent_binary_dirs("codex", "linux", &roots)
            .is_empty());
        assert!(manifest
            .binary_dirs("macos", &roots)
            .contains(&PathBuf::from("/opt/homebrew/bin")));
        assert!(!manifest
            .binary_dirs("linux", &roots)
            .contains(&PathBuf::from("/opt/homebrew/bin")));
        let history = manifest.history_roots("codex", &roots);
        assert_eq!(history.len(), 1);
        assert_eq!(
            history[0].path,
            PathBuf::from("/portable/client-state/codex/sessions")
        );
        assert_eq!(
            expand_template("{portable}/x", &roots),
            Some(PathBuf::from("/portable/x"))
        );
    }

    #[test]
    fn windows_package_manager_dirs_are_listed() {
        let manifest = fixture_manifest();
        let roots = fixture_roots();
        let dirs = manifest.binary_dirs("windows", &roots);
        assert!(dirs.contains(&PathBuf::from("/local-app-data/Microsoft/WindowsApps")));
        assert!(dirs.contains(&PathBuf::from("/profile/scoop/shims")));
        assert!(dirs.contains(&PathBuf::from("/program-data/chocolatey/bin")));
    }

    #[test]
    fn windows_network_roots_never_enter_the_binary_probe_set() {
        let manifest = fixture_manifest();
        let mut roots = fixture_roots();
        roots.appdata = Some(unc_path("server", r"profile\AppData\Roaming"));
        roots.local_appdata = Some(unc_path("server", r"profile\AppData\Local"));
        let dirs = manifest.binary_dirs("windows", &roots);
        assert!(dirs.iter().all(|path| !is_windows_network_path(path)));
    }

    #[test]
    fn nvm_default_alias_adds_that_node_bin_dir() {
        let manifest = fixture_manifest();
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default();
        let home = std::env::temp_dir().join(format!(
            "a4aa-scan-nvm-{}-{}",
            stamp.as_secs(),
            stamp.subsec_nanos()
        ));
        fs::create_dir_all(home.join(".nvm/alias/lts")).unwrap();
        fs::write(home.join(".nvm/alias/default"), "lts/krypton\n").unwrap();
        fs::write(home.join(".nvm/alias/lts/krypton"), "22.14.0\n").unwrap();
        let mut roots = HostRoots::from_home(&home);
        roots
            .extra
            .insert("portable".to_string(), PathBuf::from("/portable"));
        let expected = home.join(".nvm/versions/node/v22.14.0/bin");
        assert!(manifest.binary_dirs("macos", &roots).contains(&expected));
        assert!(manifest.allow_prefixes("macos", &roots).contains(&expected));
        fs::write(home.join(".nvm/alias/default"), "../Desktop\n").unwrap();
        assert!(!manifest
            .binary_dirs("macos", &roots)
            .iter()
            .any(|path| path.ends_with("Desktop") || path.ends_with("Desktop/bin")));
        let _ = fs::remove_dir_all(&home);
    }

    #[cfg(unix)]
    #[test]
    fn automatic_probe_rejects_symlink_chain_in_a_parent_component() {
        use std::os::unix::fs::symlink;

        let manifest = fixture_manifest();
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default();
        let home = std::env::temp_dir().join(format!(
            "a4aa-scan-symlink-{}-{}",
            stamp.as_secs(),
            stamp.subsec_nanos()
        ));
        let denied_target = home.join("Documents").join("claude");
        let chained_link = home.join("catalog-link");
        let allowlisted_link = home.join(".claude");
        let nested_probe = allowlisted_link.join("settings.json");
        fs::create_dir_all(&denied_target).unwrap();
        fs::write(denied_target.join("settings.json"), "{}").unwrap();
        symlink(&denied_target, &chained_link).unwrap();
        symlink(&chained_link, &allowlisted_link).unwrap();

        let mut roots = HostRoots::from_home(&home);
        roots
            .extra
            .insert("portable".to_string(), PathBuf::from("/portable"));
        assert!(manifest.admitted_scan_path(&nested_probe, "macos", &roots));
        assert!(manifest.symlink_escapes_denied_location(&nested_probe, &roots));
        assert!(!manifest.probe_exists(&nested_probe, "macos", &roots));

        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn the_real_home_is_a_valid_named_catalog_base() {
        let manifest = fixture_manifest();
        let home = PathBuf::from("/profile");
        assert!(!manifest.catalog_home_denied(&home, Some(&home)));
        assert!(manifest.catalog_home_denied(&home.join("Desktop"), Some(&home)));
        assert!(
            manifest.catalog_home_denied(&unc_path("server", "redirected-profile"), Some(&home))
        );
    }

    #[test]
    fn unused_agent_discovery_never_authorizes_automatic_execution() {
        let manifest = fixture_manifest();
        assert!(!automatic_agent_execution_admitted());
        assert!(
            !manifest.discovered_agent_may_execute(Path::new("/opt/homebrew/bin/claude"), false)
        );
        assert!(manifest.discovered_agent_may_execute(Path::new("/opt/homebrew/bin/claude"), true));
        assert!(!manifest.discovered_agent_may_execute(
            &posix_absolute(&["Volumes", "team-share", "claude"]),
            true
        ));
    }

    #[test]
    fn unused_agent_probes_skip_other_app_containers_without_denying_them() {
        let manifest = fixture_manifest();
        let roots = fixture_roots();
        let cursor_support =
            PathBuf::from("/profile/Library/Application Support/Code/User/workspaceStorage");
        let kimi_logs = PathBuf::from("/profile/Library/Logs/Kimi");
        let pnpm = PathBuf::from("/profile/Library/pnpm");
        let claude = PathBuf::from("/profile/.claude");
        assert!(manifest.is_other_app_container(&cursor_support, &roots));
        assert!(manifest.is_other_app_container(&kimi_logs, &roots));
        assert!(!manifest.is_other_app_container(&pnpm, &roots));
        assert!(!manifest.is_other_app_container(&claude, &roots));
        assert!(!manifest.denied(&cursor_support, roots.home.as_deref()));
        assert!(manifest.is_other_app_container(
            &PathBuf::from("/profile/Library/Mobile Documents/com~apple~CloudDocs"),
            &roots
        ));
        assert!(manifest.is_other_app_container(
            &PathBuf::from("/profile/Library/CloudStorage/iCloudDrive"),
            &roots
        ));
    }

    #[test]
    fn selected_agent_named_store_exists_skips_other_app_until_selected() {
        let manifest = fixture_manifest();
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default();
        let home = std::env::temp_dir().join(format!(
            "a4aa-selected-store-{}-{}",
            stamp.as_secs(),
            stamp.subsec_nanos()
        ));
        let other_app = home
            .join("Library")
            .join("Application Support")
            .join("Code")
            .join("User")
            .join("globalStorage")
            .join("state.vscdb");
        fs::create_dir_all(other_app.parent().unwrap()).unwrap();
        fs::write(&other_app, []).unwrap();
        let own_store = home
            .join(".local")
            .join("share")
            .join("kilo")
            .join("kilo.db");
        fs::create_dir_all(own_store.parent().unwrap()).unwrap();
        fs::write(&own_store, []).unwrap();

        assert!(!manifest.selected_agent_named_store_exists(&other_app, &home, false));
        assert!(manifest.selected_agent_named_store_exists(&other_app, &home, true));
        assert!(manifest.selected_agent_named_store_exists(&own_store, &home, false));
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn named_catalog_files_under_an_explicit_home_may_exist_without_the_host_allowlist() {
        let manifest = fixture_manifest();
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default();
        let home = std::env::temp_dir().join(format!(
            "a4aa-scan-catalog-home-{}-{}",
            stamp.as_secs(),
            stamp.subsec_nanos()
        ));
        let settings = home.join(".claude").join("settings.json");
        fs::create_dir_all(settings.parent().unwrap()).unwrap();
        fs::write(&settings, "{}\n").unwrap();
        assert!(manifest.probe_exists_under_home(&settings, &home));
        assert!(!manifest.probe_exists(&settings, "macos", &fixture_roots()));
        assert!(!manifest.probe_exists_under_home(
            Path::new("/profile/Desktop/.claude/settings.json"),
            Path::new("/profile/Desktop"),
        ));
        assert!(!manifest.probe_exists_under_home(
            &posix_absolute(&["Volumes", "team-share", ".claude", "settings.json"]),
            &posix_absolute(&["Volumes", "team-share"]),
        ));
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn find_binary_in_dirs_skips_denied_and_symlinked_candidates() {
        let manifest = fixture_manifest();
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default();
        let dir = std::env::temp_dir().join(format!(
            "a4aa-find-binary-{}-{}",
            stamp.as_secs(),
            stamp.subsec_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        assert_eq!(
            manifest.find_binary_in_dirs(&["fixture-agent"], std::slice::from_ref(&dir)),
            None
        );
        let binary = dir.join("fixture-agent");
        fs::write(&binary, "synthetic").unwrap();
        assert_eq!(
            manifest.find_binary_in_dirs(&["fixture-agent"], std::slice::from_ref(&dir)),
            Some(binary)
        );
        let _ = fs::remove_dir_all(&dir);
    }
}
