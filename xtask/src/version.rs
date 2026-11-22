//! `cargo xtask version` prints the workspace version, and `cargo xtask version set <version>`
//! changes it.
//!
//! The version is `[workspace.package] version` in the root `Cargo.toml`. The `shellcraft-*` crates
//! and `xtask` inherit it with `version.workspace = true`; the `craft-<name>` crates keep their own.

use std::ops::Range;
use std::path::Path;

use craft_xtask::runner;
use toml_edit::Document;

const MISSING: &str = "the root Cargo.toml has no `version` in [workspace.package]";
const NOT_A_STRING: &str = "[workspace.package] version is not a string";

/// The value of `version` in `[workspace.package]`.
pub fn read(manifest: &str) -> Result<String, String> {
    let (value, _) = locate(manifest)?;
    Ok(value)
}

/// `manifest` with the workspace version set to `new`. Only the version's quoted value changes;
/// every other byte of the file, line endings and comments included, stays as it is.
pub fn replace(manifest: &str, new: &str) -> Result<String, String> {
    validate(new)?;
    let (_, span) = locate(manifest)?;
    let mut out = String::with_capacity(manifest.len() + new.len());
    out.push_str(&manifest[..span.start]);
    out.push('"');
    out.push_str(new);
    out.push('"');
    out.push_str(&manifest[span.end..]);
    Ok(out)
}

/// The workspace version and the byte range of its quoted value in `manifest`, found by parsing the
/// file as TOML.
fn locate(manifest: &str) -> Result<(String, Range<usize>), String> {
    let doc = Document::parse(manifest).map_err(|e| format!("Cargo.toml is not valid TOML: {e}"))?;
    let item = doc.get("workspace").and_then(|w| w.get("package")).and_then(|p| p.get("version")).ok_or(MISSING)?;
    let value = item.as_str().ok_or(NOT_A_STRING)?;
    if value.is_empty() {
        return Err("[workspace.package] version is empty".into());
    }
    let span = item.span().filter(|s| manifest.get(s.clone()).is_some()).ok_or("the position of [workspace.package] version is unknown")?;
    Ok((value.to_string(), span))
}

/// A version is `MAJOR.MINOR.PATCH` with an optional `-pre.release` part and no build metadata,
/// and each number fits in a `u64`, as Cargo requires.
pub fn validate(version: &str) -> Result<(), String> {
    let invalid = || Err(format!("`{version}` is not a version like 1.2.3 or 1.2.3-rc.1"));
    let (core, pre) = match version.split_once('-') {
        Some((core, pre)) => (core, Some(pre)),
        None => (version, None),
    };
    let numbers: Vec<&str> = core.split('.').collect();
    let bad_number =
        |n: &&str| n.is_empty() || !n.bytes().all(|b| b.is_ascii_digit()) || (n.len() > 1 && n.starts_with('0')) || n.parse::<u64>().is_err();
    if numbers.len() != 3 || numbers.iter().any(bad_number) {
        return invalid();
    }
    // A numeric prerelease part may not start with 0; a part with a letter in it may.
    let bad_part = |part: &str| {
        part.is_empty()
            || !part.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
            || (part.len() > 1 && part.starts_with('0') && part.bytes().all(|b| b.is_ascii_digit()))
    };
    if let Some(pre) = pre
        && (pre.is_empty() || pre.split('.').any(bad_part))
    {
        return invalid();
    }
    Ok(())
}

/// Replaces the manifest at `path` with `text` atomically, so a failed write leaves the old
/// manifest in place.
fn write_manifest(path: &Path, text: &str) -> Result<(), String> {
    craft_atomic::atomic_write(path, text.as_bytes()).map_err(|e| format!("replace {}: {e}", path.display()))
}

/// Sets the version in the manifest at `path` (whose text is `text`) to `new`, then runs
/// `update`. When the file no longer holds `text` just before the write, nothing is written and
/// that is an error. This check does not stop an editor that saves the file between the check and
/// the write. When `update` fails, the old manifest is written back, unless the file changed
/// after this function wrote it; then the file is left as it is. When the update passes but the file
/// changed while it ran, that is an error too, because Cargo.lock may not match the file. Returns
/// the old version.
fn set(path: &Path, text: &str, new: &str, update: impl FnOnce() -> Result<(), String>) -> Result<String, String> {
    let old = read(text)?;
    let written = replace(text, new)?;
    let current = std::fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    if current != text {
        return Err("Cargo.toml changed after it was read, so it was left as it is".into());
    }
    write_manifest(path, &written)?;
    let updated = update();
    let now = std::fs::read_to_string(path).map_err(|r| format!("Cargo.toml could not be read back after the update: {r}"))?;
    match updated {
        Ok(()) if now == written => Ok(old),
        Ok(()) if read(&now).as_deref() == Ok(new) => {
            Err("Cargo.toml changed while the update ran, so it was left as it is; run `cargo update --workspace` to bring Cargo.lock in line".into())
        }
        Ok(()) => Err(format!("Cargo.toml changed while the update ran and no longer holds {new}, so it was left as it is")),
        Err(e) if now != written => Err(format!("{e}; Cargo.toml changed while the update ran, so it was left as it is")),
        Err(e) => Err(match write_manifest(path, text) {
            Ok(()) => format!("{e}; Cargo.toml is back at {old}"),
            Err(restore) => format!("{e}; Cargo.toml could not be put back: {restore}"),
        }),
    }
}

/// `cargo update --workspace` rewrites the workspace crates' entries in Cargo.lock and upgrades
/// no dependency. It tries offline first.
fn update_lock(root: &Path) -> Result<(), String> {
    let mut offline = runner::cargo(root);
    offline.args(["update", "--workspace", "--offline"]);
    runner::run(offline, "cargo update --workspace --offline").or_else(|_| {
        let mut online = runner::cargo(root);
        online.args(["update", "--workspace"]);
        runner::run(online, "cargo update --workspace")
    })
}

/// `cargo xtask version [set <version>]`; `args` are the words after `version`.
pub fn run_command(root: &Path, args: &[String]) -> Result<(), String> {
    let path = root.join("Cargo.toml");
    let text = std::fs::read_to_string(&path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    match args.as_slice() {
        [] => {
            println!("{}", read(&text)?);
            Ok(())
        }
        ["set", new] => {
            let old = set(&path, &text, new, || update_lock(root))?;
            println!("version: {old} -> {new}");
            Ok(())
        }
        _ => Err("usage: cargo xtask version [set <version>]".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MANIFEST: &str = "[workspace]\nmembers = [\"a\"]\n\n[workspace.package]\nversion = \"0.1.0\"\nedition = \"2024\"\n\n[workspace.dependencies]\nserde = { version = \"1\" }\n";

    #[test]
    fn reads_the_workspace_version_only() {
        assert_eq!(read(MANIFEST).unwrap(), "0.1.0");
        assert!(read("[package]\nversion = \"1.0.0\"\n").is_err());
    }

    #[test]
    fn replaces_only_the_workspace_version() {
        let out = replace(MANIFEST, "1.2.3-rc.1").unwrap();
        assert_eq!(out, MANIFEST.replace("version = \"0.1.0\"", "version = \"1.2.3-rc.1\""));
        assert!(out.contains("serde = { version = \"1\" }"));
    }

    #[test]
    fn reads_and_keeps_comments_on_the_version_and_header_lines() {
        let commented = MANIFEST.replace("version = \"0.1.0\"", "version = \"0.1.0\" # the release series");
        assert_eq!(read(&commented).unwrap(), "0.1.0");
        assert_eq!(replace(&commented, "0.2.0").unwrap(), commented.replace("\"0.1.0\"", "\"0.2.0\""));
        assert!(read(&MANIFEST.replace("\"0.1.0\"", "0.1.0")).is_err());

        let header = MANIFEST.replace("[workspace.package]\n", "[workspace.package] # inherited by the app crates\n");
        assert_eq!(read(&header).unwrap(), "0.1.0");
        assert_eq!(replace(&header, "0.2.0").unwrap(), header.replace("\"0.1.0\"", "\"0.2.0\""));
    }

    #[test]
    fn finds_the_table_and_not_text_that_looks_like_it() {
        let sample = "[workspace.metadata]\nexample = \"\"\"\n[workspace.package]\nversion = \"9.9.9\"\n\"\"\"\n\n";
        let manifest = format!("{sample}{MANIFEST}");
        assert_eq!(read(&manifest).unwrap(), "0.1.0");
        assert_eq!(replace(&manifest, "0.2.0").unwrap(), format!("{sample}{}", MANIFEST.replace("\"0.1.0\"", "\"0.2.0\"")));
    }

    #[test]
    fn keeps_crlf_line_endings() {
        let crlf = MANIFEST.replace('\n', "\r\n");
        assert_eq!(replace(&crlf, "2.0.0").unwrap(), crlf.replace("\"0.1.0\"", "\"2.0.0\""));
        let mixed = MANIFEST.replacen('\n', "\r\n", 1);
        assert_eq!(replace(&mixed, "2.0.0").unwrap(), mixed.replace("\"0.1.0\"", "\"2.0.0\""));
    }

    #[test]
    fn accepts_versions_cargo_accepts_and_rejects_the_rest() {
        for good in ["0.1.0", "1.20.300", "1.0.0-rc.1", "1.0.0-alpha", "1.0.0-x-y.2", "1.0.0-0", "1.0.0-01a", "18446744073709551615.0.0"] {
            assert!(validate(good).is_ok(), "{good}");
        }
        for bad in
            ["", "1.0", "1.0.0.0", "v1.0.0", "01.0.0", "1..0", "1.0.0-", "1.0.0-a..b", "1.0.0+meta", "1.a.0", "1.0.0-01", "18446744073709551616.0.0"]
        {
            assert!(validate(bad).is_err(), "{bad}");
            assert!(replace(MANIFEST, bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_failed_lock_update_puts_the_manifest_back() {
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.subsec_nanos());
        let dir = std::env::temp_dir().join(format!("xtask-version-{}-{nanos}", std::process::id()));
        std::fs::create_dir(&dir).unwrap();
        let path = dir.join("Cargo.toml");
        std::fs::write(&path, MANIFEST).unwrap();
        let err = set(&path, MANIFEST, "2.0.0", || Err("cargo update failed".into())).unwrap_err();
        assert!(err.contains("back at 0.1.0"), "{err}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), MANIFEST);
        assert_eq!(set(&path, MANIFEST, "2.0.0", || Ok(())).unwrap(), "0.1.0");
        assert_eq!(read(&std::fs::read_to_string(&path).unwrap()).unwrap(), "2.0.0");

        // An edit made while the update ran is kept.
        let edited = MANIFEST.replace("edition", "rust-version = \"1.95\"\nedition");
        std::fs::write(&path, MANIFEST).unwrap();
        let err = set(&path, MANIFEST, "3.0.0", || {
            std::fs::write(&path, &edited).unwrap();
            Err("cargo update failed".into())
        })
        .unwrap_err();
        assert!(err.contains("left as it is"), "{err}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), edited);

        // A version changed by someone else while a passing update ran is reported, not claimed.
        let other = MANIFEST.replace("\"0.1.0\"", "\"5.0.0\"");
        std::fs::write(&path, MANIFEST).unwrap();
        let err = set(&path, MANIFEST, "4.0.0", || {
            std::fs::write(&path, &other).unwrap();
            Ok(())
        })
        .unwrap_err();
        assert!(err.contains("no longer holds 4.0.0"), "{err}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), other);

        // Another edit made while a passing update ran is kept and reported, because Cargo.lock may not match it.
        let added = MANIFEST.replace("[workspace.dependencies]\n", "[workspace.dependencies]\nlog = \"0.4\"\n");
        let added_new = added.replace("\"0.1.0\"", "\"7.0.0\"");
        std::fs::write(&path, MANIFEST).unwrap();
        let err = set(&path, MANIFEST, "7.0.0", || {
            std::fs::write(&path, &added_new).unwrap();
            Ok(())
        })
        .unwrap_err();
        assert!(err.contains("cargo update --workspace"), "{err}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), added_new);
        std::fs::write(&path, &other).unwrap();

        // A manifest saved by someone else after it was read is not overwritten, and the update does not run.
        let err = set(&path, MANIFEST, "6.0.0", || panic!("the update ran")).unwrap_err();
        assert!(err.contains("changed after it was read"), "{err}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), other);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_workspace_version_is_valid() {
        let root = crate::root().unwrap();
        let text = std::fs::read_to_string(root.join("Cargo.toml")).unwrap();
        validate(&read(&text).unwrap()).unwrap();
    }
}
