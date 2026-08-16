use super::Plan;
use crate::carve::{carve_consts, carve_errors, carve_macros, carve_types};
use crate::cluster::cluster_remaining;
use crate::item::{ItemId, ItemKind, ParsedItem};
use crate::refine::pull_misc_by_calls;
use std::collections::BTreeMap;
use std::collections::BTreeSet;

/// One placement pass. All carves + the clusterer share this shape, which
/// lets [`PASSES`] declare the pipeline as data and makes the ordering
/// obvious at a glance.
pub type Pass = fn(&[ParsedItem], &mut Plan, &mut BTreeSet<ItemId>);

/// The deterministic placement pipeline, in execution order. Early passes
/// carve off easy buckets (tests, macros, error types, const piles, public
/// data types); the final clusterer mops up everything else by type
/// anchor / fn-name prefix.
pub const PASSES: &[(&str, Pass)] = &[
    ("macros", carve_macros),
    ("error", carve_errors),
    ("consts", carve_consts),
    ("types", carve_types),
    ("cluster", cluster_remaining),
    // Refinement: move misc items into the bucket their refs talk to most.
    // Runs last because it depends on the cluster pass having placed every
    // item somewhere first.
    ("pull_misc_by_calls", pull_misc_by_calls),
];

pub fn build(items: &[ParsedItem]) -> Plan {
    let mut plan = Plan::default();
    // A `#[cfg(test)]` declaration can still be part of the module-level API
    // used by non-test items. The classic example is a pair of cfg-selected
    // constants: production code refers to the name unconditionally while
    // the test build selects the test value. Keep that test-side declaration
    // at the module root so child buckets can see it through `use super::*`.
    let cfg_test_names: BTreeSet<String> = items
        .iter()
        .filter(|i| i.is_cfg_test && !i.name.is_empty())
        .map(|i| i.name.clone())
        .collect();
    let shared_cfg_test_names: BTreeSet<String> = items
        .iter()
        .filter(|i| !is_test_item(i))
        .flat_map(|i| i.refs.iter())
        .filter(|name| cfg_test_names.contains(*name))
        .cloned()
        .collect();
    let test_ids: BTreeSet<ItemId> = items
        .iter()
        .filter(|i| is_test_item(i) && !(i.is_cfg_test && shared_cfg_test_names.contains(&i.name)))
        .map(|i| i.id)
        .collect();
    let mut unassigned: BTreeSet<ItemId> = items
        .iter()
        .filter(|i| !test_ids.contains(&i.id))
        .map(|i| i.id)
        .collect();

    for (_name, pass) in PASSES {
        pass(items, &mut plan, &mut unassigned);
    }

    // Anything genuinely left (Use stmts, ForeignMod, etc.) → mod_root.
    for id in std::mem::take(&mut unassigned) {
        plan.assign("mod_root", id, "kept at module root");
    }

    assign_tests(items, &test_ids, &mut plan);

    plan
}

fn is_test_item(it: &ParsedItem) -> bool {
    it.is_cfg_test || matches!(it.kind, ItemKind::Fn { is_test: true })
}

fn assign_tests(items: &[ParsedItem], test_ids: &BTreeSet<ItemId>, plan: &mut Plan) {
    let name_to_id: BTreeMap<&str, ItemId> = items
        .iter()
        .filter(|i| !i.name.is_empty())
        .map(|i| (i.name.as_str(), i.id))
        .collect();
    let bucket_of: BTreeMap<ItemId, String> = plan
        .assignments
        .iter()
        .flat_map(|(bucket, ids)| ids.iter().map(move |id| (*id, bucket.clone())))
        .collect();

    for id in test_ids {
        let it = &items[*id];
        let mut buckets: BTreeSet<String> = BTreeSet::new();
        for r in &it.refs {
            let Some(target_id) = name_to_id.get(r.as_str()) else {
                continue;
            };
            if test_ids.contains(target_id) {
                continue;
            }
            let Some(bucket) = bucket_of.get(target_id) else {
                continue;
            };
            if bucket == "mod_root" {
                continue;
            }
            buckets.insert(bucket.clone());
        }
        if buckets.len() == 1 {
            let bucket = buckets.iter().next().expect("checked len").clone();
            plan.assign(
                &bucket,
                *id,
                format!("unit test: refs only `{bucket}` bucket"),
            );
        } else {
            plan.assign("tests", *id, "integration/cross-bucket test");
        }
    }
}
