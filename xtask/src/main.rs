//! Project checks, run as `cargo xtask <command>`.
//!
//! The steps run through `craft-xtask`'s runner, which runs them in order, stops at the first
//! failure and prints a summary. Each step also runs on its own, for example `cargo xtask clippy`.

mod version;
mod wasm;
mod workspace;

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use craft_xtask::runner::{self, RunOptions, Step, StepLog, TestMode};

/// This package, which the workspace test command leaves out (see [`test`]).
const XTASK_PACKAGE: &str = "xtask";

/// The steps of `cargo xtask ci`, in the order they run.
const CI_STEPS: &[&str] = &["fmt", "clippy", "test", "wasm"];

const USAGE: &str = "\
usage: cargo xtask <command>

commands:
  fmt        cargo fmt --all -- --check
  clippy     cargo clippy --workspace --all-targets -- -D warnings
  test       the workspace tests, then the tests of this xtask package
  wasm       cargo check --target wasm32-unknown-unknown for the crates that build for the web
  ci         fmt, clippy, test and wasm in that order, stopping at the first failure (wasm is left
             out while no crate builds for the web); `ci --report <file>` also writes the result
             of each step to <file> as JSON
  version    print the workspace version; `version set <version>` changes it and Cargo.lock
";

fn main() -> ExitCode {
    // Arguments are read as `OsString`, so a report path that is not UTF-8 is accepted.
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    let rest = args.get(1..).unwrap_or(&[]);
    let result = match args.first().map(|a| a.to_str()) {
        Some(Some("ci")) => root().and_then(|root| ci(&root, rest)),
        Some(Some(name)) if CI_STEPS.contains(&name) => root().and_then(|root| single(&root, name, rest)),
        Some(Some("version")) => text_args(rest).and_then(|rest| root().and_then(|root| version::run_command(&root, &rest))),
        Some(Some("-h" | "--help" | "help")) | None => {
            print!("{USAGE}");
            Ok(())
        }
        Some(_) => Err(format!("unknown command `{}`\n\n{USAGE}", args.first().map(|a| a.to_string_lossy()).unwrap_or_default())),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

/// The workspace root: the parent of this package's manifest directory.
fn root() -> Result<PathBuf, String> {
    runner::root(Path::new(env!("CARGO_MANIFEST_DIR"))).map_err(String::from)
}

/// `args` as text, or an error naming the first argument that is not UTF-8.
fn text_args(args: &[OsString]) -> Result<Vec<String>, String> {
    args.iter().map(|a| a.to_str().map(str::to_owned).ok_or_else(|| format!("`{}` is not valid UTF-8", a.to_string_lossy()))).collect()
}

/// `cargo xtask ci [--report <file>]`.
///
/// The `wasm` step is left out while no crate builds for the web, so that the summary and the
/// report never count a check that did not run as passed.
fn ci(root: &Path, args: &[OsString]) -> Result<(), String> {
    let options = match args {
        [] => RunOptions::default(),
        [flag, path] if flag == "--report" => RunOptions::default().with_report(path),
        _ => return Err(format!("usage: cargo xtask ci [--report <file>]\n\n{USAGE}")),
    };
    let names: Vec<&str> = CI_STEPS.iter().copied().filter(|name| *name != "wasm" || !wasm::crates().is_empty()).collect();
    if names.len() < CI_STEPS.len() {
        eprintln!("note: the wasm step is left out because no crate in the workspace builds for {} yet", wasm::TARGET);
    }
    let mut steps: Vec<Step<'_>> = names.iter().filter_map(|name| step(root, name)).collect();
    runner::run_steps(&mut steps, &options).map(|_| ()).map_err(String::from)
}

/// One step of `ci` run on its own. The `wasm` step with no crate to check is an error, so a script
/// that runs it never reads the skipped check as passed.
fn single(root: &Path, name: &str, args: &[OsString]) -> Result<(), String> {
    if !args.is_empty() {
        return Err(format!("`cargo xtask {name}` takes no arguments\n\n{USAGE}"));
    }
    if name == "wasm" && wasm::crates().is_empty() {
        return Err(format!("wasm: not run, because no crate in the workspace builds for {} yet", wasm::TARGET));
    }
    let mut steps: Vec<Step<'_>> = step(root, name).into_iter().collect();
    runner::run_steps(&mut steps, &RunOptions::default().with_title(name)).map(|_| ()).map_err(String::from)
}

/// The step called `name`, or `None` for a name that is not a step.
fn step<'a>(root: &'a Path, name: &str) -> Option<Step<'a>> {
    let step = match name {
        "fmt" => Step::new("fmt", move |log| fmt(root, log)),
        "clippy" => Step::new("clippy", move |log| clippy(root, log)),
        "test" => Step::new("test", move |log| test(root, log)),
        "wasm" => Step::new("wasm", move |log| wasm::run(root, log, wasm::crates())),
        _ => return None,
    };
    Some(step)
}

fn fmt(root: &Path, log: &mut StepLog) -> Result<(), String> {
    let mut c = runner::cargo(root);
    c.args(["fmt", "--all", "--", "--check"]);
    runner::run(c, log.command("cargo fmt --all -- --check"))
}

fn clippy(root: &Path, log: &mut StepLog) -> Result<(), String> {
    let mut c = runner::cargo(root);
    c.args(["clippy", "--workspace", "--all-targets", "--", "-D", "warnings"]);
    runner::run(c, log.command("cargo clippy --workspace --all-targets -- -D warnings"))
}

/// The workspace tests without this package, then this package's own unit tests.
///
/// The workspace command leaves this package out, so that the test build does not replace the
/// `xtask` executable that is running it, which Windows does not allow. It runs only when the
/// workspace has another member, because Cargo fails on a selection with no package in it. The unit
/// tests build into a test executable of their own, so the second command does not replace the
/// running one either.
fn test(root: &Path, log: &mut StepLog) -> Result<(), String> {
    let (workspace, label) = runner::nextest_or_test(root, XTASK_PACKAGE, TestMode::CargoTest, &[] as &[&str]).map_err(String::from)?;
    let own_label = format!("cargo test --package {XTASK_PACKAGE} --bins");
    if workspace::members(root)?.iter().any(|name| name != XTASK_PACKAGE) {
        log.command(&format!("{label} && {own_label}"));
        runner::run(workspace, &label)?;
    } else {
        log.command(&own_label);
        println!("test: the workspace has no member besides {XTASK_PACKAGE}, so `{label}` did not run");
    }
    let mut own = runner::cargo(root);
    own.args(["test", "--package", XTASK_PACKAGE, "--bins"]);
    runner::run(own, &own_label)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_wasm_step_with_no_crate_to_check_is_an_error() {
        if wasm::crates().is_empty() {
            let err = single(Path::new("."), "wasm", &[]).unwrap_err();
            assert!(err.contains("not run"), "{err}");
        }
    }
}
