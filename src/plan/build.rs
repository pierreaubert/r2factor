use super::Plan;
use crate::carve::{carve_consts, carve_errors, carve_macros, carve_tests, carve_types};
use crate::cluster::cluster_remaining;
use crate::item::{ItemId, ParsedItem};
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
    ("tests", carve_tests),
    ("macros", carve_macros),
    ("error", carve_errors),
    ("consts", carve_consts),
    ("types", carve_types),
    ("cluster", cluster_remaining),
];

pub fn build(items: &[ParsedItem]) -> Plan {
    let mut plan = Plan::default();
    let mut unassigned: BTreeSet<ItemId> = items.iter().map(|i| i.id).collect();

    for (_name, pass) in PASSES {
        pass(items, &mut plan, &mut unassigned);
    }

    // Anything genuinely left (Use stmts, ForeignMod, etc.) → mod_root.
    for id in std::mem::take(&mut unassigned) {
        plan.assign("mod_root", id, "kept at module root");
    }

    plan
}
