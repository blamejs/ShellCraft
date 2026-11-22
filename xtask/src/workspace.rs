//! What the steps need to know about the workspace, read from `cargo metadata`.

use std::path::Path;

use craft_xtask::runner;

/// The names of the workspace members, from `cargo metadata --no-deps`.
pub fn members(root: &Path) -> Result<Vec<String>, String> {
    let mut c = runner::cargo(root);
    c.args(["metadata", "--no-deps", "--format-version", "1"]);
    let out = c.output().map_err(|e| format!("cargo metadata: failed to spawn: {e}"))?;
    if !out.status.success() {
        return Err(format!("cargo metadata: exited with {}: {}", out.status, String::from_utf8_lossy(&out.stderr).trim()));
    }
    member_names(&out.stdout)
}

/// The package names in `cargo metadata --no-deps` output.
pub fn member_names(metadata: &[u8]) -> Result<Vec<String>, String> {
    let value: serde_json::Value = serde_json::from_slice(metadata).map_err(|e| format!("cargo metadata: {e}"))?;
    let packages = value.get("packages").and_then(serde_json::Value::as_array).ok_or("cargo metadata: no `packages` list")?;
    packages
        .iter()
        .map(|p| {
            p.get("name").and_then(serde_json::Value::as_str).map(str::to_owned).ok_or_else(|| "cargo metadata: a package has no name".to_owned())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_member_names_and_rejects_other_shapes() {
        let names = member_names(br#"{"packages":[{"name":"xtask"},{"name":"craft-queue"}],"version":1}"#).unwrap();
        assert_eq!(names, ["xtask", "craft-queue"]);
        assert!(member_names(br#"{"version":1}"#).is_err());
        assert!(member_names(br#"{"packages":[{"id":"x"}]}"#).is_err());
        assert!(member_names(b"not json").is_err());
    }
}
