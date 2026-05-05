use crate::item::{ItemId, ParsedItem};
use anyhow::{Context, Result};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use tokensave::db::Database;
use tokensave::types::{Edge, EdgeKind, Node};
use tokio::runtime::{Builder, Runtime};

/// Edge kinds that mean "item A semantically belongs near item B" — `calls`
/// is the obvious one, `uses`/`type_of`/`returns` capture the cross-symbol
/// shape edges added in tokensave v4.4 that were previously invisible.
const CLUSTER_EDGE_KINDS: &[EdgeKind] = &[
    EdgeKind::Calls,
    EdgeKind::Uses,
    EdgeKind::TypeOf,
    EdgeKind::Returns,
];

pub struct Tokensave {
    db: Database,
    project_root: PathBuf,
    rt: Runtime,
}

#[derive(Debug, Default)]
pub struct CrossFileEvidence {
    /// For each of our items, the set of OTHER items in the same file
    /// reached by an outgoing semantic edge.
    pub intra_file_callees: HashMap<ItemId, HashSet<ItemId>>,
    /// For each of our items, the set of node ids that target it from
    /// outside the file (external callers / users).
    pub external_callers: HashMap<ItemId, HashSet<String>>,
}

impl Tokensave {
    /// Walk up from `start` looking for `.tokensave/tokensave.db`. Returns
    /// the project root (parent of `.tokensave`).
    pub fn locate(start: &Path) -> Option<PathBuf> {
        let mut cur = start
            .canonicalize()
            .ok()
            .and_then(|p| p.parent().map(Path::to_path_buf))
            .or_else(|| start.parent().map(Path::to_path_buf))?;
        loop {
            if cur.join(".tokensave").join("tokensave.db").exists() {
                return Some(cur);
            }
            if !cur.pop() {
                return None;
            }
        }
    }

    pub fn open(project_root: &Path) -> Result<Self> {
        let rt = Builder::new_current_thread()
            .enable_all()
            .build()
            .context("build tokio runtime for tokensave")?;
        let db_path = project_root.join(".tokensave").join("tokensave.db");
        let (db, _migrated) = rt
            .block_on(Database::open(&db_path))
            .with_context(|| format!("open tokensave db {}", db_path.display()))?;
        Ok(Self {
            db,
            project_root: project_root.to_path_buf(),
            rt,
        })
    }

    pub fn evidence_for_file(
        &self,
        file_path: &Path,
        items: &[ParsedItem],
    ) -> Result<CrossFileEvidence> {
        let abs = file_path
            .canonicalize()
            .with_context(|| format!("canonicalize {}", file_path.display()))?;
        let rel = abs
            .strip_prefix(&self.project_root)
            .map(|p| p.to_path_buf())
            .unwrap_or(abs);
        let rel_str = rel.to_string_lossy().to_string();

        let nodes: Vec<Node> = self
            .rt
            .block_on(self.db.get_nodes_by_file(&rel_str))
            .context("get_nodes_by_file")?;

        // Match each ParsedItem to the tightest enclosing node. Both sides
        // are 1-indexed: we use proc_macro2 line numbers, tokensave exposes
        // `attrs_start_line` (first line of the leading attr/doc block) and
        // `end_line`.
        let mut node_to_item: HashMap<String, ItemId> = HashMap::new();
        let mut item_to_node: HashMap<ItemId, String> = HashMap::new();
        for it in items {
            if let Some(node_id) = best_node_for_item(it, &nodes) {
                node_to_item.insert(node_id.clone(), it.id);
                item_to_node.insert(it.id, node_id);
            }
        }

        if node_to_item.is_empty() {
            return Ok(CrossFileEvidence::default());
        }

        // Outgoing edges from each of our nodes. One round-trip per source —
        // node count is small (items in a single file), so this is fine.
        let mut intra: HashMap<ItemId, HashSet<ItemId>> = HashMap::new();
        for (item_id, node_id) in &item_to_node {
            let edges: Vec<Edge> = self
                .rt
                .block_on(
                    self.db
                        .get_outgoing_edges(node_id, CLUSTER_EDGE_KINDS),
                )
                .context("get_outgoing_edges")?;
            for e in edges {
                if let Some(&to) = node_to_item.get(&e.target)
                    && to != *item_id
                {
                    intra.entry(*item_id).or_default().insert(to);
                }
            }
        }

        // Bulk incoming edges to all our nodes — one round-trip total.
        let target_ids: Vec<String> = node_to_item.keys().cloned().collect();
        let in_edges: Vec<Edge> = self
            .rt
            .block_on(
                self.db
                    .get_incoming_edges_bulk(&target_ids, CLUSTER_EDGE_KINDS),
            )
            .context("get_incoming_edges_bulk")?;

        let mut callers: HashMap<ItemId, HashSet<String>> = HashMap::new();
        for e in in_edges {
            if node_to_item.contains_key(&e.source) {
                continue; // intra-file, already counted via outgoing
            }
            if let Some(&to) = node_to_item.get(&e.target) {
                callers.entry(to).or_default().insert(e.source);
            }
        }

        Ok(CrossFileEvidence {
            intra_file_callees: intra,
            external_callers: callers,
        })
    }
}

fn best_node_for_item(it: &ParsedItem, nodes: &[Node]) -> Option<String> {
    // Prefer the node whose [attrs_start_line, end_line] tightly aligns with
    // the item's own line range. Tightness = absolute distance between the
    // node's attrs_start_line and the item's line_start.
    let mut best: Option<(String, u32)> = None;
    for n in nodes {
        let s = n.attrs_start_line as usize;
        if s >= it.line_start && s <= it.line_end {
            let d = (s as i64 - it.line_start as i64).unsigned_abs() as u32;
            match &best {
                None => best = Some((n.id.clone(), d)),
                Some((_, bd)) if d < *bd => best = Some((n.id.clone(), d)),
                _ => {}
            }
        }
    }
    best.map(|(id, _)| id)
}
