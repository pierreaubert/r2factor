use super::Tokensave;
use super::matching::best_node_for_item;
use crate::item::{ItemId, ParsedItem};
use anyhow::{Context, Result};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use tokensave::types::{Edge, EdgeKind, Node};

/// Edge kinds that mean "item A semantically belongs near item B" — `calls`
/// is the obvious one, `uses`/`type_of`/`returns` capture the cross-symbol
/// shape edges added in tokensave v4.4 that were previously invisible.
const CLUSTER_EDGE_KINDS: &[EdgeKind] = &[
    EdgeKind::Calls,
    EdgeKind::Uses,
    EdgeKind::TypeOf,
    EdgeKind::Returns,
];

#[derive(Debug, Default)]
pub struct CrossFileEvidence {
    /// For each of our items, the set of OTHER items in the same file
    /// reached by an outgoing semantic edge.
    pub intra_file_callees: HashMap<ItemId, HashSet<ItemId>>,
}

pub(super) fn evidence_for_file(
    ts: &Tokensave,
    file_path: &Path,
    items: &[ParsedItem],
) -> Result<CrossFileEvidence> {
    let abs = file_path
        .canonicalize()
        .with_context(|| format!("canonicalize {}", file_path.display()))?;
    let rel = abs
        .strip_prefix(&ts.project_root)
        .map(|p| p.to_path_buf())
        .unwrap_or(abs);
    let rel_str = rel.to_string_lossy().to_string();

    let nodes: Vec<Node> = ts
        .rt
        .block_on(ts.db.get_nodes_by_file(&rel_str))
        .context("get_nodes_by_file")?;

    // Match each ParsedItem to the tightest enclosing node. Both sides are
    // 1-indexed: we use proc_macro2 line numbers, tokensave exposes
    // `attrs_start_line` and `end_line`.
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
        let edges: Vec<Edge> = ts
            .rt
            .block_on(ts.db.get_outgoing_edges(node_id, CLUSTER_EDGE_KINDS))
            .context("get_outgoing_edges")?;
        for e in edges {
            if let Some(&to) = node_to_item.get(&e.target)
                && to != *item_id
            {
                intra.entry(*item_id).or_default().insert(to);
            }
        }
    }

    Ok(CrossFileEvidence {
        intra_file_callees: intra,
    })
}
