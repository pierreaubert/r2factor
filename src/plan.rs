use crate::carve::{carve_consts, carve_errors, carve_macros, carve_tests, carve_types};
use crate::cluster::cluster_remaining;
use crate::item::{ItemId, ItemKind, ParsedItem};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Default, Debug)]
pub struct Plan {
    pub assignments: BTreeMap<String, Vec<ItemId>>,
    pub rationale: BTreeMap<ItemId, String>,
}

impl Plan {
    pub fn assign(&mut self, module: &str, id: ItemId, rationale: impl Into<String>) {
        self.assignments
            .entry(module.to_string())
            .or_default()
            .push(id);
        self.rationale.insert(id, rationale.into());
    }
}

pub fn build(items: &[ParsedItem]) -> Plan {
    let mut plan = Plan::default();
    let mut unassigned: BTreeSet<ItemId> = items.iter().map(|i| i.id).collect();

    carve_tests(items, &mut plan, &mut unassigned);
    carve_macros(items, &mut plan, &mut unassigned);
    carve_errors(items, &mut plan, &mut unassigned);
    carve_consts(items, &mut plan, &mut unassigned);
    carve_types(items, &mut plan, &mut unassigned);
    cluster_remaining(items, &mut plan, &mut unassigned);

    // Anything genuinely left (Use stmts, ForeignMod, etc.) → mod_root.
    for id in std::mem::take(&mut unassigned) {
        plan.assign("mod_root", id, "kept at module root");
    }

    plan
}

pub fn print_dry_run(plan: &Plan, items: &[ParsedItem]) {
    let total: usize = plan.assignments.values().map(|v| v.len()).sum();
    println!(
        "r2factor split — {} items across {} proposed file(s)",
        total,
        plan.assignments.len()
    );
    println!();

    let by_id: BTreeMap<ItemId, &ParsedItem> = items.iter().map(|i| (i.id, i)).collect();

    for (module, ids) in &plan.assignments {
        let total_lines: usize = ids
            .iter()
            .map(|id| {
                let it = by_id[id];
                it.line_end.saturating_sub(it.line_start) + 1
            })
            .sum();
        println!("== {module}.rs  ({} items, ~{total_lines} lines) ==", ids.len());
        for id in ids {
            let it = by_id[id];
            let kind = kind_label(&it.kind);
            let name = if it.name.is_empty() { "<anon>" } else { &it.name };
            let why = plan.rationale.get(id).map(String::as_str).unwrap_or("");
            println!(
                "  L{:>4}-{:<4}  {:<6}  {:<32}  {}",
                it.line_start, it.line_end, kind, name, why
            );
        }
        println!();
    }
}

fn kind_label(kind: &ItemKind) -> &'static str {
    match kind {
        ItemKind::Fn { is_test: true } => "test",
        ItemKind::Fn { is_test: false } => "fn",
        ItemKind::Struct => "struct",
        ItemKind::Enum => "enum",
        ItemKind::Union => "union",
        ItemKind::Trait => "trait",
        ItemKind::TraitAlias => "tralia",
        ItemKind::Impl { .. } => "impl",
        ItemKind::Macro => "macro",
        ItemKind::Const => "const",
        ItemKind::Static => "static",
        ItemKind::TypeAlias => "type",
        ItemKind::Use => "use",
        ItemKind::ExternCrate => "extern",
        ItemKind::Mod => "mod",
        ItemKind::ForeignMod => "extern{}",
        ItemKind::Verbatim => "verb",
    }
}
