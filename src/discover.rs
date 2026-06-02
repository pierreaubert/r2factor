//! Walk a directory tree, discover all `.rs` files, parse them, and build a
//! global item index plus a unified reference graph.
//!
//! This is the foundation for whole-codebase refactoring. Every other phase
//! (spec matching, global planning, cross-file rewrites) depends on the data
//! produced here.

use crate::graph;
use crate::item::{ItemId, ItemKind, ItemVis, ParsedItem, parse_file};
use crate::tokensave::Tokensave;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};

pub type GlobalItemId = usize;

// ---------------------------------------------------------------------------
// Cargo metadata
// ---------------------------------------------------------------------------

/// Parsed subset of `cargo metadata --format-version 1` output.
#[derive(Debug, Clone, Deserialize)]
struct CargoMetadata {
    packages: Vec<CargoPackage>,
    workspace_root: String,
}

#[derive(Debug, Clone, Deserialize)]
struct CargoPackage {
    name: String,
    manifest_path: String,
    #[serde(default)]
    targets: Vec<CargoTarget>,
}

#[derive(Debug, Clone, Deserialize)]
struct CargoTarget {
    name: String,
    #[serde(default)]
    kind: Vec<String>,
    src_path: String,
}

/// Information about the Cargo workspace / crate that was discovered.
#[derive(Debug, Clone, Serialize)]
pub struct CargoInfo {
    pub workspace_root: PathBuf,
    pub packages: Vec<CargoPackageInfo>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CargoPackageInfo {
    pub name: String,
    pub manifest_dir: PathBuf,
    pub targets: Vec<CargoTargetInfo>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CargoTargetInfo {
    pub name: String,
    pub kind: String,
    pub src_path: PathBuf,
}

/// One file that has been discovered and parsed.
#[derive(Debug, Clone, Serialize)]
pub struct DiscoveredFile {
    pub path: PathBuf,
    #[serde(skip)]
    pub source: String,
    pub items: Vec<ParsedItem>,
}

/// A single item lifted from a file into the global namespace.
#[derive(Debug, Clone, Serialize)]
pub struct GlobalItem {
    pub id: GlobalItemId,
    pub file_id: usize,
    pub local_id: ItemId,
    pub name: String,
    pub kind: ItemKind,
    pub vis: ItemVis,
    pub line_start: usize,
    pub line_end: usize,
}

/// Reference edge: `from` item references `to` item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct RefEdge {
    pub from: GlobalItemId,
    pub to: GlobalItemId,
}

/// Result of walking a directory tree.
#[derive(Debug, Clone, Serialize)]
pub struct Discovery {
    pub root: PathBuf,
    pub files: Vec<DiscoveredFile>,
    pub items: Vec<GlobalItem>,
    pub edges: Vec<RefEdge>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cargo: Option<CargoInfo>,
}

impl Discovery {
    /// Look up a global item by its stable id.
    pub fn item(&self, id: GlobalItemId) -> Option<&GlobalItem> {
        self.items.get(id)
    }

    /// All items whose `name` matches (there may be many because of
    /// module-level scoping).
    pub fn items_by_name(&self, name: &str) -> Vec<&GlobalItem> {
        self.items.iter().filter(|i| i.name == name).collect()
    }

    /// All items that live in a given file path.
    pub fn items_in_file(&self, path: &Path) -> Vec<&GlobalItem> {
        self.files
            .iter()
            .position(|f| f.path == path)
            .map(|fid| self.items.iter().filter(|i| i.file_id == fid).collect())
            .unwrap_or_default()
    }

    /// Convenience: map from item name to the set of global ids that share
    /// that name.
    pub fn name_index(&self) -> BTreeMap<&str, BTreeSet<GlobalItemId>> {
        let mut m: BTreeMap<&str, BTreeSet<GlobalItemId>> = BTreeMap::new();
        for it in &self.items {
            if !it.name.is_empty() {
                m.entry(&it.name).or_default().insert(it.id);
            }
        }
        m
    }
}

// ---------------------------------------------------------------------------
// Human report

/// Human-readable summary of a discovery run.
pub fn human_report(disc: &Discovery) -> String {
    let mut out = String::new();
    if let Some(cargo) = &disc.cargo {
        out.push_str(&format!(
            "Cargo workspace: {} ({} package(s))\n",
            cargo.workspace_root.display(),
            cargo.packages.len()
        ));
        for pkg in &cargo.packages {
            out.push_str(&format!(
                "  - {} @ {}\n",
                pkg.name,
                pkg.manifest_dir.display()
            ));
        }
        out.push('\n');
    }
    out.push_str(&format!(
        "Discovered {} file(s), {} item(s), {} edge(s)\n",
        disc.files.len(),
        disc.items.len(),
        disc.edges.len()
    ));
    for file in &disc.files {
        let file_items: Vec<&GlobalItem> = disc.items_in_file(&file.path);
        out.push_str(&format!(
            "\n{} ({} items)\n",
            file.path.display(),
            file_items.len()
        ));
        for it in file_items {
            out.push_str(&format!(
                "  {:4} {:?} {:12} L{}-{}",
                it.id,
                it.vis,
                format!("{:?}", it.kind),
                it.line_start,
                it.line_end
            ));
            if !it.name.is_empty() {
                out.push_str(&format!("  {}", it.name));
            }
            out.push('\n');
        }
    }
    if !disc.edges.is_empty() {
        out.push_str("\nTop cross-file edges (first 20):\n");
        for e in disc.edges.iter().take(20) {
            let from = disc.item(e.from).map(|i| i.name.as_str()).unwrap_or("?");
            let to = disc.item(e.to).map(|i| i.name.as_str()).unwrap_or("?");
            out.push_str(&format!("  {} -> {}\n", from, to));
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Main entry point

/// Discover every `.rs` file under `root` (except well-known ignored dirs),
/// parse them, and build a global index + reference graph.
///
/// When `use_tokensave` is true and a `.tokensave/` database is found, cross-
/// file semantic edges are folded into the graph.
pub fn discover_dir(root: &Path, use_tokensave: bool) -> anyhow::Result<Discovery> {
    let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let cargo = discover_cargo(&root);
    let paths = collect_rust_files(&root)?;

    let mut files: Vec<DiscoveredFile> = Vec::with_capacity(paths.len());
    let mut items: Vec<GlobalItem> = Vec::new();

    for (file_id, path) in paths.into_iter().enumerate() {
        let source = std::fs::read_to_string(&path)?;
        let mut parsed = parse_file(&source)?;
        graph::annotate_refs(&mut parsed, None);

        for it in &parsed {
            let gid = items.len();
            items.push(GlobalItem {
                id: gid,
                file_id,
                local_id: it.id,
                name: it.name.clone(),
                kind: it.kind.clone(),
                vis: it.vis.clone(),
                line_start: it.line_start,
                line_end: it.line_end,
            });
        }

        files.push(DiscoveredFile {
            path,
            source,
            items: parsed,
        });
    }

    let mut edges = build_edges(&files, &items);

    if use_tokensave {
        if let Err(e) = try_enrich_with_tokensave(&files, &items, &mut edges) {
            eprintln!("[discover] tokensave enrichment failed: {e}");
        }
    }

    Ok(Discovery {
        root,
        files,
        items,
        edges,
        cargo,
    })
}

// ---------------------------------------------------------------------------
// Cargo workspace discovery

fn discover_cargo(start: &Path) -> Option<CargoInfo> {
    let manifest = find_manifest(start)?;
    let manifest_dir = manifest
        .canonicalize()
        .ok()?
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
    let output = std::process::Command::new("cargo")
        .arg("metadata")
        .arg("--format-version")
        .arg("1")
        .arg("--no-deps")
        .current_dir(&manifest_dir)
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let meta: CargoMetadata = serde_json::from_slice(&output.stdout).ok()?;

    let packages = meta
        .packages
        .into_iter()
        .map(|p| {
            let manifest_dir = PathBuf::from(&p.manifest_path)
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_default();
            let targets = p
                .targets
                .into_iter()
                .map(|t| CargoTargetInfo {
                    name: t.name,
                    kind: t.kind.first().cloned().unwrap_or_else(|| "lib".to_string()),
                    src_path: PathBuf::from(t.src_path),
                })
                .collect();
            CargoPackageInfo {
                name: p.name,
                manifest_dir,
                targets,
            }
        })
        .collect();

    Some(CargoInfo {
        workspace_root: PathBuf::from(meta.workspace_root),
        packages,
    })
}

fn find_manifest(start: &Path) -> Option<PathBuf> {
    let mut cur = if start.is_file() {
        start.parent()?.to_path_buf()
    } else {
        start.to_path_buf()
    };
    // Ensure we have an absolute base so pop() eventually reaches root,
    // not the empty string.
    if cur.as_os_str().is_empty() || cur.as_os_str() == "." {
        cur = std::env::current_dir().ok()?;
    }
    loop {
        let candidate = cur.join("Cargo.toml");
        if candidate.is_file() {
            return Some(candidate);
        }
        if !cur.pop() {
            return None;
        }
    }
}

// ---------------------------------------------------------------------------
// Directory walking

fn collect_rust_files(root: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];

    while let Some(dir) = stack.pop() {
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(_) => continue,
        };

        for entry in entries.flatten() {
            let path = entry.path();
            let ft = entry.file_type();
            let is_dir = ft.as_ref().map(|t| t.is_dir()).unwrap_or(false);
            let is_file = ft.as_ref().map(|t| t.is_file()).unwrap_or(false);

            if is_dir {
                if let Some(name) = path.file_name().and_then(|s| s.to_str()) {
                    if should_skip_dir(name) {
                        continue;
                    }
                }
                stack.push(path);
            } else if is_file {
                if path.extension().and_then(|s| s.to_str()) == Some("rs") {
                    out.push(path);
                }
            }
        }
    }

    out.sort();
    Ok(out)
}

fn should_skip_dir(name: &str) -> bool {
    matches!(
        name,
        "target" | ".git" | ".tokensave" | "node_modules" | ".idea" | ".vscode" | "dist" | "build"
    )
}

// ---------------------------------------------------------------------------
// Edge building (intra-file + heuristic cross-file)

fn build_edges(files: &[DiscoveredFile], items: &[GlobalItem]) -> Vec<RefEdge> {
    let mut edges = Vec::new();

    // Precompute lookup so we never confuse local ItemId with GlobalItemId.
    let global_id_of: BTreeMap<(usize, ItemId), GlobalItemId> = items
        .iter()
        .map(|g| ((g.file_id, g.local_id), g.id))
        .collect();

    // 1. Intra-file edges (from ParsedItem.refs)
    for file in files {
        let file_id = files
            .iter()
            .position(|f| f.path == file.path)
            .unwrap_or(usize::MAX);

        let file_items: BTreeMap<ItemId, GlobalItemId> = items
            .iter()
            .filter(|g| g.file_id == file_id)
            .map(|g| (g.local_id, g.id))
            .collect();

        let local_name_to_gid: BTreeMap<&str, GlobalItemId> = items
            .iter()
            .filter(|g| g.file_id == file_id && !g.name.is_empty())
            .map(|g| (g.name.as_str(), g.id))
            .collect();

        for it in &file.items {
            let Some(&from) = file_items.get(&it.id) else {
                continue;
            };
            for r in &it.refs {
                if let Some(&to) = local_name_to_gid.get(r.as_str()) {
                    if from != to {
                        edges.push(RefEdge { from, to });
                    }
                }
            }
        }
    }

    // 2. Cross-file heuristic edges
    let global_names: HashSet<String> = items
        .iter()
        .filter(|i| !i.name.is_empty())
        .map(|i| i.name.clone())
        .collect();

    let name_index = {
        let mut m: BTreeMap<&str, Vec<GlobalItemId>> = BTreeMap::new();
        for it in items {
            if !it.name.is_empty() {
                m.entry(&it.name).or_default().push(it.id);
            }
        }
        m
    };

    for file in files {
        let file_id = files
            .iter()
            .position(|f| f.path == file.path)
            .unwrap_or(usize::MAX);

        let local_names: BTreeSet<&str> = file
            .items
            .iter()
            .filter(|i| !i.name.is_empty())
            .map(|i| i.name.as_str())
            .collect();

        for it in &file.items {
            let Some(&from) = global_id_of.get(&(file_id, it.id)) else {
                continue;
            };
            let global_refs = graph::collect_refs(&it.source, &global_names, &it.name);
            for r in global_refs {
                if local_names.contains(r.as_str()) {
                    continue;
                }
                if let Some(cands) = name_index.get(r.as_str()) {
                    // Only link when there is a single global candidate.
                    if cands.len() == 1 {
                        let to = cands[0];
                        if to != from {
                            edges.push(RefEdge { from, to });
                        }
                    }
                }
            }
        }
    }

    // Deduplicate and sort for deterministic output.
    edges.sort_by(|a, b| (a.from, a.to).cmp(&(b.from, b.to)));
    edges.dedup_by(|a, b| a.from == b.from && a.to == b.to);
    edges
}

// ---------------------------------------------------------------------------
// Tokensave enrichment

fn try_enrich_with_tokensave(
    files: &[DiscoveredFile],
    items: &[GlobalItem],
    edges: &mut Vec<RefEdge>,
) -> anyhow::Result<()> {
    let root = match Tokensave::locate(
        files
            .first()
            .map(|f| f.path.as_path())
            .unwrap_or(Path::new(".")),
    ) {
        Some(r) => r,
        None => {
            eprintln!("[discover] no .tokensave/ found; skipping semantic enrichment");
            return Ok(());
        }
    };
    let ts = Tokensave::open_safe(&root)?;

    // 1. Match each ParsedItem to a tokensave Node ID per file.
    let mut node_to_global: HashMap<String, GlobalItemId> = HashMap::new();

    for (file_id, file) in files.iter().enumerate() {
        let abs = file
            .path
            .canonicalize()
            .unwrap_or_else(|_| file.path.clone());
        let rel = abs
            .strip_prefix(&root)
            .map(|p| p.to_path_buf())
            .unwrap_or(abs);
        let rel_str = rel.to_string_lossy().to_string();

        let nodes: Vec<tokensave::types::Node> = ts
            .rt
            .block_on(ts.db.get_nodes_by_file(&rel_str))
            .map_err(|e| anyhow::anyhow!("get_nodes_by_file: {e}"))?;

        for it in &file.items {
            if let Some(node_id) = crate::tokensave::matching::best_node_for_item(it, &nodes) {
                if let Some(gid) = items
                    .iter()
                    .find(|g| g.file_id == file_id && g.local_id == it.id)
                    .map(|g| g.id)
                {
                    node_to_global.insert(node_id.clone(), gid);
                }
            }
        }
    }

    if node_to_global.is_empty() {
        eprintln!("[discover] tokensave: no nodes matched; skipping semantic enrichment");
        return Ok(());
    }

    // 2. Query all internal edges among matched nodes.
    let node_ids: Vec<String> = node_to_global.keys().cloned().collect();
    let db_edges: Vec<tokensave::types::Edge> = ts
        .rt
        .block_on(ts.db.get_internal_edges(&node_ids))
        .map_err(|e| anyhow::anyhow!("get_internal_edges: {e}"))?;

    let mut added = 0;
    for e in db_edges {
        let Some(&from_gid) = node_to_global.get(&e.source) else {
            continue;
        };
        let Some(&to_gid) = node_to_global.get(&e.target) else {
            continue;
        };
        if from_gid == to_gid {
            continue;
        }
        // Only add cross-file edges (heuristic edges already cover intra-file).
        let from_file = items[from_gid].file_id;
        let to_file = items[to_gid].file_id;
        if from_file != to_file {
            edges.push(RefEdge {
                from: from_gid,
                to: to_gid,
            });
            added += 1;
        }
    }

    eprintln!("[discover] tokensave: added {added} cross-file semantic edge(s)");
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests

#[cfg(test)]
mod tests {
    use super::*;

    fn make_src(dir: &Path, name: &str, content: &str) -> PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, content).unwrap();
        p
    }

    #[test]
    fn discovers_and_indexes_two_files() {
        let tmp = tempfile::tempdir().unwrap();
        make_src(tmp.path(), "a.rs", "pub fn foo() {}\n");
        make_src(tmp.path(), "b.rs", "pub fn bar() { foo(); }\n");

        let disc = discover_dir(tmp.path(), false).unwrap();
        assert_eq!(disc.files.len(), 2);
        assert_eq!(disc.items.len(), 2);

        let foo = disc.items_by_name("foo");
        assert_eq!(foo.len(), 1);

        let bar = disc.items_by_name("bar");
        assert_eq!(bar.len(), 1);
    }

    #[test]
    fn discovery_json_omits_raw_source() {
        let tmp = tempfile::tempdir().unwrap();
        make_src(tmp.path(), "a.rs", "pub fn foo() {}\n");

        let disc = discover_dir(tmp.path(), false).unwrap();
        let json = serde_json::to_string(&disc).unwrap();

        assert!(!json.contains("source"));
        assert!(!json.contains("pub fn foo"));
    }

    #[test]
    fn intra_file_edge_resolves() {
        let tmp = tempfile::tempdir().unwrap();
        make_src(
            tmp.path(),
            "a.rs",
            "fn helper() {}\nfn main() { helper(); }\n",
        );

        let disc = discover_dir(tmp.path(), false).unwrap();
        assert!(!disc.edges.is_empty());

        let main = disc.items_by_name("main")[0];
        let helper = disc.items_by_name("helper")[0];
        assert!(
            disc.edges
                .iter()
                .any(|e| e.from == main.id && e.to == helper.id)
        );
    }

    #[test]
    fn cross_file_edge_for_unique_name() {
        let tmp = tempfile::tempdir().unwrap();
        make_src(tmp.path(), "a.rs", "pub fn unique_helper() {}\n");
        make_src(tmp.path(), "b.rs", "pub fn caller() { unique_helper(); }\n");

        let disc = discover_dir(tmp.path(), false).unwrap();
        let caller = disc.items_by_name("caller")[0];
        let helper = disc.items_by_name("unique_helper")[0];
        assert!(
            disc.edges
                .iter()
                .any(|e| e.from == caller.id && e.to == helper.id)
        );
    }

    #[test]
    fn ambiguous_name_does_not_create_cross_edge() {
        let tmp = tempfile::tempdir().unwrap();
        make_src(tmp.path(), "a.rs", "pub fn ambiguous() {}\n");
        make_src(tmp.path(), "b.rs", "pub fn ambiguous() {}\n");
        make_src(tmp.path(), "c.rs", "pub fn caller() { ambiguous(); }\n");

        let disc = discover_dir(tmp.path(), false).unwrap();
        let caller = disc.items_by_name("caller")[0];
        // `ambiguous` has two global candidates → no heuristic edge.
        assert!(
            !disc
                .edges
                .iter()
                .any(|e| e.from == caller.id && disc.item(e.to).unwrap().name == "ambiguous")
        );
    }

    #[test]
    fn skips_target_dir() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("target").join("debug")).unwrap();
        make_src(
            &tmp.path().join("target").join("debug"),
            "junk.rs",
            "fn junk() {}\n",
        );
        make_src(tmp.path(), "real.rs", "fn real() {}\n");

        let disc = discover_dir(tmp.path(), false).unwrap();
        assert_eq!(disc.files.len(), 1);
        assert_eq!(disc.items_by_name("real").len(), 1);
        assert!(disc.items_by_name("junk").is_empty());
    }

    #[test]
    fn cargo_discovery_finds_manifest() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("Cargo.toml"),
            r#"
[package]
name = "demo"
version = "0.1.0"
edition = "2021"
"#,
        )
        .unwrap();
        std::fs::create_dir_all(tmp.path().join("src")).unwrap();
        std::fs::write(tmp.path().join("src/lib.rs"), "pub fn demo() {}\n").unwrap();

        let disc = discover_dir(tmp.path(), false).unwrap();
        assert!(disc.cargo.is_some());
        let cargo = disc.cargo.unwrap();
        assert_eq!(cargo.packages.len(), 1);
        assert_eq!(cargo.packages[0].name, "demo");
        assert!(cargo.packages[0].targets.iter().any(|t| t.kind == "lib"));
    }
}
