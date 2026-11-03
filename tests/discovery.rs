//! Discovery engine integration: the published cards project into a
//! [`ScanManifest`] — the card stays the single data authority, so a host
//! scanning carded agents needs no second catalog.

use a4aa::discovery::{DenySpec, HostRoots, ScanManifest};
use std::path::{Path, PathBuf};

#[test]
fn from_cards_projects_the_card_surface_into_the_manifest() {
    let card = a4aa_codex::card();
    let manifest = ScanManifest::from_cards(&[card]);

    let roots = HostRoots::from_home(Path::new("/profile"));
    assert_eq!(
        manifest.config_path("codex", "macos", &roots),
        Some(PathBuf::from("/profile/.codex/config.toml"))
    );
    assert!(
        manifest
            .history_roots("codex", &roots)
            .iter()
            .any(|root| root.kind == "codex-session-store"
                && root.path == Path::new("/profile/.codex/sessions")),
        "codex card history root projects into the manifest"
    );
    assert_eq!(manifest.deny, DenySpec::standard());
}

#[test]
fn every_published_card_declares_its_own_scan_surface() {
    for (_name, card) in a4aa_dialects::cards() {
        let manifest = ScanManifest::from_cards(&[card]);
        let roots = HostRoots::from_home(Path::new("/profile"));
        // A card's own locations admit on its own scan: every detection and
        // history root it declares expands and survives the deny spec.
        for root in manifest.history_roots(card.id, &roots) {
            assert!(
                manifest.admitted_scan_path(&root.path, "macos", &roots),
                "{:?} history root {} is not admit-scan-reachable",
                card.id,
                root.path.display(),
            );
        }
    }
}
