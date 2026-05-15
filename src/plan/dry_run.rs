use super::Plan;
use crate::item::{ItemId, ItemKind, ParsedItem};
use serde::Serialize;
use std::collections::BTreeMap;

/// JSON-friendly view of a split plan. Both the CLI printer and the MCP
/// server consume this — keeping the structure stable means callers can
/// rely on the field names for schema-driven workflows.
#[derive(Debug, Clone, Serialize)]
pub struct DryRunReport {
    pub total_items: usize,
    pub buckets: Vec<BucketReport>,
}

#[derive(Debug, Clone, Serialize)]
pub struct BucketReport {
    pub name: String,
    pub item_count: usize,
    pub line_count: usize,
    pub items: Vec<ItemReport>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ItemReport {
    pub id: usize,
    pub kind: &'static str,
    /// Empty string for anonymous items (use stmts, macro invocations).
    pub name: String,
    pub line_start: usize,
    pub line_end: usize,
    pub rationale: String,
}

/// Build the structured report. Pure function — no I/O. Sorted by bucket
/// name (alphabetical) to match the on-disk module declaration order.
pub fn dry_run_report(plan: &Plan, items: &[ParsedItem]) -> DryRunReport {
    let by_id: BTreeMap<ItemId, &ParsedItem> = items.iter().map(|i| (i.id, i)).collect();
    let total_items: usize = plan.assignments.values().map(Vec::len).sum();
    let buckets = plan
        .assignments
        .iter()
        .map(|(module, ids)| {
            let mut item_reports: Vec<ItemReport> = ids
                .iter()
                .map(|id| {
                    let it = by_id[id];
                    ItemReport {
                        id: it.id,
                        kind: kind_label(&it.kind),
                        name: it.name.clone(),
                        line_start: it.line_start,
                        line_end: it.line_end,
                        rationale: plan
                            .rationale
                            .get(id)
                            .cloned()
                            .unwrap_or_default(),
                    }
                })
                .collect();
            item_reports.sort_by_key(|i| i.line_start);
            let line_count: usize = item_reports
                .iter()
                .map(|i| i.line_end.saturating_sub(i.line_start) + 1)
                .sum();
            BucketReport {
                name: module.clone(),
                item_count: ids.len(),
                line_count,
                items: item_reports,
            }
        })
        .collect();
    DryRunReport {
        total_items,
        buckets,
    }
}

pub fn print_dry_run(plan: &Plan, items: &[ParsedItem]) {
    let report = dry_run_report(plan, items);
    println!(
        "r2factor split — {} items across {} proposed file(s)",
        report.total_items,
        report.buckets.len()
    );
    println!();
    for bucket in &report.buckets {
        println!(
            "== {name}.rs  ({n} items, ~{lines} lines) ==",
            name = bucket.name,
            n = bucket.item_count,
            lines = bucket.line_count
        );
        for it in &bucket.items {
            let name = if it.name.is_empty() { "<anon>" } else { &it.name };
            println!(
                "  L{:>4}-{:<4}  {:<6}  {:<32}  {}",
                it.line_start, it.line_end, it.kind, name, it.rationale
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
