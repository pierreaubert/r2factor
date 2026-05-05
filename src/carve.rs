use crate::item::{ItemId, ItemKind, ItemVis, ParsedItem};
use crate::plan::Plan;
use std::collections::BTreeSet;

/// Items annotated `#[cfg(test)]` (or test fns) → `tests`.
pub fn carve_tests(items: &[ParsedItem], plan: &mut Plan, unassigned: &mut BTreeSet<ItemId>) {
    for it in items {
        if !unassigned.contains(&it.id) {
            continue;
        }
        let is_test_fn = matches!(it.kind, ItemKind::Fn { is_test: true });
        if it.is_cfg_test || is_test_fn {
            plan.assign("tests", it.id, "cfg(test) / #[test]");
            unassigned.remove(&it.id);
        }
    }
}

/// `macro_rules!` definitions → `macros`. They must be defined before use,
/// so consolidating them at the top of the module tree avoids order surprises.
pub fn carve_macros(items: &[ParsedItem], plan: &mut Plan, unassigned: &mut BTreeSet<ItemId>) {
    for it in items {
        if !unassigned.contains(&it.id) {
            continue;
        }
        if matches!(it.kind, ItemKind::Macro) {
            plan.assign("macros", it.id, "macro_rules!");
            unassigned.remove(&it.id);
        }
    }
}

/// Error types (heuristic: name ends with `Error` or has `#[derive(thiserror::Error)]`-ish
/// attrs) and impls anchored on them → `error`.
pub fn carve_errors(items: &[ParsedItem], plan: &mut Plan, unassigned: &mut BTreeSet<ItemId>) {
    let error_types: BTreeSet<String> = items
        .iter()
        .filter(|i| matches!(i.kind, ItemKind::Struct | ItemKind::Enum) && looks_like_error(&i.name))
        .map(|i| i.name.clone())
        .collect();

    if error_types.is_empty() {
        return;
    }

    for it in items {
        if !unassigned.contains(&it.id) {
            continue;
        }
        let is_error = match &it.kind {
            ItemKind::Struct | ItemKind::Enum => error_types.contains(&it.name),
            ItemKind::Impl { self_ty, .. } => error_types.contains(self_ty),
            _ => false,
        };
        if is_error {
            plan.assign("error", it.id, "error type or its impl");
            unassigned.remove(&it.id);
        }
    }
}

fn looks_like_error(name: &str) -> bool {
    name.ends_with("Error") || name.ends_with("Err")
}

/// Pile of `const`/`static` items → `consts` if there are at least 3.
pub fn carve_consts(items: &[ParsedItem], plan: &mut Plan, unassigned: &mut BTreeSet<ItemId>) {
    let candidates: Vec<ItemId> = items
        .iter()
        .filter(|i| {
            unassigned.contains(&i.id) && matches!(i.kind, ItemKind::Const | ItemKind::Static)
        })
        .map(|i| i.id)
        .collect();

    if candidates.len() < 3 {
        return;
    }

    for id in candidates {
        plan.assign("consts", id, "const/static pile");
        unassigned.remove(&id);
    }
}

/// Public traits with ≥2 impls in the file → `types` (along with their impls).
/// Plain data types (no impls in file) also go to `types`.
pub fn carve_types(items: &[ParsedItem], plan: &mut Plan, unassigned: &mut BTreeSet<ItemId>) {
    let mut impl_count_by_self: std::collections::BTreeMap<String, usize> =
        std::collections::BTreeMap::new();
    for it in items {
        if let ItemKind::Impl { self_ty, .. } = &it.kind {
            *impl_count_by_self.entry(self_ty.clone()).or_default() += 1;
        }
    }

    let mut data_no_impls: BTreeSet<String> = BTreeSet::new();
    for it in items {
        if it.is_data_kind() && impl_count_by_self.get(&it.name).copied().unwrap_or(0) == 0 {
            data_no_impls.insert(it.name.clone());
        }
    }

    let mut public_widely_impl_traits: BTreeSet<String> = BTreeSet::new();
    for it in items {
        if matches!(it.kind, ItemKind::Trait) && it.vis == ItemVis::Public {
            let impls = impl_count_by_self.get(&it.name).copied().unwrap_or(0);
            if impls >= 2 {
                public_widely_impl_traits.insert(it.name.clone());
            }
        }
    }

    for it in items {
        if !unassigned.contains(&it.id) {
            continue;
        }
        let go = match &it.kind {
            ItemKind::Struct | ItemKind::Enum | ItemKind::Union | ItemKind::TypeAlias => {
                data_no_impls.contains(&it.name)
            }
            ItemKind::Trait => public_widely_impl_traits.contains(&it.name),
            ItemKind::Impl { self_ty, trait_path } => {
                if let Some(tp) = trait_path {
                    let last = tp.rsplit("::").next().unwrap_or(tp);
                    public_widely_impl_traits.contains(last)
                        || public_widely_impl_traits.contains(tp)
                } else {
                    let _ = self_ty;
                    false
                }
            }
            _ => false,
        };
        if go {
            plan.assign("types", it.id, "plain data / pub trait with multi-impls");
            unassigned.remove(&it.id);
        }
    }
}
