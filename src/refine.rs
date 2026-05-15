//! Post-cluster refinement passes. The deterministic carves + type-anchor
//! clusterer produce a plan that's correct but type-myopic — free helpers
//! that share no type and no name prefix all land in `misc` even when they
//! have strong call-graph coupling to an existing bucket. This module
//! cleans that up by moving misc items toward the bucket they actually
//! talk to.

use crate::item::{ItemId, ParsedItem};
use crate::plan::Plan;
use std::collections::{BTreeMap, BTreeSet};

/// Pass signature matches `plan::build::Pass`. We run multiple rounds because
/// the per-round signal is "does this misc item ref something in a real
/// bucket": once a misc item moves out, its in-misc callers can see it in
/// its new bucket and propagate too. Termination is guaranteed by strict
/// shrinkage — each successful round removes ≥1 item from `misc`, so the
/// loop converges after at most `misc.len()` rounds.
pub fn pull_misc_by_calls(
    items: &[ParsedItem],
    plan: &mut Plan,
    _unassigned: &mut BTreeSet<ItemId>,
) {
    let name_to_id: BTreeMap<&str, ItemId> = items
        .iter()
        .filter(|i| !i.name.is_empty())
        .map(|i| (i.name.as_str(), i.id))
        .collect();
    let by_id: BTreeMap<ItemId, &ParsedItem> = items.iter().map(|i| (i.id, i)).collect();

    while pull_one_round(plan, &name_to_id, &by_id) {}

    if plan
        .assignments
        .get("misc")
        .is_some_and(|v| v.is_empty())
    {
        plan.assignments.remove("misc");
    }
}

/// Does one snapshot-driven pass: rebuild the bucket map, then move each
/// misc item to the non-staging bucket its refs talk to most. Returns
/// whether anything moved, which is the fixed-point signal for the outer
/// loop.
fn pull_one_round(
    plan: &mut Plan,
    name_to_id: &BTreeMap<&str, ItemId>,
    by_id: &BTreeMap<ItemId, &ParsedItem>,
) -> bool {
    let misc_ids: Vec<ItemId> = plan
        .assignments
        .get("misc")
        .cloned()
        .unwrap_or_default();
    if misc_ids.is_empty() {
        return false;
    }

    let bucket_of: BTreeMap<ItemId, String> = plan
        .assignments
        .iter()
        .flat_map(|(b, ids)| ids.iter().map(move |id| (*id, b.clone())))
        .collect();

    let mut moved = false;
    for id in misc_ids {
        let it = by_id[&id];
        let mut counts: BTreeMap<String, usize> = BTreeMap::new();
        for r in &it.refs {
            let Some(target_id) = name_to_id.get(r.as_str()) else {
                continue;
            };
            let Some(target_bucket) = bucket_of.get(target_id) else {
                continue;
            };
            // `misc` and `mod_root` are staging sinks, not destinations.
            if target_bucket == "misc" || target_bucket == "mod_root" {
                continue;
            }
            *counts.entry(target_bucket.clone()).or_default() += 1;
        }
        let Some((dest, weight)) = counts.into_iter().max_by_key(|(_, c)| *c) else {
            continue;
        };
        if weight == 0 {
            continue;
        }
        if let Some(v) = plan.assignments.get_mut("misc") {
            v.retain(|x| *x != id);
        }
        plan.assignments
            .entry(dest.clone())
            .or_default()
            .push(id);
        plan.rationale.insert(
            id,
            format!("call-graph: {weight} ref(s) into `{dest}`"),
        );
        moved = true;
    }
    moved
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::item::{ItemKind, ItemVis};

    fn fake_fn(id: ItemId, name: &str, refs: Vec<&str>) -> ParsedItem {
        ParsedItem {
            id,
            kind: ItemKind::Fn { is_test: false },
            name: name.to_string(),
            vis: ItemVis::Private,
            is_cfg_test: false,
            line_start: 1,
            line_end: 1,
            source: String::new(),
            refs: refs.into_iter().map(String::from).collect(),
        }
    }

    #[test]
    fn pull_chain_propagates_via_iteration() {
        // a -> b -> parser. Single-pass would leave `a` in misc (its only
        // ref `b` is still in misc when first checked). Iteration lets `a`
        // move once `b` has been pulled into parser.
        let items = vec![
            fake_fn(0, "a", vec!["b"]),
            fake_fn(1, "b", vec!["Parser"]),
            ParsedItem {
                kind: ItemKind::Struct,
                name: "Parser".into(),
                ..fake_fn(2, "Parser", vec![])
            },
        ];
        let mut plan = Plan::default();
        plan.assign("misc", 0, "");
        plan.assign("misc", 1, "");
        plan.assign("parser", 2, "");
        let mut unassigned = BTreeSet::new();
        pull_misc_by_calls(&items, &mut plan, &mut unassigned);
        let parser = plan.assignments.get("parser").expect("parser bucket");
        assert!(parser.contains(&0), "a should propagate into parser");
        assert!(parser.contains(&1), "b should move into parser");
        assert!(
            !plan.assignments.contains_key("misc"),
            "empty misc bucket should be dropped"
        );
    }

    #[test]
    fn pull_leaves_unanchored_misc_alone() {
        // Two misc items that only ref each other never reach a real bucket,
        // so they stay in misc and the pass terminates.
        let items = vec![
            fake_fn(0, "a", vec!["b"]),
            fake_fn(1, "b", vec!["a"]),
        ];
        let mut plan = Plan::default();
        plan.assign("misc", 0, "");
        plan.assign("misc", 1, "");
        let mut unassigned = BTreeSet::new();
        pull_misc_by_calls(&items, &mut plan, &mut unassigned);
        let misc = plan.assignments.get("misc").expect("misc retained");
        assert_eq!(misc.len(), 2);
    }
}
