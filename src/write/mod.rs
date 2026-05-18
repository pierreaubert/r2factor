//! Materialize a `Plan` to disk. The orchestrator [`write_plan`] handles
//! ordering (backup before destruction, sub-files before facade) and stale-
//! file cleanup; the rendering helpers live in submodules.

mod backup;
mod facade;
mod preamble;
mod subfile;
mod uses;

use crate::item::{ItemId, ItemKind, ParsedItem};
use crate::plan::Plan;
use crate::promote::{
    CrossImport, RefContext, compute_cross_imports, compute_facade_imports, compute_field_lifts,
    compute_impl_lifts, compute_promotions,
};
use std::collections::HashSet;
use anyhow::{Context, Result, anyhow, bail};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use backup::make_backup_path;
use facade::render_facade;
use preamble::extract_inner_attrs;
use subfile::render_sub_file;

/// Sentinel emitted as the first line of the auto-generated facade. We refuse
/// to split a file that contains it in its first few lines: feeding the
/// facade back into `r2factor` would parse only the `mod`/`use` declarations
/// and regenerate a different facade, then stale-file cleanup would delete the
/// previously-generated sub-files. We don't want that.
pub(super) const FACADE_MARKER: &str =
    "// r2factor:facade — do not pass this file back into r2factor";

/// Substring searched for inside the marker line. Centralized here so the
/// detection rule lives next to the emission rule — if `FACADE_MARKER`
/// changes shape later, callers don't drift.
const FACADE_MARKER_NEEDLE: &str = "r2factor:facade";

/// Returns true if `src` looks like an r2factor-generated facade. Checked by
/// both `write_plan` (to bail before destroying sub-files) and
/// `pipeline::run_split` (to bail before even running the dry-run).
pub fn is_r2factor_facade(src: &str) -> bool {
    src.lines()
        .take(20)
        .any(|l| l.contains(FACADE_MARKER_NEEDLE))
}

pub struct WriteOptions {
    pub force: bool,
}

#[derive(Debug, serde::Serialize)]
pub struct WriteReport {
    pub backup: PathBuf,
    /// `None` when every bucket ended up in the facade (mod_root + primary)
    /// and no sub-files were written, so the target dir was never kept.
    pub target_dir: Option<PathBuf>,
    pub written_files: Vec<PathBuf>,
    pub facade: PathBuf,
}

pub fn write_plan(
    original: &Path,
    plan: &Plan,
    items: &[ParsedItem],
    opts: &WriteOptions,
) -> Result<WriteReport> {
    let parent = original.parent().unwrap_or(Path::new("."));
    let stem = original
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or_else(|| anyhow!("invalid file stem in {}", original.display()))?;
    if matches!(stem, "lib" | "main" | "mod") {
        bail!("splitting `{stem}.rs` is not supported in v0 — choose a regular module file");
    }

    // Refuse to split our own output. The pipeline already bails for the
    // common case, but this is the last-line guard before we destroy
    // anything on disk.
    let original_src = fs::read_to_string(original)?;
    if is_r2factor_facade(&original_src) {
        bail!(
            "refusing to split {}: it is already an r2factor facade. Run on the original source or restore from the `.bak` backup.",
            original.display()
        );
    }
    let target_dir = parent.join(stem);

    // 1) Backup FIRST. If this fails, we abort with no destruction.
    let backup = make_backup_path(original)?;
    fs::copy(original, &backup).with_context(|| {
        format!("backup {} -> {}", original.display(), backup.display())
    })?;

    // 2) Source preamble: inner attrs/doc-mod comments. The full set of
    //    `use` items is gathered as ParsedItem refs so we can pick a
    //    minimal subset per bucket below.
    let by_id: BTreeMap<ItemId, &ParsedItem> = items.iter().map(|i| (i.id, i)).collect();
    let inner_attrs = extract_inner_attrs(&original_src);
    let all_uses: Vec<&ParsedItem> = items
        .iter()
        .filter(|i| matches!(i.kind, ItemKind::Use))
        .collect();
    // Cross-bucket reference graph -> private items that need lifting,
    // plus the explicit imports each consumer needs so the bare-name refs
    // still resolve after the lift. All three derived from one shared
    // `RefContext` so we don't rebuild the lookup maps three times.
    let ctx = RefContext::new(plan, items);
    let promote: BTreeSet<ItemId> = compute_promotions(&ctx, items, stem);
    let cross_imports = compute_cross_imports(&ctx, items, &promote, stem);
    let facade_imports = compute_facade_imports(&ctx, items, &promote, stem);
    // Inherent-impl blocks for promoted types: rewrite associated items
    // (fn/const/type) to `pub(super)` so cross-bucket `Type::method()`
    // calls resolve. Without this, E0624 ("associated function ... is
    // private") fires on every promoted type that has an impl block.
    let impl_lifts: BTreeSet<ItemId> = compute_impl_lifts(&ctx, items, stem);
    // Field-vis lift for *non-promoted* structs/unions in sub-buckets.
    // Catches `pub struct` with private fields read cross-bucket (E0616).
    let field_lifts: BTreeSet<ItemId> = compute_field_lifts(&ctx, items, &promote, stem);

    // Bucket-name renames: a bucket whose name shadows something brought
    // into facade scope (e.g. `fmt` colliding with `use std::fmt;`) would
    // produce an E0255 ("name defined multiple times") at the `mod fmt;`
    // emission. Detect collisions against the names imported by
    // `mod_root` and append `_mod` to disambiguate.
    let renames = compute_bucket_renames(plan, &all_uses, stem);
    let assignments = rename_assignments(&plan.assignments, &renames);
    let cross_imports = rename_cross_imports(cross_imports, &renames);
    let facade_imports = rename_facade_imports(facade_imports, &renames);

    // Only files that the plan actually materialises can conflict.
    let planned_files: BTreeSet<String> = assignments
        .keys()
        .filter(|&m| m != "mod_root" && m != stem)
        .cloned()
        .collect();

    // Determine which .rs files already live in the target directory so we
    // can preserve user-created sub-modules and only error when a planned
    // file would collide.
    let existing_modules: BTreeSet<String> = if target_dir.is_dir() {
        fs::read_dir(&target_dir)
            .with_context(|| format!("read_dir {}", target_dir.display()))?
            .filter_map(|e| e.ok())
            .filter_map(|e| {
                let p = e.path();
                if p.is_file() && p.extension().is_some_and(|ext| ext == "rs") {
                    p.file_stem().and_then(|s| s.to_str()).map(String::from)
                } else {
                    None
                }
            })
            .collect()
    } else {
        BTreeSet::new()
    };

    let conflicts: Vec<String> = planned_files
        .intersection(&existing_modules)
        .cloned()
        .collect();

    if !conflicts.is_empty() && !opts.force {
        bail!(
            "target file(s) already exist: {}; pass --force to overwrite",
            conflicts.join(", ")
        );
    }

    // With --force, remove conflicting files and any stale generated files
    // (identified by the r2factor marker) that are no longer in the plan.
    // User-created files that do not conflict are always preserved.
    if opts.force && target_dir.is_dir() {
        for entry in fs::read_dir(&target_dir)
            .with_context(|| format!("read_dir {}", target_dir.display()))?
        {
            let entry = entry.with_context(|| format!("read_dir entry in {}", target_dir.display()))?;
            let path = entry.path();
            if path.is_file() && path.extension().is_some_and(|ext| ext == "rs") {
                let file_stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
                let is_conflicting = planned_files.contains(file_stem);
                let is_stale_generated = !planned_files.contains(file_stem) && {
                    fs::read_to_string(&path)
                        .unwrap_or_default()
                        .contains("Auto-generated by r2factor")
                };
                if is_conflicting || is_stale_generated {
                    fs::remove_file(&path)
                        .with_context(|| format!("remove stale/conflicting {}", path.display()))?;
                }
            }
        }
    }

    // Re-scan after potential purge so the facade reflects the actual disk state.
    let existing_modules: BTreeSet<String> = if target_dir.is_dir() {
        fs::read_dir(&target_dir)
            .with_context(|| format!("read_dir {}", target_dir.display()))?
            .filter_map(|e| e.ok())
            .filter_map(|e| {
                let p = e.path();
                if p.is_file() && p.extension().is_some_and(|ext| ext == "rs") {
                    p.file_stem().and_then(|s| s.to_str()).map(String::from)
                } else {
                    None
                }
            })
            .collect()
    } else {
        BTreeSet::new()
    };

    // 3) Sub-files.
    fs::create_dir_all(&target_dir)
        .with_context(|| format!("mkdir {}", target_dir.display()))?;

    let mut written: Vec<PathBuf> = Vec::new();
    let mut sub_modules: Vec<String> = Vec::new();
    let mut facade_uses: Vec<&ParsedItem> = Vec::new();
    let mut facade_primary: Vec<&ParsedItem> = Vec::new();

    // Preserve existing sub-modules that are not part of the current plan.
    for module in &existing_modules {
        if !planned_files.contains(module) {
            sub_modules.push(module.clone());
        }
    }

    for (module, ids) in &assignments {
        if module == "mod_root" {
            for id in ids {
                facade_uses.push(by_id[id]);
            }
            continue;
        }
        if module == stem {
            for id in ids {
                facade_primary.push(by_id[id]);
            }
            continue;
        }
        let path = target_dir.join(format!("{module}.rs"));
        let bucket_prelude = bucket_use_prelude(ids, &by_id, &all_uses);
        let imports = cross_imports.get(module).cloned().unwrap_or_default();
        let body = render_sub_file(
            ids,
            &by_id,
            &bucket_prelude,
            &promote,
            &impl_lifts,
            &field_lifts,
            &imports,
        );
        fs::write(&path, body).with_context(|| format!("write {}", path.display()))?;
        written.push(path);
        sub_modules.push(module.clone());
    }
    sub_modules.sort();

    let kept_target_dir = if sub_modules.is_empty() {
        match fs::remove_dir(&target_dir) {
            Ok(()) => None,
            Err(e) => {
                eprintln!(
                    "[write] warn: could not remove unused target dir {}: {e}",
                    target_dir.display()
                );
                Some(target_dir.clone())
            }
        }
    } else {
        Some(target_dir.clone())
    };

    // 4) Facade: replace the original file. Primary items get promoted too
    //    since the facade is a sibling module to the sub-files at the
    //    parent's perspective.
    let facade_body = render_facade(
        &inner_attrs,
        &facade_uses,
        &facade_primary,
        &sub_modules,
        &promote,
        &impl_lifts,
        &facade_imports,
    );
    fs::write(original, facade_body)
        .with_context(|| format!("write facade {}", original.display()))?;

    Ok(WriteReport {
        backup,
        target_dir: kept_target_dir,
        written_files: written,
        facade: original.to_path_buf(),
    })
}

/// Pick the subset of `all_uses` that the bucket actually references and
/// render them joined by newlines. Each surviving `use` is rebased through
/// `uses::rebase_use_for_subfile` because sub-files live one module-level
/// deeper than the original — `use super::foo;` from the original needs to
/// become `use super::super::foo;` from a sub-file.
///
/// Falls back to the full (unrebased) prelude if the bucket's source fails
/// to parse — defensive only, shouldn't trip since each item came through
/// `syn::parse_file` originally.
fn bucket_use_prelude(
    bucket_ids: &[ItemId],
    by_id: &BTreeMap<ItemId, &ParsedItem>,
    all_uses: &[&ParsedItem],
) -> String {
    let Some(idents) = uses::bucket_idents_for(bucket_ids, by_id) else {
        return all_uses
            .iter()
            .map(|u| uses::rebase_use_for_subfile(&u.source))
            .collect::<Vec<_>>()
            .join("\n");
    };
    let selected = uses::select_uses_for(all_uses, &idents);
    selected
        .iter()
        .map(|u| uses::rebase_use_for_subfile(&u.source))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Compute renames for sub-bucket names that collide with names already in
/// the facade's scope. The collision matters at the `mod NAME;` emission —
/// Rust forbids the same name appearing as both an imported item and a
/// module in the same scope, so `use std::fmt;` + `mod fmt;` is E0255.
///
/// Facade buckets (`mod_root`, `tests`, the stem-named primary bucket) are
/// never renamed: the stem-bucket items live in the facade itself, mod_root
/// items don't materialize as their own module, and `tests` is a fixed
/// well-known name.
fn compute_bucket_renames(
    plan: &Plan,
    all_uses: &[&ParsedItem],
    stem: &str,
) -> BTreeMap<String, String> {
    // Names brought into facade scope by `use` items that live in `mod_root`.
    // We don't worry about cross-bucket imports here — those are emitted
    // from inside sub-files where the bucket name doesn't collide with
    // facade-level imports.
    let mod_root_ids: BTreeSet<ItemId> = plan
        .assignments
        .get("mod_root")
        .map(|v| v.iter().copied().collect())
        .unwrap_or_default();
    let mut imported: HashSet<String> = HashSet::new();
    for u in all_uses {
        if !mod_root_ids.contains(&u.id) {
            continue;
        }
        for name in uses::use_bindings(u) {
            imported.insert(name);
        }
    }
    let mut renames: BTreeMap<String, String> = BTreeMap::new();
    for bucket in plan.assignments.keys() {
        if bucket == "mod_root" || bucket == stem || bucket == "tests" {
            continue;
        }
        if imported.contains(bucket) {
            renames.insert(bucket.clone(), format!("{bucket}_mod"));
        }
    }
    renames
}

fn rename_assignments(
    src: &BTreeMap<String, Vec<ItemId>>,
    renames: &BTreeMap<String, String>,
) -> BTreeMap<String, Vec<ItemId>> {
    if renames.is_empty() {
        return src.clone();
    }
    let mut out: BTreeMap<String, Vec<ItemId>> = BTreeMap::new();
    for (bucket, ids) in src {
        let key = renames.get(bucket).cloned().unwrap_or_else(|| bucket.clone());
        out.entry(key).or_default().extend(ids.iter().copied());
    }
    out
}

fn rename_cross_imports(
    src: BTreeMap<String, BTreeSet<CrossImport>>,
    renames: &BTreeMap<String, String>,
) -> BTreeMap<String, BTreeSet<CrossImport>> {
    if renames.is_empty() {
        return src;
    }
    let mut out: BTreeMap<String, BTreeSet<CrossImport>> = BTreeMap::new();
    for (consumer, imports) in src {
        let consumer_new = renames.get(&consumer).cloned().unwrap_or(consumer);
        let entry = out.entry(consumer_new).or_default();
        for c in imports {
            let src_new = renames
                .get(&c.source_bucket)
                .cloned()
                .unwrap_or(c.source_bucket);
            entry.insert(CrossImport {
                source_bucket: src_new,
                name: c.name,
                cfg_attrs: c.cfg_attrs,
            });
        }
    }
    out
}

fn rename_facade_imports(
    src: BTreeSet<CrossImport>,
    renames: &BTreeMap<String, String>,
) -> BTreeSet<CrossImport> {
    if renames.is_empty() {
        return src;
    }
    src.into_iter()
        .map(|c| {
            let src_new = renames
                .get(&c.source_bucket)
                .cloned()
                .unwrap_or(c.source_bucket);
            CrossImport {
                source_bucket: src_new,
                name: c.name,
                cfg_attrs: c.cfg_attrs,
            }
        })
        .collect()
}


