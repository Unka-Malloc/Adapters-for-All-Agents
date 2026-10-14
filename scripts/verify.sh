#!/usr/bin/env bash
# Single-shot verification gate for the a4aa workspace: format, lint, test
# across the core crate and every a4aa-<agent> dialect crate.
set -euo pipefail
cd "$(dirname "$0")/.."

cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --all-targets
