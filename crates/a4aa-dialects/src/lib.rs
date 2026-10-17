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

use a4aa::card::AgentCard;
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

/// `(name, card)` pairs — every adapter crate in [`DIALECTS`] publishes an
/// [`AgentCard`]; only the portable `generic` baseline has none.
static CARDS: &[(&str, &AgentCard)] = &[
    ("antigravity", a4aa_antigravity::card()),
    ("claude", a4aa_claude::card()),
    ("codex", a4aa_codex::card()),
    ("commandcode", a4aa_command_code::card()),
    ("cursor", a4aa_cursor::card()),
    ("devin", a4aa_devin::card()),
    ("dsh", a4aa_dsh::card()),
    ("factory", a4aa_factory::card()),
    ("grok", a4aa_grok::card()),
    ("hermes", a4aa_hermes::card()),
    ("kilocode", a4aa_kilo_code::card()),
    ("kimicode", a4aa_kimi_code::card()),
    ("kiro", a4aa_kiro::card()),
    ("openclaw", a4aa_open_claw::card()),
    ("opencode", a4aa_opencode::card()),
    ("pi", a4aa_pi::card()),
];

/// Look an agent's card up by id. `None` for `generic` and unknown names —
/// callers fall back to `dialect(name)` for write-surface knowledge.
pub fn card(name: &str) -> Option<&'static AgentCard> {
    CARDS.iter().find(|(n, _)| *n == name).map(|(_, c)| *c)
}

/// Every published `(name, card)` pair.
pub fn cards() -> impl Iterator<Item = (&'static str, &'static AgentCard)> {
    CARDS.iter().copied()
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
        let mut names: Vec<_> = all().map(|(n, _)| n).collect();
        names.sort_unstable();
        assert_eq!(
            names,
            [
                "antigravity",
                "claude",
                "codex",
                "commandcode",
                "cursor",
                "devin",
                "dsh",
                "factory",
                "generic",
                "grok",
                "hermes",
                "kilocode",
                "kimicode",
                "kiro",
                "openclaw",
                "opencode",
                "pi",
            ]
        );
        for (name, d) in all() {
            assert_eq!(name, d.id);
        }
    }

    #[test]
    fn cards_match_their_dialects() {
        for (name, card) in cards() {
            assert_eq!(name, card.id);
            assert_eq!(dialect(name).map(|d| d.id), Some(card.dialect.id));
        }
        for (name, _) in all() {
            if name == "generic" {
                continue;
            }
            assert!(card(name).is_some(), "{name} has a dialect but no card");
        }
    }
}
