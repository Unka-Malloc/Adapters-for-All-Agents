//! `a4aa-dialects` — a convenience registry over the workspace's
//! `a4aa-<agent>` dialect crates.
//!
//! This crate exists for **multi-agent hosts** that need runtime lookup by
//! dialect name (for example a `--dialect <name>` flag). It pulls in every
//! adapter crate, so depending on it compiles all dialect descriptors into
//! the binary.
//!
//! **Single-agent hosts should not use this crate.** A host that only ever
//! drives Pi depends on `a4aa-pi` alone and calls `a4aa_pi::adapt(&set)` —
//! that keeps the dependency graph (and compile time) at exactly one
//! descriptor. Nothing in `a4aa` core requires this registry.

use a4aa::Dialect;

/// `(name, descriptor)` pairs for every adapter crate in the workspace.
/// Names match each adapter's `Dialect::id`.
static DIALECTS: &[(&str, &Dialect)] = &[
    ("antigravity", a4aa_antigravity::dialect()),
    ("claude", a4aa_claude::dialect()),
    ("codex", a4aa_codex::dialect()),
    ("commandcode", a4aa_command_code::dialect()),
    ("cursor", a4aa_cursor::dialect()),
    ("devin", a4aa_devin::dialect()),
    ("dsh", a4aa_dsh::dialect()),
    ("factory", a4aa_factory::dialect()),
    ("grok", a4aa_grok::dialect()),
    ("hermes", a4aa_hermes::dialect()),
    ("kilocode", a4aa_kilo_code::dialect()),
    ("kimicode", a4aa_kimi_code::dialect()),
    ("kiro", a4aa_kiro::dialect()),
    ("openclaw", a4aa_open_claw::dialect()),
    ("opencode", a4aa_opencode::dialect()),
    ("pi", a4aa_pi::dialect()),
];

/// Look a dialect up by id (`"codex"`, `"pi"`, `"generic"`, ...). Returns
/// `None` for agents with no adapter crate in this workspace — callers
/// should report the unknown name rather than guessing.
pub fn dialect(name: &str) -> Option<&'static Dialect> {
    if name == "generic" {
        static GENERIC: Dialect = Dialect::generic();
        return Some(&GENERIC);
    }
    DIALECTS.iter().find(|(n, _)| *n == name).map(|(_, d)| *d)
}

/// Every `(name, descriptor)` pair, plus the portable `generic` baseline.
/// Useful for listing `--dialect` choices in host CLIs.
pub fn all() -> impl Iterator<Item = (&'static str, &'static Dialect)> {
    static GENERIC: Dialect = Dialect::generic();
    DIALECTS
        .iter()
        .copied()
        .chain(std::iter::once(("generic", &GENERIC as &'static Dialect)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_returns_the_adapter_crate_descriptor() {
        assert_eq!(dialect("pi").map(|d| d.id), Some("pi"));
        assert_eq!(dialect("codex").map(|d| d.id), Some("codex"));
        assert_eq!(dialect("generic").map(|d| d.id), Some("generic"));
        assert!(dialect("no-such-agent").is_none());
    }

    #[test]
    fn all_covers_every_adapter_plus_generic() {
        let names: Vec<_> = all().map(|(n, _)| n).collect();
        assert_eq!(names.len(), 28);
        for (name, d) in all() {
            assert_eq!(name, d.id);
        }
    }
}
