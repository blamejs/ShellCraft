//! `cargo xtask wasm`: the crates that build for the web compile for `wasm32-unknown-unknown`.

use std::path::Path;

use craft_xtask::runner::{self, StepLog};

/// The target checked.
pub const TARGET: &str = "wasm32-unknown-unknown";

/// The crates checked: every `craft-<name>` crate that builds for the web, every L0 `shellcraft-*`
/// crate, and later `shellcraft-ui-egui` with its `board` feature (AGENTS.md › Layers). The pull
/// request that adds such a crate adds it here.
const CRATES: &[&str] = &[];

/// The crates the step checks.
pub fn crates() -> &'static [&'static str] {
    CRATES
}

/// Runs `cargo check --target wasm32-unknown-unknown` on `crates`. The callers leave the step out
/// while `crates` is empty; an empty list here is an error, so it never counts as a pass.
pub fn run(root: &Path, log: &mut StepLog, crates: &[&str]) -> Result<(), String> {
    if crates.is_empty() {
        return Err(format!("no crate to check for {TARGET}"));
    }
    let mut c = runner::cargo(root);
    c.args(["check", "--target", TARGET]);
    for name in crates {
        c.args(["-p", name]);
    }
    let label = format!("cargo check --target {TARGET} -p {}", crates.join(" -p "));
    runner::run(c, log.command(&label)).map_err(|e| format!("{e} (when the target is missing, add it with `rustup target add {TARGET}`)"))
}
