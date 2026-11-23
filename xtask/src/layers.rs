//! `cargo xtask layers`: the crate layers and the manifest rules of AGENTS.md › Layers and
//! › `craft-<name>` crates.
//!
//! - Every workspace member has a row in the crate table of AGENTS.md › Layers, and depends only on
//!   members in its own layer or a lower one. No crate in a layer depends on a tooling crate.
//! - Only L6 crates depend on the UI crates (`egui`, `eframe`, `winit`, `rfd` and the `egui_` and
//!   `egui-` crates), directly or through packages outside the workspace.
//! - Each app in [`MUST_NOT_LINK`] links none of the crates listed for it when it is built on its
//!   own with every feature on, for each of [`RELEASE_TARGETS`].
//! - A `craft-<name>` crate sets its own package fields, with the craft-libs edition and minimum
//!   Rust version, has its own `README.md`, depends on no `shellcraft` crate and does not use the
//!   word ShellCraft. Every other member inherits its package fields from `[workspace.package]`.
//!
//! Direct dependencies come from `cargo metadata --all-features`, dev- and build-dependencies
//! included. Paths through other packages come from `cargo tree -p` for each member on its own, as
//! Cargo resolves its features when that member alone is built.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::io::{BufRead, BufReader};
use std::path::{Component, Path, PathBuf};

use craft_xtask::runner::{self, StepLog};
use toml_edit::{Document, Item};

use crate::{wasm, workspace};

/// The layer that may depend on the UI crates.
pub const UI_LAYER: u8 = 6;

/// The license every `craft-<name>` crate sets.
pub const CRAFT_LICENSE: &str = "MIT OR Apache-2.0";

/// `edition` in craft-libs' `[workspace.package]` at the rev the root `Cargo.toml` pins. The pull
/// request that moves the pin updates it.
pub const CRAFT_LIBS_EDITION: &str = "2024";

/// `rust-version` in craft-libs' `[workspace.package]` at the rev the root `Cargo.toml` pins. The
/// pull request that moves the pin updates it.
pub const CRAFT_LIBS_RUST_VERSION: &str = "1.95";

/// The targets the releases ship: Linux, Windows and macOS, each on x86_64 and aarch64. The
/// must-not-link check resolves each app for each of them.
pub const RELEASE_TARGETS: &[&str] = &[
    "x86_64-unknown-linux-gnu",
    "aarch64-unknown-linux-gnu",
    "x86_64-pc-windows-msvc",
    "aarch64-pc-windows-msvc",
    "x86_64-apple-darwin",
    "aarch64-apple-darwin",
];

/// The header row of the crate table in AGENTS.md.
const TABLE_HEADER: &str = "| Crate | Layer |";

/// The layer cell of a tooling crate.
const TOOLING: &str = "tooling, outside the layers";

/// The UI crates by exact name. Names that start with `egui_` or `egui-` are UI crates too.
const UI_CRATES: &[&str] = &["egui", "eframe", "winit", "rfd"];

/// The package fields a `craft-<name>` crate sets itself, as plain strings.
const CRAFT_OWN_FIELDS: &[&str] = &["version", "license", "edition", "rust-version"];

/// The package fields a `craft-<name>` crate does not have, inherited or not.
const CRAFT_ABSENT_FIELDS: &[&str] = &["repository", "homepage", "authors"];

/// The `[package]` fields Cargo can take from `[workspace.package]`
/// (https://doc.rust-lang.org/cargo/reference/workspaces.html#the-package-table).
const INHERITABLE_FIELDS: &[&str] = &[
    "authors",
    "categories",
    "description",
    "documentation",
    "edition",
    "exclude",
    "homepage",
    "include",
    "keywords",
    "license",
    "license-file",
    "publish",
    "readme",
    "repository",
    "rust-version",
    "version",
];

/// The package fields every other member inherits with `<field>.workspace = true`.
const INHERITED_FIELDS: &[&str] = &["version", "edition", "rust-version", "license", "repository"];

/// The extensions of the files whose contents the ShellCraft-word scan of a `craft-<name>` crate
/// reads. It checks the names of all files and folders.
const SCANNED_EXTENSIONS: &[&str] = &["rs", "md", "toml"];

/// A crate an app must not link, or one feature of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Banned {
    pub krate: &'static str,
    /// `None` bans the crate; `Some(feature)` bans only that feature of it.
    pub feature: Option<&'static str>,
}

const fn krate(krate: &'static str) -> Banned {
    Banned { krate, feature: None }
}

/// The apps, by the folder of their manifest relative to the workspace root, and the crates they
/// must not link (AGENTS.md › Layers). An app whose folder holds no workspace member is skipped.
pub const MUST_NOT_LINK: &[(&str, &[Banned])] = &[(
    // The GUI.
    "apps/shellcraft",
    &[
        krate("tokio"),
        krate("russh"),
        krate("tokio-postgres"),
        krate("rusqlite"),
        krate("craft-vault"),
        krate("craft-keywrap"),
        krate("craft-scrub"),
        krate("craft-term-grid"),
        krate("craft-oauth"),
        Banned { krate: "craft-chat", feature: Some("http") },
        krate("craft-chat-store"),
    ],
)];

/// Where a crate sits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layer {
    /// L0 to L6.
    Level(u8),
    /// Tooling such as `xtask`: it may depend on any crate, and no crate in a layer depends on it.
    Tooling,
}

impl fmt::Display for Layer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Layer::Level(n) => write!(f, "L{n}"),
            Layer::Tooling => f.write_str("tooling"),
        }
    }
}

/// The kind of a dependency, as `cargo metadata` gives it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Normal,
    Dev,
    Build,
}

impl Kind {
    fn suffix(self) -> &'static str {
        match self {
            Kind::Normal => "",
            Kind::Dev => " as a dev-dependency",
            Kind::Build => " as a build-dependency",
        }
    }
}

/// A resolved dependency of a workspace member: the package name, which `package = "..."` renames
/// do not change, its kind, and whether Cargo resolved it to a workspace member (a path dependency,
/// or a registry or git dependency that a `[patch]` points at a member).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dep {
    pub name: String,
    pub kind: Kind,
    pub member: bool,
}

/// A workspace member.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Package {
    pub name: String,
    /// Cargo's package ID, which `cargo tree -p` takes when a dependency shares the name.
    pub id: String,
    pub manifest_path: String,
    /// The folder of the manifest relative to the workspace root, with `/` between components, or
    /// `None` for a manifest outside the root.
    pub dir: Option<String>,
    pub deps: Vec<Dep>,
    /// The source files of its targets and its `readme` file, as `cargo metadata` resolves them,
    /// that are outside the folder of its manifest.
    pub files_outside: Vec<String>,
    /// The source files of its targets and its `readme` file, as `cargo metadata` gives them.
    pub sources: Vec<String>,
}

/// What the ShellCraft-word scan found in a `craft-<name>` crate.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct CrateFiles {
    /// Every file and folder, relative to the crate, with `/` between components.
    pub names: Vec<String>,
    /// Each line of a `.rs`, `.md` or `.toml` file that holds the word: the file and the line
    /// number, from 1.
    pub word_lines: Vec<(String, usize)>,
    /// The symbolic links, which the scan does not follow.
    pub links: Vec<String>,
    /// The Git submodules, whose files the scan does not read.
    pub submodules: Vec<String>,
}

/// The crate table of AGENTS.md › Layers: each crate's name and layer.
pub fn parse_table(agents: &str) -> Result<BTreeMap<String, Layer>, String> {
    let mut lines = agents.lines().map(str::trim);
    if !lines.any(|l| l == TABLE_HEADER) {
        return Err(format!("AGENTS.md has no crate table with the header `{TABLE_HEADER}`"));
    }
    match lines.next() {
        Some(l) if l.contains('-') && l.chars().all(|c| matches!(c, '|' | '-' | ':' | ' ')) => {}
        _ => return Err("AGENTS.md: the crate table has no separator row after its header".into()),
    }
    let mut table = BTreeMap::new();
    for line in lines.take_while(|l| l.starts_with('|')) {
        let inner = line
            .strip_prefix('|')
            .and_then(|l| l.strip_suffix('|'))
            .ok_or_else(|| format!("AGENTS.md crate table: `{line}` does not end with `|`"))?;
        let cells: Vec<&str> = inner.split('|').map(str::trim).collect();
        let [name, layer] = cells.as_slice() else {
            return Err(format!("AGENTS.md crate table: `{line}` does not have two cells"));
        };
        let name = name
            .strip_prefix('`')
            .and_then(|n| n.strip_suffix('`'))
            .filter(|n| !n.is_empty() && !n.contains('`'))
            .ok_or_else(|| format!("AGENTS.md crate table: `{name}` is not a crate name in backticks"))?;
        let layer =
            parse_layer(layer).ok_or_else(|| format!("AGENTS.md crate table: `{layer}` for `{name}` is not L0 to L{UI_LAYER} or `{TOOLING}`"))?;
        if table.insert(name.to_owned(), layer).is_some() {
            return Err(format!("AGENTS.md crate table: `{name}` has more than one row"));
        }
    }
    if table.is_empty() {
        return Err("AGENTS.md: the crate table has no rows".into());
    }
    Ok(table)
}

fn parse_layer(cell: &str) -> Option<Layer> {
    if cell == TOOLING {
        return Some(Layer::Tooling);
    }
    let digits = cell.strip_prefix('L')?;
    if digits.len() != 1 {
        return None;
    }
    digits.parse::<u8>().ok().filter(|n| *n <= UI_LAYER).map(Layer::Level)
}

fn str_field<'v>(v: &'v serde_json::Value, key: &str) -> Option<&'v str> {
    v.get(key).and_then(serde_json::Value::as_str)
}

fn array_field<'v>(v: &'v serde_json::Value, key: &str) -> Option<&'v Vec<serde_json::Value>> {
    v.get(key).and_then(serde_json::Value::as_array)
}

/// The resolved dependency graph in `cargo metadata` output (not `--no-deps`).
struct Graph<'v> {
    root: &'v Path,
    packages: BTreeMap<&'v str, &'v serde_json::Value>,
    members: Vec<&'v str>,
    nodes: BTreeMap<&'v str, &'v serde_json::Value>,
}

/// One resolved dependency edge: the package id and the kinds it is declared with.
struct Edge<'v> {
    pkg: &'v str,
    kinds: Vec<Kind>,
}

impl<'v> Graph<'v> {
    fn parse(value: &'v serde_json::Value) -> Result<Self, String> {
        let root = Path::new(str_field(value, "workspace_root").ok_or("cargo metadata: no `workspace_root`")?);
        let mut packages = BTreeMap::new();
        for p in array_field(value, "packages").ok_or("cargo metadata: no `packages` list")? {
            packages.insert(str_field(p, "id").ok_or("cargo metadata: a package has no id")?, p);
        }
        let members = array_field(value, "workspace_members")
            .ok_or("cargo metadata: no `workspace_members` list")?
            .iter()
            .map(|m| m.as_str().ok_or("cargo metadata: a workspace member id is not a string"))
            .collect::<Result<_, _>>()?;
        let mut nodes = BTreeMap::new();
        for n in value.get("resolve").and_then(|r| array_field(r, "nodes")).ok_or("cargo metadata: no resolved dependency graph")? {
            nodes.insert(str_field(n, "id").ok_or("cargo metadata: a resolve node has no id")?, n);
        }
        Ok(Graph { root, packages, members, nodes })
    }

    fn package(&self, id: &str) -> Result<&'v serde_json::Value, String> {
        self.packages.get(id).copied().ok_or_else(|| format!("cargo metadata: {id} is not in `packages`"))
    }

    fn name(&self, id: &str) -> Result<&'v str, String> {
        str_field(self.package(id)?, "name").ok_or_else(|| format!("cargo metadata: {id} has no name"))
    }

    fn is_member(&self, id: &str) -> bool {
        self.members.contains(&id)
    }

    fn edges(&self, id: &str) -> Result<Vec<Edge<'v>>, String> {
        let Some(node) = self.nodes.get(id) else { return Ok(Vec::new()) };
        let deps = array_field(node, "deps").ok_or_else(|| format!("cargo metadata: the resolve node {id} has no deps"))?;
        deps.iter()
            .map(|d| {
                let pkg = str_field(d, "pkg").ok_or_else(|| format!("cargo metadata: a dependency of {id} has no package id"))?;
                let kinds = array_field(d, "dep_kinds").ok_or_else(|| format!("cargo metadata: {id}'s dependency {pkg} has no dep_kinds"))?;
                let kinds = kinds
                    .iter()
                    .map(|k| match str_field(k, "kind") {
                        Some("dev") => Kind::Dev,
                        Some("build") => Kind::Build,
                        _ => Kind::Normal,
                    })
                    .collect();
                Ok(Edge { pkg, kinds })
            })
            .collect()
    }
}

/// The workspace members and their resolved dependencies, from `cargo metadata` output with its
/// `resolve` graph (not `--no-deps`).
pub fn packages(metadata: &[u8]) -> Result<Vec<Package>, String> {
    let value: serde_json::Value = serde_json::from_slice(metadata).map_err(|e| format!("cargo metadata: {e}"))?;
    let graph = Graph::parse(&value)?;
    graph
        .members
        .iter()
        .map(|id| {
            let name = graph.name(id)?.to_owned();
            let manifest_path =
                str_field(graph.package(id)?, "manifest_path").ok_or_else(|| format!("cargo metadata: `{name}` has no manifest path"))?.to_owned();
            let dir = relative_dir(Path::new(&manifest_path), graph.root);
            let mut deps = Vec::new();
            for edge in graph.edges(id)? {
                let dep = graph.name(edge.pkg)?;
                let member = graph.is_member(edge.pkg);
                for kind in edge.kinds {
                    let dep = Dep { name: dep.to_owned(), kind, member };
                    if !deps.contains(&dep) {
                        deps.push(dep);
                    }
                }
            }
            let package = graph.package(id)?;
            let crate_dir = Path::new(&manifest_path).parent().unwrap_or(Path::new(""));
            let targets = array_field(package, "targets").into_iter().flatten().filter_map(|t| str_field(t, "src_path"));
            let sources: Vec<String> = targets.chain(str_field(package, "readme")).map(str::to_owned).collect();
            let files_outside = sources.iter().filter(|path| !stays_inside(crate_dir, Path::new(path))).cloned().collect();
            Ok(Package { name, id: (*id).to_owned(), manifest_path, dir, deps, sources, files_outside })
        })
        .collect()
}

/// A package in a [`Tree`]: its label, `<package>|<features>|` as `cargo tree --format "{p}|{f}|"`
/// prints it, and whether it builds for the host (under a build-dependency or a proc-macro crate)
/// or for the target. The same package built with other features or for the other platform is
/// another node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Node<'t> {
    label: &'t str,
    host: bool,
}

/// One level of the prefix that `cargo tree --charset utf8` draws before a package.
const TREE_PREFIXES: &[&str] = &["│   ", "├── ", "└── ", "    "];

/// The output of [`package_tree`] as a graph.
///
/// Each prefix level of a line is one level of depth, and a line is a dependency of the nearest
/// line above it that is one level less deep. A `[build-dependencies]` or `[dev-dependencies]`
/// line, drawn at its package's depth, marks the dependencies of that kind that follow it. A node
/// seen before ends with ` (*)` and has no lines below it, so its dependencies come from the place
/// it first appears, under any root.
struct Tree<'t> {
    roots: Vec<Node<'t>>,
    children: BTreeMap<Node<'t>, Vec<(Node<'t>, Kind)>>,
}

/// A package on the path from a root to the line being read, and the kind of the dependencies
/// listed under it at that point.
struct Frame<'t> {
    node: Node<'t>,
    kind: Kind,
}

impl<'t> Tree<'t> {
    fn parse(text: &'t str) -> Self {
        let mut roots = Vec::new();
        let mut children: BTreeMap<Node<'t>, Vec<(Node<'t>, Kind)>> = BTreeMap::new();
        let mut stack: Vec<Frame<'t>> = Vec::new();
        for line in text.lines() {
            let mut rest = line;
            let mut depth = 0;
            while let Some(after) = TREE_PREFIXES.iter().find_map(|p| rest.strip_prefix(p)) {
                rest = after;
                depth += 1;
            }
            if let Some(header) = rest.strip_prefix('[') {
                let kind = match header {
                    "build-dependencies]" => Kind::Build,
                    "dev-dependencies]" => Kind::Dev,
                    _ => Kind::Normal,
                };
                stack.truncate(depth + 1);
                if let Some(frame) = stack.get_mut(depth) {
                    frame.kind = kind;
                }
                continue;
            }
            let label = rest.strip_suffix(" (*)").unwrap_or(rest);
            if label.matches('|').count() < 2 {
                continue;
            }
            stack.truncate(depth);
            let node = match stack.last() {
                Some(parent) if depth > 0 => {
                    let host = parent.node.host || parent.kind == Kind::Build || is_proc_macro(label);
                    let node = Node { label, host };
                    children.entry(parent.node).or_default().push((node, parent.kind));
                    node
                }
                _ => {
                    let node = Node { label, host: is_proc_macro(label) };
                    roots.push(node);
                    node
                }
            };
            stack.push(Frame { node, kind: Kind::Normal });
        }
        Tree { roots, children }
    }

    fn children(&self, node: Node<'t>) -> &[(Node<'t>, Kind)] {
        self.children.get(&node).map_or(&[], Vec::as_slice)
    }
}

/// The package name of a [`Tree`] node.
fn node_name(node: &str) -> &str {
    node.split_whitespace().next().unwrap_or_default()
}

/// The package part of a [`Tree`] node: name, version and source.
fn node_package(node: &str) -> &str {
    node.rsplitn(3, '|').nth(2).unwrap_or_default()
}

/// Whether a [`Tree`] node is a proc-macro crate: `cargo tree` prints `(proc-macro)` after the
/// version, before the source, as in `serde_derive v1.0.228 (proc-macro)`.
fn is_proc_macro(label: &str) -> bool {
    node_package(label).splitn(3, ' ').nth(2).is_some_and(|rest| rest.starts_with("(proc-macro)"))
}

/// The features of a [`Tree`] node.
fn node_features(node: &str) -> impl Iterator<Item = &str> {
    node.rsplit('|').nth(1).unwrap_or_default().split(',').filter(|f| !f.is_empty())
}

/// A workspace member as it appears in a [`Tree`]: its package name and the folder of its
/// manifest, which `cargo tree` prints as the source of a path package.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Member {
    pub name: String,
    pub dir: String,
}

impl Member {
    fn is(&self, node: &str) -> bool {
        node_name(node) == self.name && node_package(node).ends_with(&format!("({})", self.dir))
    }
}

/// What a member reaches through one of its dependencies in the graph of its own build.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reach {
    /// The member.
    pub from: String,
    /// Its direct dependency.
    pub via: String,
    /// The first UI crate reached below `via`.
    pub ui: Option<String>,
    /// The members reached below `via`.
    pub members: Vec<String>,
}

/// Every [`Reach`] of the roots of the output of [`package_tree`]. It fails when the output has no
/// root or a root that is not a member.
pub fn reaches(tree: &str, members: &[Member]) -> Result<Vec<Reach>, String> {
    let tree = Tree::parse(tree);
    let member_of = |node: Node<'_>| members.iter().find(|m| m.is(node.label));
    if tree.roots.is_empty() {
        return Err("cargo tree listed no package, so no dependency could be checked".into());
    }
    if let Some(r) = tree.roots.iter().find(|r| member_of(**r).is_none()) {
        return Err(format!("cargo tree listed `{}`, which is not a workspace member, so its dependencies could not be checked", r.label));
    }
    // The walk goes through every package of the root's build, members included, since Cargo builds
    // them with the features the root's whole graph turns on. It does not follow the
    // dev-dependencies of packages other than the root, which are not built for it.
    let mut out = Vec::new();
    for &root in &tree.roots {
        let Some(from) = member_of(root) else { continue };
        for &(start, _) in tree.children(root) {
            let mut reach = Reach { from: from.name.clone(), via: node_name(start.label).to_owned(), ui: None, members: Vec::new() };
            let mut seen = BTreeSet::from([start]);
            let mut queue = std::collections::VecDeque::from([start]);
            while let Some(node) = queue.pop_front() {
                for &(child, _) in tree.children(node).iter().filter(|(_, kind)| *kind != Kind::Dev) {
                    if !seen.insert(child) {
                        continue;
                    }
                    let name = node_name(child.label);
                    if reach.ui.is_none() && is_ui_crate(name) {
                        reach.ui = Some(name.to_owned());
                    }
                    if let Some(m) = member_of(child)
                        && !reach.members.contains(&m.name)
                    {
                        reach.members.push(m.name.clone());
                    }
                    queue.push_back(child);
                }
            }
            if (reach.ui.is_some() || !reach.members.is_empty()) && !out.contains(&reach) {
                out.push(reach);
            }
        }
    }
    Ok(out)
}

/// The craft crate rule across the dependency graph: a `craft-<name>` root of the output of
/// [`package_tree`] reaches no `shellcraft` package through another package, member or not. A direct
/// dependency on one is reported by [`check_craft_crate`].
pub fn check_craft_paths(tree: &str, members: &[Member]) -> Vec<String> {
    let tree = Tree::parse(tree);
    let mut problems = Vec::new();
    for &root in &tree.roots {
        let Some(from) = members.iter().find(|m| m.is(root.label) && is_craft_crate(&m.name)) else { continue };
        for &(hop, _) in tree.children(root) {
            if is_shellcraft_crate(node_name(hop.label)) {
                continue;
            }
            let mut seen = BTreeSet::from([root, hop]);
            let mut queue = std::collections::VecDeque::from([hop]);
            while let Some(node) = queue.pop_front() {
                // Another package's dev-dependencies are not built for its dependents.
                for &(child, _) in tree.children(node).iter().filter(|(_, kind)| *kind != Kind::Dev) {
                    if !seen.insert(child) {
                        continue;
                    }
                    let name = node_name(child.label);
                    if is_shellcraft_crate(name) {
                        let problem = format!(
                            "`{}` depends on `{}`, which depends on `{name}`; a craft crate depends on no shellcraft crate",
                            from.name,
                            node_name(hop.label)
                        );
                        if !problems.contains(&problem) {
                            problems.push(problem);
                        }
                    }
                    queue.push_back(child);
                }
            }
        }
    }
    problems
}

/// Every package the root `app` (a package name) links in the output of [`package_tree`], with the
/// features it is built with: its normal dependencies built for the target, followed through every
/// package. `None` when `app` is not a root of the output.
pub fn linked(tree: &str, app: &str) -> Option<BTreeMap<String, BTreeSet<String>>> {
    let tree = Tree::parse(tree);
    let root = *tree.roots.iter().find(|r| node_name(r.label) == app)?;
    let mut out: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut seen = BTreeSet::from([root]);
    let mut queue = std::collections::VecDeque::from([root]);
    while let Some(node) = queue.pop_front() {
        out.entry(node_name(node.label).to_owned()).or_default().extend(node_features(node.label).map(str::to_owned));
        for &(child, kind) in tree.children(node) {
            if kind == Kind::Normal && !child.host && seen.insert(child) {
                queue.push_back(child);
            }
        }
    }
    Some(out)
}

/// `cargo tree -p <id>` for the package `name` with Cargo package ID `id`, with every feature of it
/// on, for `target`, following `edges` (`normal,build,dev` or `normal,no-proc-macro`), in the form
/// [`Tree`] reads. Cargo resolves the features as a build of that package alone does, without the
/// features other members turn on. Without dev-dependencies, the features only they turn on are
/// left out too, as in a release build.
fn package_tree(root: &Path, name: &str, id: &str, target: &str, edges: &str) -> Result<String, String> {
    let label = format!("cargo tree --package {name} --target {target} --edges {edges}");
    let mut c = runner::cargo(root);
    c.args(["tree", "--locked", "--package", id, "--all-features", "--edges", edges, "--target", target]);
    c.args(["--prefix", "indent", "--charset", "utf8", "--format", "{p}|{f}|", "--color", "never"]);
    let out = c.output().map_err(|e| format!("{label}: failed to spawn: {e}"))?;
    if !out.status.success() {
        return Err(format!("{label}: exited with {}: {}", out.status, String::from_utf8_lossy(&out.stderr).trim()));
    }
    String::from_utf8(out.stdout).map_err(|e| format!("{label}: the output is not UTF-8: {e}"))
}

/// Whether `path`, rooted or relative to `base`, names a file inside `base` without passing
/// outside it on the way: a relative path with a `..` that leaves `base`, such as
/// `../x/README.md`, is outside even when it comes back in.
fn stays_inside(base: &Path, path: &Path) -> bool {
    let relative = if path.has_root() {
        match path.strip_prefix(base) {
            Ok(rest) => rest,
            Err(_) => return false,
        }
    } else {
        path
    };
    let mut depth = 0usize;
    for c in relative.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir if depth == 0 => return false,
            Component::ParentDir => depth -= 1,
            Component::Normal(_) => depth += 1,
            Component::RootDir | Component::Prefix(_) => return false,
        }
    }
    true
}

/// The folder of `manifest_path` relative to `root`, with `/` between components.
fn relative_dir(manifest_path: &Path, root: &Path) -> Option<String> {
    let dir = manifest_path.parent()?.strip_prefix(root).ok()?;
    dir.components()
        .map(|c| if let Component::Normal(part) = c { part.to_str() } else { None })
        .collect::<Option<Vec<_>>>()
        .map(|parts| parts.join("/"))
}

/// A layer below L6. Tooling is outside the layers and may depend on any crate.
fn below_ui_layer(layer: Layer) -> bool {
    matches!(layer, Layer::Level(n) if n < UI_LAYER)
}

fn is_ui_crate(name: &str) -> bool {
    UI_CRATES.contains(&name) || name.starts_with("egui_") || name.starts_with("egui-")
}

fn is_craft_crate(name: &str) -> bool {
    name.starts_with("craft-")
}

fn is_shellcraft_crate(name: &str) -> bool {
    name == "shellcraft" || name.starts_with("shellcraft-")
}

/// The layer rules: every member has a row, depends on members in its own layer or a lower one and
/// on no tooling crate, and only L6 members depend on a UI crate.
pub fn check_layers(table: &BTreeMap<String, Layer>, packages: &[Package]) -> Vec<String> {
    let mut problems = Vec::new();
    for p in packages {
        let Some(&from) = table.get(&p.name) else {
            problems.push(format!("`{}` has no row in the crate table of AGENTS.md › Layers", p.name));
            continue;
        };
        for d in &p.deps {
            if d.member {
                match (from, table.get(&d.name)) {
                    (Layer::Level(_), Some(Layer::Tooling)) => {
                        problems.push(format!("`{}` ({from}) depends on the tooling crate `{}`{}", p.name, d.name, d.kind.suffix()));
                    }
                    (Layer::Level(a), Some(&to @ Layer::Level(b))) if b > a => {
                        problems.push(format!("`{}` ({from}) depends on `{}` ({to}), a higher layer{}", p.name, d.name, d.kind.suffix()));
                    }
                    _ => {}
                }
            }
            if below_ui_layer(from) && is_ui_crate(&d.name) {
                problems.push(format!("`{}` ({from}) depends on the UI crate `{}`{}; only L{UI_LAYER} crates may", p.name, d.name, d.kind.suffix()));
            }
        }
    }
    problems
}

/// The layer and UI rules for what a member reaches below its direct dependencies ([`reaches`]),
/// run on the tree of one member and one target.
pub fn check_reach(table: &BTreeMap<String, Layer>, reaches: &[Reach]) -> Vec<String> {
    let mut problems = Vec::new();
    for r in reaches {
        let Some(&from) = table.get(&r.from) else { continue };
        let via = format!("`{}` ({from}) depends on `{}`, which depends on", r.from, r.via);
        for m in &r.members {
            match (from, table.get(m)) {
                (Layer::Level(_), Some(Layer::Tooling)) => problems.push(format!("{via} the tooling crate `{m}`")),
                (Layer::Level(a), Some(&to @ Layer::Level(b))) if b > a => problems.push(format!("{via} `{m}` ({to}), a higher layer")),
                _ => {}
            }
        }
        if let Some(ui) = &r.ui
            && below_ui_layer(from)
        {
            problems.push(format!("{via} the UI crate `{ui}`; only L{UI_LAYER} crates may"));
        }
    }
    problems
}

/// The `[package]` table of a member's manifest.
fn package_table<'d>(name: &str, doc: &'d Document<&str>) -> Result<&'d Item, String> {
    doc.get("package").filter(|p| p.is_table_like()).ok_or_else(|| format!("`{name}`: Cargo.toml has no [package] table"))
}

/// The manifest rules of a `craft-<name>` crate.
pub fn check_craft_crate(p: &Package, manifest: &str, has_readme: bool, files: &CrateFiles) -> Vec<String> {
    let name = &p.name;
    let mut problems = Vec::new();
    match Document::parse(manifest) {
        Ok(doc) => match package_table(name, &doc) {
            Ok(package) => {
                for field in CRAFT_OWN_FIELDS {
                    match package.get(field) {
                        Some(item) if item.as_str().is_some() => {}
                        Some(_) => problems.push(format!("`{name}` inherits `{field}`; a craft crate sets its own")),
                        None => problems.push(format!("`{name}` has no `{field}`; a craft crate sets its own")),
                    }
                }
                for (field, want) in [("license", CRAFT_LICENSE), ("edition", CRAFT_LIBS_EDITION), ("rust-version", CRAFT_LIBS_RUST_VERSION)] {
                    // Cargo reads `rust-version = "1.95"` and `"1.95.0"` as the same version.
                    let same = |value: &str| value == want || (field == "rust-version" && value.strip_suffix(".0") == Some(want));
                    if let Some(value) = package.get(field).and_then(Item::as_str)
                        && !same(value)
                    {
                        problems.push(format!("`{name}` has `{field} = \"{value}\"`; a craft crate has `{field} = \"{want}\"`"));
                    }
                }
                if let Some(version) = package.get("version").and_then(Item::as_str) {
                    match at_least_0_1_0(version) {
                        Some(true) => {}
                        Some(false) => problems.push(format!("`{name}` has `version = \"{version}\"`; a craft crate starts at 0.1.0")),
                        None => problems.push(format!("`{name}` has `version = \"{version}\"`, which is not a version like 0.1.0")),
                    }
                }
                for field in CRAFT_ABSENT_FIELDS {
                    if package.get(field).is_some() {
                        problems.push(format!("`{name}` has a `{field}` field; a craft crate has none"));
                    }
                }
                // Any other field taken from `[workspace.package]`, such as `description`.
                for field in INHERITABLE_FIELDS.iter().filter(|f| !CRAFT_OWN_FIELDS.contains(f) && !CRAFT_ABSENT_FIELDS.contains(f)) {
                    if package.get(field).and_then(|item| item.get("workspace")).and_then(Item::as_bool) == Some(true) {
                        problems.push(format!("`{name}` inherits `{field}`; a craft crate sets its own"));
                    }
                }
            }
            Err(e) => problems.push(e),
        },
        Err(e) => problems.push(format!("`{name}`: Cargo.toml is not valid TOML: {e}")),
    }
    if !has_readme {
        problems.push(format!("`{name}` has no README.md of its own"));
    }
    for d in &p.deps {
        if is_shellcraft_crate(&d.name) {
            problems.push(format!("`{name}` depends on `{}`{}; a craft crate depends on no shellcraft crate", d.name, d.kind.suffix()));
        }
    }
    // The crate's own folder moves with it; the folders above it do not.
    if let Some(folder) = Path::new(&p.manifest_path).parent().and_then(Path::file_name).map(|f| f.to_string_lossy())
        && folder.to_ascii_lowercase().contains("shellcraft")
    {
        problems.push(format!("`{name}`: its folder {folder} uses the word ShellCraft"));
    }
    for path in &files.names {
        if path.to_ascii_lowercase().contains("shellcraft") {
            problems.push(format!("`{name}`: the name {path} uses the word ShellCraft"));
        }
    }
    for (path, line) in &files.word_lines {
        problems.push(format!("`{name}`: {path}:{line} uses the word ShellCraft"));
    }
    for path in &p.files_outside {
        problems.push(format!("`{name}` uses {path}, which is outside its folder; a craft crate holds its own files"));
    }
    for path in &files.submodules {
        problems.push(format!("`{name}`: {path} is a Git submodule; a craft crate holds its own files"));
    }
    for path in &files.links {
        problems.push(format!("`{name}`: {path} is a symbolic link; a craft crate holds its own files"));
    }
    problems
}

/// Whether the SemVer version `version` is 0.1.0 or later, or `None` when it is not a version. Build
/// metadata (`+...`) does not count, and a prerelease of a version comes before it, so
/// `0.1.0-rc.1` is earlier than 0.1.0.
fn at_least_0_1_0(version: &str) -> Option<bool> {
    let without_build = version.split_once('+').map_or(version, |(v, _)| v);
    let (core, pre) = without_build.split_once('-').map_or((without_build, None), |(c, p)| (c, Some(p)));
    let numbers: Vec<u64> =
        core.split('.').map(|n| n.parse().ok().filter(|_| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))).collect::<Option<_>>()?;
    let [major, minor, patch] = numbers.as_slice() else { return None };
    if pre.is_some_and(str::is_empty) {
        return None;
    }
    Some((*major, *minor, *patch) > (0, 1, 0) || ((*major, *minor, *patch) == (0, 1, 0) && pre.is_none()))
}

/// The manifest rule of every member that is not a `craft-<name>` crate: it inherits its package
/// fields from `[workspace.package]`.
pub fn check_inherited(p: &Package, manifest: &str) -> Vec<String> {
    let name = &p.name;
    let doc = match Document::parse(manifest) {
        Ok(doc) => doc,
        Err(e) => return vec![format!("`{name}`: Cargo.toml is not valid TOML: {e}")],
    };
    let package = match package_table(name, &doc) {
        Ok(package) => package,
        Err(e) => return vec![e],
    };
    INHERITED_FIELDS
        .iter()
        .filter(|field| package.get(field).and_then(|item| item.get("workspace")).and_then(Item::as_bool) != Some(true))
        .map(|field| format!("`{name}` does not inherit `{field}` from [workspace.package]"))
        .collect()
}

/// The banned crates and features that the app `app` (a package name) links, from [`linked`].
pub fn check_links(app: &str, banned: &[Banned], linked: &BTreeMap<String, BTreeSet<String>>) -> Vec<String> {
    banned
        .iter()
        .filter_map(|b| {
            let features = linked.get(b.krate)?;
            match b.feature {
                None => Some(format!("`{app}` links `{}`, which it must not link", b.krate)),
                Some(f) if features.contains(f) => Some(format!("`{app}` links `{}` with its `{f}` feature, which it must not link", b.krate)),
                Some(_) => None,
            }
        })
        .collect()
}

/// A file of a crate as Git lists it: its path relative to the crate, with `/` between components,
/// and whether it is a symbolic link that the checkout wrote as a plain file (mode `120000` with
/// `core.symlinks=false`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitFile {
    pub path: String,
    pub link: bool,
    /// In Git's index; otherwise untracked and not ignored.
    pub tracked: bool,
    /// Tracked, and left off the disk by a sparse checkout.
    pub sparse: bool,
    /// A Git submodule (mode `160000`), whose files Git lists in the submodule only.
    pub submodule: bool,
}

/// The files under `dir` that move with the crate: the tracked ones, and the untracked ones that
/// `.gitignore` does not exclude, from `git ls-files` run in `dir`.
fn git_files(dir: &Path, output_dirs: &[PathBuf]) -> Result<Vec<GitFile>, String> {
    let run = |args: &[&str]| -> Result<Vec<u8>, String> {
        let label = format!("git ls-files {}", args.join(" "));
        let out = std::process::Command::new("git")
            // The repository is the one `dir` is in, whatever a calling hook set.
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .arg("ls-files")
            .args(args)
            .current_dir(dir)
            .output()
            .map_err(|e| format!("{label} in {}: failed to spawn: {e}", dir.display()))?;
        if !out.status.success() {
            return Err(format!("{label} in {}: exited with {}: {}", dir.display(), out.status, String::from_utf8_lossy(&out.stderr).trim()));
        }
        Ok(out.stdout)
    };
    // A path that is not UTF-8 is an error rather than a file the scan would open under another name.
    let text = |bytes: &[u8]| {
        String::from_utf8(bytes.to_vec()).map_err(|_| format!("git ls-files in {}: `{}` is not UTF-8", dir.display(), String::from_utf8_lossy(bytes)))
    };
    // Where the checkout writes symbolic links as links, the disk tells whether a file is one, which
    // also covers a link replaced by a file and not yet staged. Where `core.symlinks` is false, Git
    // writes them as plain files, and only the index mode tells.
    let plain_links = std::process::Command::new("git")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_CONFIG")
        .args(["config", "--bool", "core.symlinks"])
        .current_dir(dir)
        .output()
        .is_ok_and(|out| String::from_utf8_lossy(&out.stdout).trim() == "false");
    let mut files = Vec::new();
    // `-t -s` prints `<tag> <mode> <object> <stage>\t<path>` for each tracked file; the tag `S` marks
    // a file that a sparse checkout leaves off the disk.
    for entry in run(&["-z", "-t", "-s", "--", "."])?.split(|b| *b == 0).filter(|e| !e.is_empty()) {
        let entry = text(entry)?;
        let (info, path) = entry.split_once('\t').ok_or_else(|| format!("git ls-files -s: `{entry}` has no tab"))?;
        let mut fields = info.split(' ');
        let (tag, mode) = (fields.next().unwrap_or_default(), fields.next().unwrap_or_default());
        files.push(GitFile {
            path: path.to_owned(),
            link: plain_links && mode == "120000",
            tracked: true,
            sparse: tag == "S",
            submodule: mode == "160000",
        });
    }
    // Untracked files in Cargo's output folders inside the crate are build output; Git does not
    // list them.
    let excludes: Vec<String> = output_dirs
        .iter()
        .filter_map(|o| o.strip_prefix(dir).ok())
        .filter(|rel| !rel.as_os_str().is_empty())
        .map(|rel| format!(":(exclude,literal){}", rel.to_string_lossy().replace('\\', "/")))
        .collect();
    let mut args = vec!["-z", "--others", "--exclude-standard", "--", "."];
    args.extend(excludes.iter().map(String::as_str));
    for path in run(&args)?.split(|b| *b == 0).filter(|e| !e.is_empty()) {
        // A nested repository that `git add` would stage as a submodule is listed as `<dir>/`.
        let path = text(path)?;
        let (path, submodule) = match path.strip_suffix('/') {
            Some(repo) => (repo.to_owned(), true),
            None => (path, false),
        };
        files.push(GitFile { path, link: false, tracked: false, sparse: false, submodule });
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    files.dedup_by(|a, b| a.path == b.path);
    Ok(files)
}

/// The files of the crate in `dir` (from [`git_files`]) outside Cargo's `output_dirs`: every file
/// and folder name, the symbolic links (recorded as such by Git, or one on disk), and the lines of
/// its `.rs`, `.md` and `.toml` files that hold the word ShellCraft. A listed file that is not on
/// disk, a deleted tracked file, is skipped.
fn crate_files(dir: &Path, files: &[GitFile], output_dirs: &[PathBuf]) -> Result<CrateFiles, String> {
    let mut out = CrateFiles::default();
    let mut names = BTreeSet::new();
    for file in files {
        let path = dir.join(&file.path);
        if file.submodule {
            names.insert(file.path.clone());
            out.submodules.push(file.path.clone());
            continue;
        }
        // Untracked files in Cargo's output folders are build output; tracked files are source
        // wherever they are. An output folder that holds the whole crate is not one inside it.
        if !file.tracked && output_dirs.iter().any(|o| o.starts_with(dir) && o != dir && path.starts_with(o)) {
            continue;
        }
        // A folder on the way that is a symbolic link is itself the problem; the files below it
        // are not read.
        let mut linked_folder = None;
        let mut sofar = PathBuf::new();
        for part in Path::new(&file.path).parent().into_iter().flat_map(Path::components) {
            sofar.push(part);
            if std::fs::symlink_metadata(dir.join(&sofar)).is_ok_and(|m| m.is_symlink()) {
                linked_folder = Some(sofar.to_string_lossy().replace('\\', "/"));
                break;
            }
        }
        if let Some(folder) = linked_folder {
            names.insert(folder.clone());
            if !out.links.contains(&folder) {
                out.links.push(folder);
            }
            continue;
        }
        let meta = match std::fs::symlink_metadata(&path) {
            Ok(meta) => meta,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && file.sparse => {
                return Err(format!("{} is not checked out (sparse checkout), so the layers check cannot read it", path.display()));
            }
            // Deleted, or below a folder that is now a file, until the change is staged.
            Err(e) if matches!(e.kind(), std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory) => continue,
            Err(e) => return Err(format!("read {}: {e}", path.display())),
        };
        let mut folder = Path::new(&file.path).parent();
        while let Some(f) = folder.filter(|f| !f.as_os_str().is_empty()) {
            names.insert(f.to_string_lossy().replace('\\', "/"));
            folder = f.parent();
        }
        names.insert(file.path.clone());
        if file.link || meta.is_symlink() {
            out.links.push(file.path.clone());
            continue;
        }
        let scanned = path.extension().and_then(|e| e.to_str()).is_some_and(|e| SCANNED_EXTENSIONS.iter().any(|s| s.eq_ignore_ascii_case(e)));
        if !meta.is_file() || !scanned {
            continue;
        }
        let handle = std::fs::File::open(&path).map_err(|e| format!("read {}: {e}", path.display()))?;
        let lines = word_lines(BufReader::new(handle)).map_err(|e| format!("read {}: {e}", path.display()))?;
        out.word_lines.extend(lines.into_iter().map(|n| (file.path.clone(), n)));
    }
    out.names = names.into_iter().collect();
    out.word_lines.sort();
    out.links.sort();
    Ok(out)
}

/// Finds the lines that hold the word ShellCraft in a stream of characters, keeping only the last
/// characters of the line.
struct WordMatcher {
    line: usize,
    window: std::collections::VecDeque<u8>,
    hit: bool,
    found: Vec<usize>,
}

impl WordMatcher {
    const WORD: &'static [u8] = b"shellcraft";

    fn new() -> Self {
        WordMatcher { line: 1, window: std::collections::VecDeque::with_capacity(Self::WORD.len()), hit: false, found: Vec::new() }
    }

    /// One character, as an ASCII byte; any other character as a byte that is not a letter.
    fn push(&mut self, c: u8) {
        if c == b'\n' {
            if self.hit {
                self.found.push(self.line);
            }
            self.line += 1;
            self.hit = false;
            self.window.clear();
            return;
        }
        if self.hit {
            return;
        }
        if self.window.len() == Self::WORD.len() {
            self.window.pop_front();
        }
        self.window.push_back(c.to_ascii_lowercase());
        if self.window.iter().eq(Self::WORD.iter()) {
            self.hit = true;
        }
    }

    fn finish(mut self) -> Vec<usize> {
        if self.hit {
            self.found.push(self.line);
        }
        self.found
    }
}

/// The numbers, from 1, of the lines in `reader` that hold the word ShellCraft in any letter case.
/// It reads the bytes as they are, and also as UTF-16 in both byte orders, with or without a byte
/// order mark, and reports the lines any reading finds. It keeps no more than the last characters of
/// a line in memory.
fn word_lines(mut reader: impl BufRead) -> std::io::Result<Vec<usize>> {
    let mut bytes = WordMatcher::new();
    let mut little = WordMatcher::new();
    let mut big = WordMatcher::new();
    let mut pending: Option<u8> = None;
    loop {
        let buf = match reader.fill_buf() {
            Ok(buf) => buf,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        if buf.is_empty() {
            break;
        }
        // A UTF-16 unit outside ASCII, a byte order mark included, is a character that is not a letter.
        let ascii = |unit: u16| u8::try_from(unit).ok().filter(u8::is_ascii).unwrap_or(0);
        for &b in buf {
            bytes.push(b);
            match pending.take() {
                None => pending = Some(b),
                Some(first) => {
                    little.push(ascii(u16::from_le_bytes([first, b])));
                    big.push(ascii(u16::from_be_bytes([first, b])));
                }
            }
        }
        let n = buf.len();
        reader.consume(n);
    }
    let mut found = bytes.finish();
    found.extend(little.finish());
    found.extend(big.finish());
    found.sort_unstable();
    found.dedup();
    Ok(found)
}

/// The `sources` (relative to `dir` or rooted) that exist and resolve outside `dir` once symbolic
/// links are followed, such as `docs/guide.md` where `docs` links to another folder.
fn resolved_outside(dir: &Path, sources: &[String]) -> Result<Vec<String>, String> {
    let resolved_dir = std::fs::canonicalize(dir).map_err(|e| format!("read {}: {e}", dir.display()))?;
    Ok(sources
        .iter()
        .filter(|s| std::fs::canonicalize(dir.join(s.as_str())).is_ok_and(|resolved| !resolved.starts_with(&resolved_dir)))
        .cloned()
        .collect())
}

/// The folders from the crate `name`'s folder `dir` up to the workspace `root` (without it) that are
/// a symbolic link or the root of another Git repository, such as a submodule.
fn folder_problems(name: &str, root: &Path, dir: &Path) -> Vec<String> {
    let mut problems = Vec::new();
    for folder in dir.ancestors().take_while(|a| *a != root && a.starts_with(root)) {
        if std::fs::symlink_metadata(folder).is_ok_and(|m| m.is_symlink()) {
            problems.push(format!("`{name}`: its folder {} is a symbolic link; a craft crate holds its own files", folder.display()));
        }
        if std::fs::symlink_metadata(folder.join(".git")).is_ok() {
            problems.push(format!("`{name}`: its folder {} is a Git submodule; a craft crate holds its own files", folder.display()));
        }
    }
    problems
}

/// Cargo's output folders in `cargo metadata` output: `target_directory`, and `build_directory`
/// where Cargo reports one.
pub fn output_dirs(metadata: &[u8]) -> Result<Vec<PathBuf>, String> {
    let value: serde_json::Value = serde_json::from_slice(metadata).map_err(|e| format!("cargo metadata: {e}"))?;
    let target = str_field(&value, "target_directory").ok_or("cargo metadata: no `target_directory`")?;
    Ok([Some(target), str_field(&value, "build_directory")].into_iter().flatten().map(PathBuf::from).collect())
}

/// Runs every rule over the workspace.
pub fn run(root: &Path, log: &mut StepLog) -> Result<(), String> {
    log.command("cargo xtask layers");
    let agents_path = root.join("AGENTS.md");
    let agents = std::fs::read_to_string(&agents_path).map_err(|e| format!("read {}: {e}", agents_path.display()))?;
    let table = parse_table(&agents)?;
    let metadata = workspace::resolved_metadata(root)?;
    let packages = packages(&metadata)?;
    let output_dirs = output_dirs(&metadata)?;
    let mut problems = check_layers(&table, &packages);
    for p in &packages {
        let manifest_path = Path::new(&p.manifest_path);
        let manifest = std::fs::read_to_string(manifest_path).map_err(|e| format!("read {}: {e}", manifest_path.display()))?;
        if is_craft_crate(&p.name) {
            let dir = manifest_path.parent().ok_or_else(|| format!("`{}`: the manifest {} has no folder", p.name, manifest_path.display()))?;
            let folder = folder_problems(&p.name, root, dir);
            if !folder.is_empty() {
                // The files there are not the crate's own, so they are not scanned.
                problems.extend(folder);
                continue;
            }
            let files = git_files(dir, &output_dirs)?;
            // The README is a regular file that Git tracks or would add, so it moves with the crate.
            let has_readme =
                files.iter().any(|f| f.path == "README.md" && !f.link) && std::fs::symlink_metadata(dir.join("README.md")).is_ok_and(|m| m.is_file());
            let mut p = p.clone();
            for source in resolved_outside(dir, &p.sources)? {
                if !p.files_outside.contains(&source) {
                    p.files_outside.push(source);
                }
            }
            problems.extend(check_craft_crate(&p, &manifest, has_readme, &crate_files(dir, &files, &output_dirs)?));
        } else {
            problems.extend(check_inherited(p, &manifest));
        }
    }
    let apps: Vec<(&Package, &[Banned])> =
        MUST_NOT_LINK.iter().filter_map(|(dir, banned)| packages.iter().find(|p| p.dir.as_deref() == Some(*dir)).map(|p| (p, *banned))).collect();
    let members: Vec<Member> = packages
        .iter()
        .filter_map(|p| Path::new(&p.manifest_path).parent().map(|d| Member { name: p.name.clone(), dir: d.display().to_string() }))
        .collect();
    // Each problem with the targets it occurs on, in the order of RELEASE_TARGETS.
    let mut found: BTreeMap<String, Vec<&str>> = BTreeMap::new();
    // The web build is checked for the layer and craft crate rules; the GUI is a desktop app.
    for &target in RELEASE_TARGETS.iter().chain([&wasm::TARGET]) {
        let mut on_target = Vec::new();
        // Each member on its own, as Cargo resolves its features when it alone is built.
        // Tooling may depend on any crate, so its own tree has nothing to check unless it is a craft
        // crate.
        for p in packages.iter().filter(|p| is_craft_crate(&p.name) || table.get(&p.name).is_some_and(|l| *l != Layer::Tooling)) {
            let tree = package_tree(root, &p.name, &p.id, target, "normal,build,dev")?;
            on_target.extend(check_reach(&table, &reaches(&tree, &members)?));
            on_target.extend(check_craft_paths(&tree, &members));
        }
        if target != wasm::TARGET {
            for (app, banned) in &apps {
                let tree = package_tree(root, &app.name, &app.id, target, "normal,no-proc-macro")?;
                match linked(&tree, &app.name) {
                    Some(linked) => on_target.extend(check_links(&app.name, banned, &linked)),
                    None => on_target.push(format!("`{}`: cargo tree did not list the app, so its dependencies could not be checked", app.name)),
                }
            }
        }
        on_target.sort();
        on_target.dedup();
        for problem in on_target {
            found.entry(problem).or_default().push(target);
        }
    }
    problems.extend(found.into_iter().map(|(problem, targets)| format!("{problem} (on {})", targets.join(", "))));
    if problems.is_empty() {
        println!("layers: {} crates follow the layer and manifest rules", packages.len());
        Ok(())
    } else {
        Err(format!("layers: {} problems:\n  {}", problems.len(), problems.join("\n  ")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TABLE: &str = "\
### Layers

| Layer | What it holds |
|-------|---------------|
| L0 | Models |

| Crate | Layer |
|-------|-------|
| `xtask` | tooling, outside the layers |
| `craft-queue` | L0 |
| `shellcraft-engine` | L4 |
| `shellcraft-ui-egui` | L6 |

The GUI must not link tokio.
";

    /// A dependency on a workspace member.
    fn dep(name: &str, kind: Kind) -> Dep {
        Dep { name: name.into(), kind, member: true }
    }

    /// A dependency on a package outside the workspace.
    fn ext(name: &str, kind: Kind) -> Dep {
        Dep { name: name.into(), kind, member: false }
    }

    fn pkg(name: &str, deps: Vec<Dep>) -> Package {
        Package {
            name: name.into(),
            id: format!("path+file:///w/crates/{name}#0.1.0"),
            manifest_path: format!("/w/crates/{name}/Cargo.toml"),
            dir: Some(format!("crates/{name}")),
            deps,
            files_outside: Vec::new(),
            sources: Vec::new(),
        }
    }

    fn members(extra: Vec<Package>) -> Vec<Package> {
        let mut all = vec![pkg("xtask", vec![]), pkg("craft-queue", vec![]), pkg("shellcraft-engine", vec![]), pkg("shellcraft-ui-egui", vec![])];
        all.extend(extra);
        all
    }

    /// The four members of [`members`], with `deps` added to the member called `name`.
    fn with_deps(name: &str, deps: Vec<Dep>) -> Vec<Package> {
        let mut all = members(vec![]);
        for p in &mut all {
            if p.name == name {
                p.deps.extend(deps.iter().cloned());
            }
        }
        all
    }

    fn table() -> BTreeMap<String, Layer> {
        parse_table(TABLE).unwrap()
    }

    #[test]
    fn reads_the_crate_table() {
        let t = table();
        assert_eq!(t.len(), 4);
        assert_eq!(t["xtask"], Layer::Tooling);
        assert_eq!(t["craft-queue"], Layer::Level(0));
        assert_eq!(t["shellcraft-ui-egui"], Layer::Level(UI_LAYER));
    }

    #[test]
    fn the_repository_crate_table_parses_and_places_xtask() {
        let t = parse_table(include_str!("../../AGENTS.md")).unwrap();
        assert_eq!(t.get("xtask"), Some(&Layer::Tooling));
    }

    #[test]
    fn rejects_a_malformed_crate_table() {
        let bad = |from: &str, to: &str| parse_table(&TABLE.replace(from, to)).unwrap_err();
        assert!(bad("| Crate | Layer |", "| Crate | Tier |").contains("no crate table"));
        assert!(bad("|-------|-------|\n| `xtask`", "| `xtask`").contains("separator"));
        assert!(bad("| L4 |", "| L7 |").contains("not L0"));
        assert!(bad("| L4 |", "| 4 |").contains("not L0"));
        assert!(bad("| L4 |", "| L04 |").contains("not L0"));
        assert!(bad("`craft-queue`", "craft-queue").contains("backticks"));
        assert!(bad("`shellcraft-engine`", "`craft-queue`").contains("more than one row"));
        assert!(bad("| L6 |", "| L6 | extra |").contains("two cells"));
    }

    #[test]
    fn a_crate_depends_on_its_own_layer_or_a_lower_one() {
        let ok = members(vec![
            pkg("shellcraft-a", vec![dep("shellcraft-engine", Kind::Normal), dep("craft-queue", Kind::Dev)]),
            pkg("shellcraft-b", vec![dep("shellcraft-a", Kind::Normal)]),
        ]);
        let mut t = table();
        t.insert("shellcraft-a".into(), Layer::Level(4));
        t.insert("shellcraft-b".into(), Layer::Level(5));
        assert_eq!(check_layers(&t, &ok), Vec::<String>::new());

        let up = members(vec![pkg("shellcraft-a", vec![dep("shellcraft-ui-egui", Kind::Normal)])]);
        let problems = check_layers(&t, &up);
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].contains("a higher layer"), "{problems:?}");

        let dev_up = members(vec![pkg("craft-x", vec![dep("shellcraft-engine", Kind::Dev)])]);
        t.insert("craft-x".into(), Layer::Level(0));
        assert!(check_layers(&t, &dev_up)[0].contains("as a dev-dependency"));
    }

    #[test]
    fn no_layer_crate_depends_on_tooling_and_tooling_may_depend_on_any() {
        let t = table();
        let problems = check_layers(&t, &with_deps("shellcraft-engine", vec![dep("xtask", Kind::Normal)]));
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].contains("tooling crate"), "{problems:?}");
        assert_eq!(
            check_layers(&t, &with_deps("xtask", vec![dep("shellcraft-ui-egui", Kind::Normal), ext("egui", Kind::Normal)])),
            Vec::<String>::new()
        );
        let reached = [Reach { from: "xtask".into(), via: "shellcraft-ui-egui".into(), ui: Some("egui".into()), members: Vec::new() }];
        assert_eq!(check_reach(&t, &reached), Vec::<String>::new());
    }

    #[test]
    fn every_member_has_a_row() {
        let problems = check_layers(&table(), &members(vec![pkg("shellcraft-new", vec![])]));
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].contains("`shellcraft-new` has no row"), "{problems:?}");
    }

    #[test]
    fn only_the_top_layer_depends_on_ui_crates() {
        let t = table();
        for ui in ["egui", "eframe", "winit", "rfd", "egui_extras", "egui-wgpu"] {
            assert_eq!(check_layers(&t, &with_deps("shellcraft-ui-egui", vec![ext(ui, Kind::Normal)])), Vec::<String>::new(), "{ui}");
            let problems = check_layers(&t, &with_deps("shellcraft-engine", vec![ext(ui, Kind::Normal)]));
            assert_eq!(problems.len(), 1, "{ui}: {problems:?}");
            assert!(problems[0].contains("UI crate"), "{problems:?}");
        }
        assert_eq!(check_layers(&t, &with_deps("shellcraft-engine", vec![ext("eguilike", Kind::Normal)])), Vec::<String>::new());
        let mut with_widgets = with_deps("shellcraft-engine", vec![dep("egui-widgets", Kind::Normal)]);
        with_widgets.push(pkg("egui-widgets", vec![]));
        let mut t = t;
        t.insert("egui-widgets".into(), Layer::Level(4));
        let problems = check_layers(&t, &with_widgets);
        assert_eq!(problems.len(), 1, "a UI crate in the workspace: {problems:?}");
        assert!(problems[0].contains("the UI crate `egui-widgets`"), "{problems:?}");
    }

    /// A manifest that follows every craft crate rule.
    fn craft_manifest() -> String {
        format!(
            "[package]\nname = \"craft-queue\"\nversion = \"0.1.0\"\nedition = \"{CRAFT_LIBS_EDITION}\"\nrust-version = \"{CRAFT_LIBS_RUST_VERSION}\"\n\
             license = \"MIT OR Apache-2.0\"\ndescription = \"Bounded queues\"\n\n[lints]\nworkspace = true\n"
        )
    }

    fn craft(manifest: &str, readme: bool, deps: Vec<Dep>, names: &[&str], word_lines: &[(&str, usize)]) -> Vec<String> {
        let files = CrateFiles {
            names: names.iter().map(|n| (*n).to_owned()).collect(),
            word_lines: word_lines.iter().map(|(p, n)| ((*p).to_owned(), *n)).collect(),
            ..CrateFiles::default()
        };
        check_craft_crate(&pkg("craft-queue", deps), manifest, readme, &files)
    }

    #[test]
    fn a_craft_crate_sets_its_own_package_fields() {
        let manifest = craft_manifest();
        assert_eq!(craft(&manifest, true, vec![], &["src", "src/lib.rs"], &[]), Vec::<String>::new());
        let patch =
            manifest.replace(&format!("rust-version = \"{CRAFT_LIBS_RUST_VERSION}\""), &format!("rust-version = \"{CRAFT_LIBS_RUST_VERSION}.0\""));
        assert_eq!(craft(&patch, true, vec![], &[], &[]), Vec::<String>::new(), "rust-version with a .0 patch");
        for later in ["0.1.1", "0.2.0", "1.0.0-rc.1", "0.1.0+build.1", "0.2.0-rc.1+build.1"] {
            assert_eq!(craft(&manifest.replace("0.1.0", later), true, vec![], &[], &[]), Vec::<String>::new(), "{later}");
        }
        let edition = format!("edition = \"{CRAFT_LIBS_EDITION}\"");
        let rust_version = format!("rust-version = \"{CRAFT_LIBS_RUST_VERSION}\"");
        for (from, to, expect) in [
            ("version = \"0.1.0\"", "version.workspace = true", "inherits `version`"),
            ("version = \"0.1.0\"", "version = { workspace = true }", "inherits `version`"),
            (edition.as_str(), "", "no `edition`"),
            (edition.as_str(), "edition = \"2015\"", "`edition = \"2015\"`"),
            (rust_version.as_str(), "", "no `rust-version`"),
            (rust_version.as_str(), "rust-version = \"1.0\"", "`rust-version = \"1.0\"`"),
            (rust_version.as_str(), "rust-version.workspace = true", "inherits `rust-version`"),
            ("license = \"MIT OR Apache-2.0\"", "license.workspace = true", "inherits `license`"),
            ("license = \"MIT OR Apache-2.0\"", "license = \"MIT\"", "`license = \"MIT\"`"),
            ("description = \"Bounded queues\"", "description.workspace = true", "inherits `description`"),
            ("description = \"Bounded queues\"", "keywords = { workspace = true }", "inherits `keywords`"),
            ("version = \"0.1.0\"", "version = \"0.0.9\"", "starts at 0.1.0"),
            ("version = \"0.1.0\"", "version = \"0.1.0-rc.1\"", "starts at 0.1.0"),
            ("version = \"0.1.0\"", "version = \"1.2\"", "not a version"),
            ("version = \"0.1.0\"", "version = \"0.1.0-\"", "not a version"),
        ] {
            let problems = craft(&manifest.replace(from, to), true, vec![], &[], &[]);
            assert_eq!(problems.len(), 1, "{to}: {problems:?}");
            assert!(problems[0].contains(expect), "{to}: {problems:?}");
        }
    }

    #[test]
    fn a_craft_crate_has_no_repository_homepage_or_authors() {
        for line in
            ["repository.workspace = true", "repository = \"https://example.com/x\"", "homepage = \"https://example.com\"", "authors = [\"A\"]"]
        {
            let manifest = craft_manifest().replace("description", &format!("{line}\ndescription"));
            let problems = craft(&manifest, true, vec![], &[], &[]);
            assert_eq!(problems.len(), 1, "{line}: {problems:?}");
            assert!(problems[0].contains("a craft crate has none"), "{line}: {problems:?}");
        }
    }

    #[test]
    fn a_craft_crate_has_its_own_readme() {
        assert_eq!(craft(&craft_manifest(), false, vec![], &[], &[]), ["`craft-queue` has no README.md of its own"]);
    }

    #[test]
    fn a_craft_crate_depends_on_no_shellcraft_crate() {
        for kind in [Kind::Normal, Kind::Dev, Kind::Build] {
            let problems = craft(&craft_manifest(), true, vec![dep("shellcraft-proto", kind)], &[], &[]);
            assert_eq!(problems.len(), 1, "{problems:?}");
            assert!(problems[0].contains("depends on no shellcraft crate"), "{problems:?}");
        }
        assert_eq!(craft(&craft_manifest(), true, vec![ext("craft-atomic", Kind::Normal)], &[], &[]), Vec::<String>::new());
    }

    #[test]
    fn a_craft_crate_does_not_use_the_word_shellcraft() {
        let problems = craft(&craft_manifest(), true, vec![], &["README.md"], &[("README.md", 3)]);
        assert_eq!(problems, ["`craft-queue`: README.md:3 uses the word ShellCraft"]);
        let mut in_folder = pkg("craft-queue", vec![]);
        // A member outside the workspace root has no `dir`; its manifest path still names the folder.
        in_folder.dir = None;
        in_folder.manifest_path = "/elsewhere/shellcraft-queue/Cargo.toml".into();
        let problems = check_craft_crate(&in_folder, &craft_manifest(), true, &CrateFiles::default());
        assert_eq!(problems, ["`craft-queue`: its folder shellcraft-queue uses the word ShellCraft"]);
        for name in ["src/shellcraft.rs", "fixtures/ShellCraft.json", "shellcraft/data.bin"] {
            assert_eq!(craft(&craft_manifest(), true, vec![], &[name], &[]), [format!("`craft-queue`: the name {name} uses the word ShellCraft")]);
        }
    }

    #[test]
    fn the_word_scan_lists_every_name_and_reads_only_text_files() {
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.subsec_nanos());
        let dir = std::env::temp_dir().join(format!("xtask-layers-{}-{nanos}", std::process::id()));
        std::fs::create_dir(&dir).unwrap();
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::create_dir_all(dir.join("target/debug")).unwrap();
        std::fs::create_dir_all(dir.join("out/debug")).unwrap();
        std::fs::write(dir.join("src/lib.rs"), "//! Queues.\n//! Not for SHELLCRAFT.\n").unwrap();
        std::fs::write(dir.join("icon.png"), b"shellcraft").unwrap();
        std::fs::write(dir.join("GUIDE.MD"), "ShellCraft\n").unwrap();
        std::fs::write(dir.join("target/debug/x.rs"), "shellcraft").unwrap();
        std::fs::write(dir.join("out/debug/x.rs"), "shellcraft").unwrap();
        std::fs::create_dir_all(dir.join("src/target")).unwrap();
        std::fs::write(dir.join("src/target/mod.rs"), "// shellcraft\n").unwrap();
        // A tracked symbolic link that the checkout wrote as a plain file holding its target.
        std::fs::write(dir.join("README.md"), "../README.md").unwrap();
        // A tracked file in an output folder is source.
        std::fs::write(dir.join("out/kept.rs"), "// shellcraft\n").unwrap();
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(dir.join("src/lib.rs"), dir.join("src/link.rs")).unwrap();
            std::os::unix::fs::symlink(dir.join("src/target"), dir.join("src/alias")).unwrap();
        }
        let git = |path: &str, link: bool, tracked: bool| GitFile { path: path.into(), link, tracked, sparse: false, submodule: false };
        let listed = [
            git("GUIDE.MD", false, true),
            git("README.md", true, true),
            git("gone.rs", false, true),
            git("icon.png", false, true),
            git("out/debug/x.rs", false, false),
            git("out/kept.rs", false, true),
            git("src/alias/mod.rs", false, true),
            git("src/lib.rs", false, true),
            git("src/link.rs", false, false),
            git("src/target/mod.rs", false, true),
            git("target/debug/x.rs", false, false),
        ];
        let files = crate_files(&dir, &listed, &[dir.join("out"), dir.join("target")]).unwrap();
        // A `target` folder that is not Cargo's output folder is scanned like any other.
        let with_target = crate_files(&dir, &listed, &[dir.join("out")]).unwrap();
        // An output folder around the crate leaves every file of the crate in the scan.
        let around = crate_files(&dir, &listed, &[dir.clone(), std::env::temp_dir()]).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(with_target.word_lines.contains(&("target/debug/x.rs".to_owned(), 1)), "{with_target:?}");
        assert!(around.word_lines.contains(&("out/debug/x.rs".to_owned(), 1)), "{around:?}");
        let mut names = vec!["GUIDE.MD", "README.md", "icon.png", "out", "out/kept.rs", "src", "src/lib.rs", "src/target", "src/target/mod.rs"];
        if cfg!(unix) {
            names.insert(6, "src/alias");
            names.insert(8, "src/link.rs");
        }
        assert_eq!(files.names, names, "a listed file that is not on disk has no name");
        let links: &[&str] = if cfg!(unix) { &["README.md", "src/alias", "src/link.rs"] } else { &["README.md"] };
        assert_eq!(files.links, links);
        assert_eq!(
            files.word_lines,
            [("GUIDE.MD".to_owned(), 1), ("out/kept.rs".to_owned(), 1), ("src/lib.rs".to_owned(), 2), ("src/target/mod.rs".to_owned(), 1)]
        );
        let sparse = [GitFile { path: "docs/away.md".into(), link: false, tracked: true, sparse: true, submodule: false }];
        assert!(crate_files(Path::new("/nonexistent-crate"), &sparse, &[]).unwrap_err().contains("sparse checkout"));
        let submodule = [GitFile { path: "vendor/lib".into(), link: false, tracked: true, sparse: false, submodule: true }];
        let files = crate_files(Path::new("/nonexistent-crate"), &submodule, &[]).unwrap();
        assert_eq!(files.submodules, ["vendor/lib"]);
        let problems = check_craft_crate(&pkg("craft-queue", vec![]), &craft_manifest(), true, &files);
        assert_eq!(problems, ["`craft-queue`: vendor/lib is a Git submodule; a craft crate holds its own files"]);
    }

    #[test]
    fn lists_the_files_git_tracks_or_would_add() {
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.subsec_nanos());
        let dir = std::env::temp_dir().join(format!("xtask-layers-git-{}-{nanos}", std::process::id()));
        std::fs::create_dir(&dir).unwrap();
        // A Git template that excludes `new.rs`; `--template=` keeps it out of both repositories.
        let template = dir.join("template");
        std::fs::create_dir_all(template.join("info")).unwrap();
        std::fs::write(template.join("info/exclude"), "new.rs\n").unwrap();
        let git = |args: &[&str]| -> String {
            let mut c = std::process::Command::new("git");
            c.env_remove("GIT_DIR").env_remove("GIT_WORK_TREE").env_remove("GIT_INDEX_FILE").env_remove("GIT_CONFIG");
            c.env("GIT_TEMPLATE_DIR", &template).args(args).current_dir(&dir);
            let out = c.output().unwrap();
            assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
            String::from_utf8_lossy(&out.stdout).trim().to_owned()
        };
        git(&["init", "--quiet", "--template="]);
        // Only the ignore rules the fixture writes, not the caller's global ones.
        std::fs::write(dir.join("no-excludes"), "").unwrap();
        git(&["config", "--local", "core.excludesFile", &dir.join("no-excludes").to_string_lossy()]);
        std::fs::create_dir(dir.join("crate")).unwrap();
        std::fs::write(dir.join("crate/tracked.rs"), "").unwrap();
        std::fs::write(dir.join("crate/new.rs"), "").unwrap();
        std::fs::write(dir.join("crate/ignored.rs"), "").unwrap();
        std::fs::write(dir.join(".gitignore"), "ignored.rs\n").unwrap();
        std::fs::write(dir.join("outside.rs"), "").unwrap();
        // A tracked symbolic link, written as a plain file holding its target.
        std::fs::write(dir.join("crate/link.md"), "../outside.rs").unwrap();
        git(&["add", "crate/tracked.rs"]);
        let blob = git(&["hash-object", "-w", "crate/link.md"]);
        git(&["update-index", "--add", "--cacheinfo", &format!("120000,{blob},crate/link.md")]);
        // A submodule entry, and a nested repository not added yet.
        git(&["update-index", "--add", "--cacheinfo", &format!("160000,{blob},crate/sub")]);
        std::fs::create_dir(dir.join("crate/nested")).unwrap();
        git(&["-C", "crate/nested", "init", "--quiet", "--template="]);
        let no_hooks = format!("core.hooksPath={}", dir.join("no-hooks").display());
        let commit = ["-C", "crate/nested", "-c", "user.name=t", "-c", "user.email=t@example.com", "-c", "commit.gpgsign=false", "-c", &no_hooks];
        git(&[&commit[..], &["commit", "--quiet", "--allow-empty", "-m", "t"]].concat());
        let file = |path: &str, link: bool, tracked: bool| GitFile { path: path.into(), link, tracked, sparse: false, submodule: false };
        git(&["config", "--local", "core.symlinks", "true"]);
        // Untracked build output in an output folder inside the crate is not listed.
        std::fs::create_dir_all(dir.join("crate/out/debug")).unwrap();
        std::fs::write(dir.join("crate/out/debug/x.rs"), "").unwrap();
        let with_links = git_files(&dir.join("crate"), &[dir.join("crate/out")]);
        git(&["config", "--local", "core.symlinks", "false"]);
        let plain_links = git_files(&dir.join("crate"), &[dir.join("crate/out")]);
        std::fs::remove_dir_all(&dir).unwrap();
        let sub = GitFile { path: "sub".into(), link: false, tracked: true, sparse: false, submodule: true };
        let nested = GitFile { path: "nested".into(), link: false, tracked: false, sparse: false, submodule: true };
        let expected =
            |link| [file("link.md", link, true), nested.clone(), file("new.rs", false, false), sub.clone(), file("tracked.rs", false, true)];
        // The disk decides where links are written as links; the index mode where they are not.
        assert_eq!(with_links.unwrap(), expected(false));
        assert_eq!(plain_links.unwrap(), expected(true));
    }

    /// `craft-codec` reaches the member `shellcraft-engine` through the member `protocol`, and a
    /// `shellcraft-core` from git through the member `wire`.
    const CLOSURE_TREE: &str = "\
craft-codec v0.1.0 (/w/crates/codec)||
├── protocol v0.1.0 (/w/crates/protocol)||
│   └── shellcraft-engine v0.1.0 (/w/crates/engine)||
└── wire v0.1.0 (/w/crates/wire)||
    └── shellcraft-core v0.1.0 (https://example.com/shellcraft#0123abcd)||

protocol v0.1.0 (/w/crates/protocol)||
└── shellcraft-engine v0.1.0 (/w/crates/engine)|| (*)

wire v0.1.0 (/w/crates/wire)||
└── shellcraft-core v0.1.0 (https://example.com/shellcraft#0123abcd)|| (*)

shellcraft-engine v0.1.0 (/w/crates/engine)||
";

    fn closure_members(names: &[(&str, &str)]) -> Vec<Member> {
        names.iter().map(|(name, dir)| Member { name: (*name).into(), dir: format!("/w/crates/{dir}") }).collect()
    }

    #[test]
    fn a_craft_crate_reaches_no_shellcraft_crate_through_other_members() {
        let all = [("craft-codec", "codec"), ("protocol", "protocol"), ("wire", "wire"), ("shellcraft-engine", "engine")];
        assert_eq!(
            check_craft_paths(CLOSURE_TREE, &closure_members(&all)),
            [
                "`craft-codec` depends on `protocol`, which depends on `shellcraft-engine`; a craft crate depends on no shellcraft crate",
                "`craft-codec` depends on `wire`, which depends on `shellcraft-core`; a craft crate depends on no shellcraft crate",
            ]
        );
        assert_eq!(check_craft_paths(CLOSURE_TREE, &closure_members(&all[1..])), Vec::<String>::new(), "only craft crates");
        // A build of `wire` without the `shellcraft-engine` dependency is another node.
        let host_only = "\
craft-codec v0.1.0 (/w/crates/codec)||
[build-dependencies]
└── wire v0.1.0 (/w/crates/wire)||

wire v0.1.0 (/w/crates/wire)||
└── shellcraft-engine v0.1.0 (/w/crates/engine)||

shellcraft-engine v0.1.0 (/w/crates/engine)||
";
        assert_eq!(check_craft_paths(host_only, &closure_members(&all)), Vec::<String>::new());
        // Only the tests of `protocol` use `shellcraft-engine`; the tests of `craft-codec` itself count.
        let dev = "\
craft-codec v0.1.0 (/w/crates/codec)||
├── protocol v0.1.0 (/w/crates/protocol)||
[dev-dependencies]
└── testkit v1.0.0||
    └── shellcraft-engine v0.1.0 (/w/crates/engine)||

protocol v0.1.0 (/w/crates/protocol)||
[dev-dependencies]
└── shellcraft-engine v0.1.0 (/w/crates/engine)|| (*)

shellcraft-engine v0.1.0 (/w/crates/engine)||
";
        assert_eq!(
            check_craft_paths(dev, &closure_members(&all)),
            ["`craft-codec` depends on `testkit`, which depends on `shellcraft-engine`; a craft crate depends on no shellcraft crate"]
        );
    }

    #[test]
    fn a_path_that_leaves_the_crate_folder_is_outside() {
        let base = Path::new("/w/crates/q");
        assert!(stays_inside(base, Path::new("README.md")));
        assert!(stays_inside(base, Path::new("./docs/../README.md")));
        assert!(stays_inside(base, Path::new("/w/crates/q/src/lib.rs")));
        assert!(!stays_inside(base, Path::new("../q/README.md")), "leaves and comes back");
        assert!(!stays_inside(base, Path::new("../../README.md")));
        assert!(!stays_inside(base, Path::new("/w/crates/qq/src/lib.rs")));
        assert!(!stays_inside(base, Path::new("/w/crates/q/../x/lib.rs")));
    }

    #[test]
    fn a_crate_folder_is_no_link_or_submodule() {
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.subsec_nanos());
        let root = std::env::temp_dir().join(format!("xtask-layers-folder-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(root.join("crates/plain")).unwrap();
        std::fs::create_dir_all(root.join("crates/sub")).unwrap();
        // A checked-out submodule holds a `.git` file.
        std::fs::write(root.join("crates/sub/.git"), "gitdir: ../../.git/modules/sub\n").unwrap();
        std::fs::write(root.join(".git"), "").unwrap();
        let plain = folder_problems("craft-plain", &root, &root.join("crates/plain"));
        let sub = folder_problems("craft-sub", &root, &root.join("crates/sub"));
        #[cfg(unix)]
        let linked = {
            std::os::unix::fs::symlink(root.join("crates/plain"), root.join("crates/alias")).unwrap();
            folder_problems("craft-alias", &root, &root.join("crates/alias"))
        };
        std::fs::remove_dir_all(&root).unwrap();
        assert_eq!(plain, Vec::<String>::new(), "the workspace root's own `.git` does not count");
        assert_eq!(sub.len(), 1, "{sub:?}");
        assert!(sub[0].contains("is a Git submodule"), "{sub:?}");
        #[cfg(unix)]
        {
            assert_eq!(linked.len(), 1, "{linked:?}");
            assert!(linked[0].contains("is a symbolic link"), "{linked:?}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_source_behind_a_symbolic_link_is_outside() {
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.subsec_nanos());
        let dir = std::env::temp_dir().join(format!("xtask-layers-resolve-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(dir.join("crate/src")).unwrap();
        std::fs::create_dir_all(dir.join("shared")).unwrap();
        std::fs::write(dir.join("crate/src/lib.rs"), "").unwrap();
        std::fs::write(dir.join("shared/guide.md"), "").unwrap();
        std::os::unix::fs::symlink(dir.join("shared"), dir.join("crate/docs")).unwrap();
        let sources = ["src/lib.rs".to_owned(), "docs/guide.md".to_owned(), "missing.md".to_owned()];
        let outside = resolved_outside(&dir.join("crate"), &sources);
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(outside.unwrap(), ["docs/guide.md"]);
    }

    #[test]
    fn a_craft_crate_uses_no_file_outside_its_folder() {
        let mut p = pkg("craft-queue", vec![]);
        p.files_outside = vec!["/w/shared/lib.rs".into()];
        let problems = check_craft_crate(&p, &craft_manifest(), true, &CrateFiles::default());
        assert_eq!(problems, ["`craft-queue` uses /w/shared/lib.rs, which is outside its folder; a craft crate holds its own files"]);
    }

    #[test]
    fn a_craft_crate_holds_no_symbolic_link() {
        let files = CrateFiles { links: vec!["README.md".into()], ..CrateFiles::default() };
        let problems = check_craft_crate(&pkg("craft-queue", vec![]), &craft_manifest(), true, &files);
        assert_eq!(problems, ["`craft-queue`: README.md is a symbolic link; a craft crate holds its own files"]);
    }

    #[test]
    fn reads_cargo_output_folders() {
        assert_eq!(output_dirs(br#"{"target_directory":"/w/target"}"#).unwrap(), [PathBuf::from("/w/target")]);
        assert_eq!(
            output_dirs(br#"{"target_directory":"/w/target","build_directory":"/b"}"#).unwrap(),
            [PathBuf::from("/w/target"), PathBuf::from("/b")]
        );
        assert!(output_dirs(b"{}").is_err());
    }

    #[test]
    fn the_word_scan_finds_every_line_in_any_case() {
        let text = b"one\nShellCraft two\nthree\nend shellcraft";
        assert_eq!(word_lines(&text[..]).unwrap(), [2, 4]);
        assert_eq!(word_lines(&b"shell craft\nShellCrafts"[..]).unwrap(), [2]);
        assert_eq!(word_lines(&b""[..]).unwrap(), Vec::<usize>::new());
        assert_eq!(word_lines(&b"\xff\xfe shellcraft \xff"[..]).unwrap(), [1], "bytes that are not UTF-8 or UTF-16");
        assert_eq!(word_lines(&b"\xfa\xfe shellcraft \xff"[..]).unwrap(), [1], "bytes that are not UTF-8");
        let utf16 = |text: &str, le: bool| -> Vec<u8> {
            let mut out = if le { vec![0xFF, 0xFE] } else { vec![0xFE, 0xFF] };
            out.extend(text.encode_utf16().flat_map(|u| if le { u.to_le_bytes() } else { u.to_be_bytes() }));
            out
        };
        assert_eq!(word_lines(&utf16("one\r\nUsed by ShellCraft.\r\n", true)[..]).unwrap(), [2], "UTF-16LE");
        assert_eq!(word_lines(&utf16("ShellCraft\n", false)[..]).unwrap(), [1], "UTF-16BE");
        for le in [true, false] {
            let without_bom = utf16("one\nShellCraft", le).split_off(2);
            assert_eq!(word_lines(&without_bom[..]).unwrap(), [2], "UTF-16 without a byte order mark, little endian {le}");
        }
        let le = utf16("one\nShellCraft\n", true);
        assert_eq!(word_lines(std::io::BufReader::with_capacity(1, &le[..])).unwrap(), [2], "read one byte at a time");
        let mut odd = le.clone();
        odd.push(b'x');
        assert_eq!(word_lines(&odd[..]).unwrap(), [2], "a trailing byte");
        let mut surrogate = vec![0xFF, 0xFE, 0x00, 0xD8];
        surrogate.extend(b"ShellCraft");
        assert_eq!(word_lines(&surrogate[..]).unwrap(), [1], "an unpaired surrogate and ASCII bytes");
    }

    const INHERITING: &str = r#"[package]
name = "shellcraft-engine"
version.workspace = true
edition = { workspace = true }
rust-version.workspace = true
license.workspace = true
repository.workspace = true
"#;

    #[test]
    fn other_crates_inherit_their_package_fields() {
        let p = pkg("shellcraft-engine", vec![]);
        assert_eq!(check_inherited(&p, INHERITING), Vec::<String>::new());
        for field in INHERITED_FIELDS {
            let manifest: String = INHERITING
                .lines()
                .filter(|l| !l.starts_with(&format!("{field}.")) && !l.starts_with(&format!("{field} =")))
                .collect::<Vec<_>>()
                .join("\n");
            assert_eq!(check_inherited(&p, &manifest), [format!("`shellcraft-engine` does not inherit `{field}` from [workspace.package]")]);
        }
        let own = INHERITING.replace("\nversion.workspace = true", "\nversion = \"0.2.0\"");
        assert_eq!(check_inherited(&p, &own), ["`shellcraft-engine` does not inherit `version` from [workspace.package]"]);
        assert!(check_inherited(&p, "not toml = = 1")[0].contains("not valid TOML"));
    }

    /// `cargo metadata` output for two members: `engine`, whose `queue` dependency a `[patch]` points
    /// at the member `craft-queue`, and `craft-queue`, outside the root. `ui` depends on `egui`
    /// through `widgets`, `helper` depends on `craft-queue`, and the registry `xtask` only shares a
    /// member's name.
    const METADATA: &[u8] = br#"{"workspace_root":"/w","workspace_members":["engine","queue"],
        "packages":[
            {"name":"shellcraft-engine","id":"engine","manifest_path":"/w/crates/engine/Cargo.toml","readme":"../../README.md",
             "targets":[{"src_path":"/w/crates/engine/src/lib.rs"},{"src_path":"/w/crates/engine-shared/main.rs"}]},
            {"name":"craft-queue","id":"queue","manifest_path":"/other/q/Cargo.toml","readme":"./docs/../README.md",
             "targets":[{"src_path":"/other/q/src/lib.rs"}]},
            {"name":"egui","id":"egui","manifest_path":"/r/egui/Cargo.toml"},
            {"name":"craft-ui","id":"ui","manifest_path":"/r/ui/Cargo.toml"},
            {"name":"widgets","id":"widgets","manifest_path":"/r/widgets/Cargo.toml"},
            {"name":"xtask","id":"reg-xtask","manifest_path":"/r/xtask/Cargo.toml"},
            {"name":"cc","id":"cc","manifest_path":"/r/cc/Cargo.toml"},
            {"name":"helper","id":"helper","manifest_path":"/r/helper/Cargo.toml"}],
        "resolve":{"nodes":[
            {"id":"engine","deps":[
                {"name":"queue","pkg":"queue","dep_kinds":[{"kind":null,"target":null},{"kind":"dev","target":null}]},
                {"name":"craft_ui","pkg":"ui","dep_kinds":[{"kind":null,"target":"cfg(unix)"},{"kind":null,"target":"cfg(windows)"}]},
                {"name":"xtask","pkg":"reg-xtask","dep_kinds":[{"kind":"dev","target":null}]},
                {"name":"cc","pkg":"cc","dep_kinds":[{"kind":"build","target":null}]},
                {"name":"helper","pkg":"helper","dep_kinds":[{"kind":null,"target":null}]}]},
            {"id":"helper","deps":[{"name":"queue","pkg":"queue","dep_kinds":[{"kind":null,"target":null}]}]},
            {"id":"queue","deps":[]},
            {"id":"ui","deps":[{"name":"widgets","pkg":"widgets","dep_kinds":[{"kind":null,"target":null}]}]},
            {"id":"widgets","deps":[{"name":"egui","pkg":"egui","dep_kinds":[{"kind":null,"target":null}]}]},
            {"id":"egui","deps":[]},
            {"id":"reg-xtask","deps":[]},
            {"id":"cc","deps":[]}]},"version":1}"#;

    #[test]
    fn reads_members_and_their_resolved_dependencies() {
        let list = packages(METADATA).unwrap();
        assert_eq!(list.len(), 2);
        let p = &list[0];
        assert_eq!(p.name, "shellcraft-engine");
        assert_eq!(p.dir.as_deref(), Some("crates/engine"));
        assert_eq!(
            p.deps,
            [
                dep("craft-queue", Kind::Normal),
                dep("craft-queue", Kind::Dev),
                ext("craft-ui", Kind::Normal),
                ext("xtask", Kind::Dev),
                ext("cc", Kind::Build),
                ext("helper", Kind::Normal)
            ]
        );
        assert_eq!(p.files_outside, ["/w/crates/engine-shared/main.rs", "../../README.md"]);
        assert_eq!(list[1].dir, None);
        assert_eq!(list[1].files_outside, Vec::<String>::new());
        let no_resolve = String::from_utf8_lossy(METADATA).replace("\"resolve\"", "\"resolved\"");
        assert!(packages(no_resolve.as_bytes()).unwrap_err().contains("resolved dependency graph"));
        assert!(packages(br#"{"packages":[]}"#).is_err());
        assert!(packages(b"[]").is_err());
    }

    #[test]
    fn a_namesake_of_a_member_is_not_a_member_edge() {
        let list = packages(METADATA).unwrap();
        let mut t = table();
        t.insert("craft-queue".into(), Layer::Level(5));
        // The registry `xtask` is not the tooling member; the patched `craft-queue` is a member.
        let problems = check_layers(&t, &list);
        assert_eq!(problems.iter().filter(|p| p.contains("`craft-queue` (L5), a higher layer")).count(), 2, "{problems:?}");
        assert!(!problems.iter().any(|p| p.contains("tooling")), "{problems:?}");
    }

    /// [`package_tree`] output with `normal,build,dev` edges for four members.
    /// `shellcraft-engine` reaches `egui` through `craft-ui` and the member `craft-queue` through
    /// `helper`, and uses a registry package that only shares `craft-queue`'s name. `craft-queue`
    /// reaches `shellcraft-proto` through `neutral`, and a `shellcraft-core` from git through
    /// `gitdep`. `painter` depends on `egui` only in the host build that `shellcraft-ui-egui`'s
    /// build script uses; the build for the target that `shellcraft-engine` uses has no
    /// dependencies. The host build of `shellcraft-proto` below `shellcraft-ui-egui` reaches `egui`
    /// through `uikit`; its own tree is clean.
    const REACH_TREE: &str = "\
shellcraft-engine v0.1.0 (/w/crates/engine)||
├── craft-queue v0.1.0 (/w/crates/queue)||
├── craft-ui v0.1.0|default|
│   └── widgets v0.1.0||
│       └── egui v0.33.0|default|
├── helper v0.1.0||
│   └── craft-queue v0.1.0 (/w/crates/queue)|| (*)
├── painter v1.0.0||
└── craft-queue v9.0.0||

craft-queue v0.1.0 (/w/crates/queue)||
├── neutral v1.0.0||
│   └── shellcraft-proto v0.1.0 (/w/crates/proto)||
└── gitdep v1.0.0||
    └── shellcraft-core v0.1.0 (https://example.com/shellcraft#0123abcd)||

shellcraft-proto v0.1.0 (/w/crates/proto)||

shellcraft-ui-egui v0.1.0 (/w/crates/ui)||
└── egui v0.33.0|default| (*)
[build-dependencies]
├── painter v1.0.0||
│   └── egui v0.33.0|default|
└── shellcraft-proto v0.1.0 (/w/crates/proto)||
    └── uikit v1.0.0||
        └── egui v0.33.0|default| (*)
";

    fn reach_members() -> Vec<Member> {
        [("shellcraft-engine", "engine"), ("craft-queue", "queue"), ("shellcraft-proto", "proto"), ("shellcraft-ui-egui", "ui")]
            .map(|(name, dir)| Member { name: name.into(), dir: format!("/w/crates/{dir}") })
            .to_vec()
    }

    fn reach_table() -> BTreeMap<String, Layer> {
        let mut t = table();
        t.insert("shellcraft-proto".into(), Layer::Level(0));
        t
    }

    #[test]
    fn reads_what_each_member_reaches_through_other_packages() {
        let reach = |from: &str, via: &str, ui: Option<&str>, members: &[&str]| Reach {
            from: from.into(),
            via: via.into(),
            ui: ui.map(str::to_owned),
            members: members.iter().map(|m| (*m).to_owned()).collect(),
        };
        assert_eq!(
            reaches(REACH_TREE, &reach_members()).unwrap(),
            [
                reach("shellcraft-engine", "craft-queue", None, &["shellcraft-proto"]),
                reach("shellcraft-engine", "craft-ui", Some("egui"), &[]),
                reach("shellcraft-engine", "helper", None, &["craft-queue", "shellcraft-proto"]),
                reach("craft-queue", "neutral", None, &["shellcraft-proto"]),
                reach("shellcraft-ui-egui", "painter", Some("egui"), &[]),
                reach("shellcraft-ui-egui", "shellcraft-proto", Some("egui"), &[]),
            ]
        );
    }

    #[test]
    fn records_members_reached_through_other_members() {
        let tree = "\
shellcraft-a v0.1.0 (/w/crates/a)||
└── lib1 v1.0.0||
    └── shellcraft-b v0.1.0 (/w/crates/b)||
        └── shellcraft-c v0.1.0 (/w/crates/c)||
";
        let members = ["a", "b", "c"].map(|n| Member { name: format!("shellcraft-{n}"), dir: format!("/w/crates/{n}") });
        let reached = reaches(tree, &members).unwrap();
        assert_eq!(reached.len(), 1);
        assert_eq!(reached[0].members, ["shellcraft-b", "shellcraft-c"]);
    }

    #[test]
    fn tells_proc_macro_crates_and_members_apart() {
        assert!(is_proc_macro("enumn v0.1.14 (proc-macro)||"));
        assert!(is_proc_macro("macros v0.1.0 (proc-macro) (/w/crates/macros)||"));
        assert!(!is_proc_macro("tool v0.1.0 (/w/proc-macro)||"));
        assert!(!is_proc_macro("serde v1.0.228|default,proc-macro|"));
        let member = Member { name: "macros".into(), dir: "/w/crates/macros".into() };
        assert!(member.is("macros v0.1.0 (proc-macro) (/w/crates/macros)||"));
        let tree = "macros v0.1.0 (proc-macro) (/w/crates/macros)||\n└── quote v1.0.47|default|\n";
        assert_eq!(Tree::parse(tree).roots, [Node { label: "macros v0.1.0 (proc-macro) (/w/crates/macros)||", host: true }]);
    }

    #[test]
    fn a_root_that_is_not_a_member_is_an_error() {
        let members: Vec<Member> = reach_members().into_iter().filter(|m| m.name != "shellcraft-proto").collect();
        assert!(reaches(REACH_TREE, &members).unwrap_err().contains("shellcraft-proto v0.1.0"));
        // Styled output: the member names no longer match.
        let styled = REACH_TREE.replacen("shellcraft-engine", "\u{1b}[1mshellcraft-engine\u{1b}[0m", 1);
        assert!(reaches(&styled, &reach_members()).unwrap_err().contains("shellcraft-engine"));
        assert!(reaches("", &reach_members()).unwrap_err().contains("no package"));
    }

    #[test]
    fn a_crate_below_the_top_layer_reaches_no_ui_crate_through_other_packages() {
        let mut t = reach_table();
        let problems = check_reach(&t, &reaches(REACH_TREE, &reach_members()).unwrap());
        assert!(
            problems.contains(&"`shellcraft-engine` (L4) depends on `craft-ui`, which depends on the UI crate `egui`; only L6 crates may".to_owned()),
            "{problems:?}"
        );
        assert!(!problems.iter().any(|p| p.contains("painter")), "the UI crate comes only with a feature L4 does not use: {problems:?}");
        t.insert("shellcraft-engine".into(), Layer::Level(UI_LAYER));
        let problems = check_reach(&t, &reaches(REACH_TREE, &reach_members()).unwrap());
        assert!(!problems.iter().any(|p| p.contains("`shellcraft-engine`") && p.contains("UI crate")), "{problems:?}");
        assert!(!problems.iter().any(|p| p.contains("`uikit`")), "a member below the root is checked in its own tree: {problems:?}");
    }

    #[test]
    fn a_path_back_into_the_workspace_follows_the_layers() {
        let mut t = reach_table();
        let tree = reaches(REACH_TREE, &reach_members()).unwrap();
        assert!(!check_reach(&t, &tree).iter().any(|p| p.contains("`helper`")));
        t.insert("craft-queue".into(), Layer::Level(5));
        assert!(
            check_reach(&t, &tree)
                .contains(&"`shellcraft-engine` (L4) depends on `helper`, which depends on `craft-queue` (L5), a higher layer".to_owned())
        );
        t.insert("craft-queue".into(), Layer::Tooling);
        assert!(
            check_reach(&t, &tree)
                .contains(&"`shellcraft-engine` (L4) depends on `helper`, which depends on the tooling crate `craft-queue`".to_owned())
        );
    }

    #[test]
    fn a_craft_crate_reaches_no_shellcraft_crate_through_other_packages() {
        assert_eq!(
            check_craft_paths(REACH_TREE, &reach_members()),
            [
                "`craft-queue` depends on `neutral`, which depends on `shellcraft-proto`; a craft crate depends on no shellcraft crate",
                "`craft-queue` depends on `gitdep`, which depends on `shellcraft-core`; a craft crate depends on no shellcraft crate",
            ]
        );
    }

    /// [`package_tree`] output with two roots. The daemon, listed first, links `tokio` and
    /// `craft-chat`; the GUI links `craft-chat`, whose dependencies the daemon's root already showed,
    /// and `tokio-free`, and builds `russh` for its build script and `rusqlite` for its tests.
    const TREE: &str = "\
shellcraft-daemon v0.1.0 (/w/apps/shellcraftd)||
├── tokio v1.48.0|default,rt|
└── craft-chat v0.1.0 (https://github.com/storytold/craft-libs?rev=c42c34b#c42c34bf)|default,stream|
    └── craft-keystore v0.1.0 (https://github.com/storytold/craft-libs?rev=c42c34b#c42c34bf)||

shellcraft v0.1.0 (/w/apps/my | folder/shellcraft)|default|
├── craft-chat v0.1.0 (https://github.com/storytold/craft-libs?rev=c42c34b#c42c34bf)|default,stream| (*)
└── tokio-free v0.1.0||
[build-dependencies]
└── russh v0.54.0||
[dev-dependencies]
└── rusqlite v0.37.0||
";

    fn gui_links(tree: &str) -> Vec<String> {
        let (_, banned) = MUST_NOT_LINK[0];
        check_links("shellcraft", banned, &linked(tree, "shellcraft").unwrap())
    }

    #[test]
    fn reads_what_the_gui_links() {
        let l = linked(TREE, "shellcraft").unwrap();
        assert_eq!(l.keys().map(String::as_str).collect::<Vec<_>>(), ["craft-chat", "craft-keystore", "shellcraft", "tokio-free"]);
        assert!(l["craft-chat"].contains("stream"));
        assert!(linked(TREE, "tokio").is_none(), "only roots are apps");
        assert!(linked(TREE, "shellcraft-cli").is_none());
    }

    #[test]
    fn the_gui_links_no_banned_crate_or_feature() {
        assert_eq!(gui_links(TREE), Vec::<String>::new());

        let with_tokio = TREE.replace("    └── craft-keystore", "    ├── tokio v1.48.0|default,rt| (*)\n    └── craft-keystore");
        assert_eq!(gui_links(&with_tokio), ["`shellcraft` links `tokio`, which it must not link"]);

        let with_http = TREE.replace("default,stream", "default,http,stream");
        assert_eq!(gui_links(&with_http), ["`shellcraft` links `craft-chat` with its `http` feature, which it must not link"]);
    }
}
