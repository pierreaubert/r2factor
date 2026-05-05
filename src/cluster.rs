use crate::item::{ItemId, ItemKind, ParsedItem};
use crate::plan::Plan;
use std::collections::{BTreeMap, BTreeSet};

/// Type-centric clustering: each remaining item is grouped by the type it
/// "anchors on". For impls and data types this is the type name itself; for
/// free functions it's the first param type, then return type, then any
/// known item name referenced. Items without any anchor go to `misc`.
pub fn cluster_remaining(
    items: &[ParsedItem],
    plan: &mut Plan,
    unassigned: &mut BTreeSet<ItemId>,
) {
    let by_id: BTreeMap<ItemId, &ParsedItem> = items.iter().map(|i| (i.id, i)).collect();

    let known_types: BTreeSet<String> = items
        .iter()
        .filter(|i| {
            matches!(
                i.kind,
                ItemKind::Struct | ItemKind::Enum | ItemKind::Union | ItemKind::Trait
            )
        })
        .map(|i| i.name.clone())
        .collect();

    // Prefix groups: fns that share a leading `_`-separated prefix and have
    // ≥2 members beat the type-anchor heuristic. Captures `eval_*`, `parse_*`,
    // `lower_*` etc. that the user wants kept together.
    let prefix_winners = winning_prefixes(items, unassigned, &by_id);

    let mut buckets: BTreeMap<String, Vec<ItemId>> = BTreeMap::new();
    let mut rationales: BTreeMap<ItemId, String> = BTreeMap::new();
    let mut to_skip: Vec<ItemId> = Vec::new();

    for id in unassigned.iter().copied() {
        let it = by_id[&id];
        if matches!(it.kind, ItemKind::Use) {
            // `use` lines are re-derived per-module after the move; leave
            // them at the module root for now.
            to_skip.push(id);
            continue;
        }
        let (anchor, why) = pick_anchor(it, &known_types, &prefix_winners);
        let bucket = anchor.unwrap_or_else(|| "misc".to_string());
        buckets.entry(bucket).or_default().push(id);
        rationales.insert(id, why);
    }

    for id in to_skip {
        unassigned.remove(&id);
        plan.assign("mod_root", id, "kept at module root");
    }

    let collapsed = collapse_singletons(buckets, &by_id);

    for (module, ids) in collapsed {
        let module_name = type_to_module_name(&module);
        for id in ids {
            let why = rationales.remove(&id).unwrap_or_default();
            plan.assign(&module_name, id, why);
            unassigned.remove(&id);
        }
    }
}

fn winning_prefixes(
    items: &[ParsedItem],
    unassigned: &BTreeSet<ItemId>,
    _by_id: &BTreeMap<ItemId, &ParsedItem>,
) -> BTreeSet<String> {
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for it in items {
        if !unassigned.contains(&it.id) {
            continue;
        }
        if !matches!(it.kind, ItemKind::Fn { .. }) {
            continue;
        }
        if let Some(p) = leading_prefix(&it.name) {
            *counts.entry(p).or_default() += 1;
        }
    }
    counts
        .into_iter()
        .filter(|(_, n)| *n >= 2)
        .map(|(k, _)| k)
        .collect()
}

fn leading_prefix(name: &str) -> Option<String> {
    let p = name.split('_').next()?;
    if p.is_empty() || p == name {
        return None;
    }
    Some(p.to_string())
}

fn pick_anchor(
    it: &ParsedItem,
    known_types: &BTreeSet<String>,
    prefix_winners: &BTreeSet<String>,
) -> (Option<String>, String) {
    match &it.kind {
        ItemKind::Impl { self_ty, .. } => (
            Some(self_ty.clone()),
            format!("impl on `{self_ty}`"),
        ),
        ItemKind::Struct | ItemKind::Enum | ItemKind::Union => {
            (Some(it.name.clone()), format!("type `{}`", it.name))
        }
        ItemKind::Trait => (
            Some(it.name.clone()),
            format!("trait `{}`", it.name),
        ),
        ItemKind::TypeAlias => (
            Some(it.name.clone()),
            format!("type alias `{}`", it.name),
        ),
        ItemKind::Fn { .. } => {
            if let Some(p) = leading_prefix(&it.name)
                && prefix_winners.contains(&p)
            {
                return (Some(p.clone()), format!("fn name prefix `{p}_`"));
            }
            if prefix_winners.contains(&it.name) {
                return (
                    Some(it.name.clone()),
                    format!("fn name matches `{}` group", it.name),
                );
            }
            for r in &it.refs {
                if known_types.contains(r) {
                    return (
                        Some(r.clone()),
                        format!("fn references type `{r}`"),
                    );
                }
            }
            (None, "no anchor type".to_string())
        }
        ItemKind::Const | ItemKind::Static => (None, "leftover const/static".to_string()),
        ItemKind::Use => (None, "use stmt — re-derived later".to_string()),
        ItemKind::Mod
        | ItemKind::ForeignMod
        | ItemKind::ExternCrate
        | ItemKind::TraitAlias
        | ItemKind::Macro
        | ItemKind::Verbatim => (None, "kept in misc".to_string()),
    }
}

fn collapse_singletons(
    buckets: BTreeMap<String, Vec<ItemId>>,
    by_id: &BTreeMap<ItemId, &ParsedItem>,
) -> BTreeMap<String, Vec<ItemId>> {
    let mut out: BTreeMap<String, Vec<ItemId>> = BTreeMap::new();
    for (key, ids) in buckets {
        // Keep a singleton bucket if the lone item is a defining type
        // (struct/enum/trait) — it'll likely accrete impls later.
        let is_def = ids.len() == 1
            && matches!(
                by_id[&ids[0]].kind,
                ItemKind::Struct | ItemKind::Enum | ItemKind::Union | ItemKind::Trait
            );
        if ids.len() == 1 && !is_def {
            out.entry("misc".to_string()).or_default().extend(ids);
        } else {
            out.entry(key).or_default().extend(ids);
        }
    }
    out
}

fn type_to_module_name(ty: &str) -> String {
    if ty == "misc" {
        return "misc".to_string();
    }
    let mut out = String::with_capacity(ty.len() + 4);
    let mut prev_lower = false;
    for ch in ty.chars() {
        if ch.is_ascii_uppercase() {
            if prev_lower {
                out.push('_');
            }
            out.push(ch.to_ascii_lowercase());
            prev_lower = false;
        } else {
            out.push(ch);
            prev_lower = ch.is_ascii_lowercase() || ch.is_ascii_digit();
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snake_cases_camel() {
        assert_eq!(type_to_module_name("FooBar"), "foo_bar");
        assert_eq!(type_to_module_name("HTTPClient"), "httpclient");
        assert_eq!(type_to_module_name("Token"), "token");
    }
}
