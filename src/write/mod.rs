//! Materialize a `Plan` to disk. The orchestrator [`write_plan`] handles
//! ordering (backup before destruction, sub-files before facade) and stale-
//! file cleanup; the rendering helpers live in submodules.

mod backup;
mod facade;
mod preamble;
mod proc_macro_entry;
mod subfile;
mod uses;

use crate::item::{ItemId, ItemKind, ItemVis, ParsedItem};
use crate::plan::Plan;
use crate::promote::{
    CrossImport, RefContext, cfg_attrs_of, compute_cross_imports, compute_facade_imports,
    compute_field_lifts, compute_impl_lifts, compute_promotions, needs_keyword_rewrite,
};
use anyhow::{Context, Result, anyhow, bail};
use quote::ToTokens;
use std::collections::HashSet;
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

#[derive(Clone, Copy, Debug)]
pub struct WriteOptions {
    pub force: bool,
    pub recursive_max_lines: Option<usize>,
}

#[derive(Debug, serde::Serialize)]
pub struct WriteReport {
    pub backup: PathBuf,
    pub removed_file_backups: Vec<RemovedFileBackup>,
    /// `None` when every bucket ended up in the facade (mod_root + primary)
    /// and no sub-files were written, so the target dir was never kept.
    pub target_dir: Option<PathBuf>,
    pub written_files: Vec<PathBuf>,
    pub facade: PathBuf,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct RemovedFileBackup {
    pub path: PathBuf,
    pub backup: PathBuf,
}

struct SplitLayout {
    primary_bucket: String,
    target_dir: PathBuf,
    facade_path_prefix: Option<String>,
    exclude_existing_file: Option<PathBuf>,
    remove_target_dir_if_empty: bool,
    keep_main_in_facade: bool,
}

impl SplitLayout {
    fn for_path(original: &Path) -> Result<Self> {
        let parent = original.parent().unwrap_or(Path::new("."));
        let file_name = original
            .file_name()
            .and_then(|s| s.to_str())
            .ok_or_else(|| anyhow!("invalid file name in {}", original.display()))?;
        let stem = original
            .file_stem()
            .and_then(|s| s.to_str())
            .ok_or_else(|| anyhow!("invalid file stem in {}", original.display()))?;

        match file_name {
            "main.rs" => {
                let in_place = should_split_in_place(parent, original, "main");
                Ok(Self {
                    primary_bucket: "main".to_string(),
                    target_dir: if in_place {
                        parent.to_path_buf()
                    } else {
                        parent.join("main")
                    },
                    facade_path_prefix: if in_place {
                        None
                    } else {
                        Some("main".to_string())
                    },
                    exclude_existing_file: in_place.then(|| original.to_path_buf()),
                    remove_target_dir_if_empty: !in_place,
                    keep_main_in_facade: true,
                })
            }
            "lib.rs" => {
                let in_place = should_split_in_place(parent, original, "lib");
                Ok(Self {
                    primary_bucket: "lib".to_string(),
                    target_dir: if in_place {
                        parent.to_path_buf()
                    } else {
                        parent.join("lib")
                    },
                    facade_path_prefix: if in_place {
                        None
                    } else {
                        Some("lib".to_string())
                    },
                    exclude_existing_file: in_place.then(|| original.to_path_buf()),
                    remove_target_dir_if_empty: !in_place,
                    keep_main_in_facade: false,
                })
            }
            "mod.rs" => {
                let primary = parent.file_name().and_then(|s| s.to_str()).ok_or_else(|| {
                    anyhow!("cannot infer module name for {}", original.display())
                })?;
                Ok(Self {
                    primary_bucket: primary.to_string(),
                    target_dir: parent.to_path_buf(),
                    facade_path_prefix: None,
                    exclude_existing_file: Some(original.to_path_buf()),
                    remove_target_dir_if_empty: false,
                    keep_main_in_facade: false,
                })
            }
            _ => {
                let in_place = should_split_in_place(parent, original, stem);
                let crate_root_like = is_cargo_crate_root_like(parent, file_name);
                Ok(Self {
                    primary_bucket: stem.to_string(),
                    target_dir: if in_place {
                        parent.to_path_buf()
                    } else {
                        parent.join(stem)
                    },
                    facade_path_prefix: if in_place {
                        (!crate_root_like).then(String::new)
                    } else {
                        crate_root_like.then(|| stem.to_string())
                    },
                    exclude_existing_file: in_place.then(|| original.to_path_buf()),
                    remove_target_dir_if_empty: !in_place,
                    keep_main_in_facade: is_binary_crate_root_like(parent, file_name),
                })
            }
        }
    }

    fn includes_existing_module_file(&self, path: &Path) -> bool {
        if !path.is_file() || path.extension().is_none_or(|ext| ext != "rs") {
            return false;
        }
        if let Some(exclude) = &self.exclude_existing_file
            && same_path(path, exclude)
        {
            return false;
        }
        true
    }
}

fn existing_sibling_super_imports(target_dir: &Path, layout: &SplitLayout) -> BTreeSet<String> {
    let mut imports = BTreeSet::new();
    let Ok(entries) = fs::read_dir(target_dir) else {
        return imports;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !layout.includes_existing_module_file(&path) {
            continue;
        }
        let Ok(source) = fs::read_to_string(&path) else {
            continue;
        };
        let Ok(file) = syn::parse_file(&source) else {
            continue;
        };
        for item in file.items {
            let syn::Item::Use(item) = item else {
                continue;
            };
            direct_super_use_bindings(&item.tree, &mut imports);
        }
    }
    imports
}

fn direct_super_use_bindings(tree: &syn::UseTree, out: &mut BTreeSet<String>) {
    match tree {
        syn::UseTree::Path(path) if path.ident == "super" => {
            direct_super_use_tail_bindings(&path.tree, out);
        }
        syn::UseTree::Group(group) => {
            for tree in &group.items {
                direct_super_use_bindings(tree, out);
            }
        }
        _ => {}
    }
}

fn direct_super_use_tail_bindings(tree: &syn::UseTree, out: &mut BTreeSet<String>) {
    match tree {
        syn::UseTree::Name(name) => {
            out.insert(name.ident.to_string());
        }
        syn::UseTree::Rename(rename) => {
            out.insert(rename.ident.to_string());
        }
        syn::UseTree::Group(group) => {
            for tree in &group.items {
                direct_super_use_tail_bindings(tree, out);
            }
        }
        // `use super::nested::Thing` is a reference to the `nested` module,
        // not a facade-level item named `Thing`; leave it unchanged.
        syn::UseTree::Path(_) | syn::UseTree::Glob(_) => {}
    }
}

fn add_existing_sibling_facade_imports(
    promote: &mut BTreeSet<ItemId>,
    facade_imports: &mut BTreeSet<CrossImport>,
    ctx: &RefContext<'_>,
    stem: &str,
    imported_names: &BTreeSet<String>,
) {
    for name in imported_names {
        let Some(ids) = ctx.name_to_ids.get(name.as_str()) else {
            continue;
        };
        // An unqualified sibling import can only be retained safely when it
        // identified exactly one source item before the split. Ambiguous
        // names already require user-side qualification, so don't invent a
        // potentially different binding in the generated facade.
        if ids.len() != 1 {
            continue;
        }
        let id = ids[0];
        let Some(bucket) = ctx.bucket_of.get(&id) else {
            continue;
        };
        if bucket.as_str() == "mod_root" || bucket.as_str() == stem {
            continue;
        }
        let item = ctx.by_id[&id];
        if item.vis == ItemVis::Private && needs_keyword_rewrite(item) {
            promote.insert(id);
        }
        facade_imports.insert(CrossImport {
            source_bucket: Some((*bucket).clone()),
            name: item.name.clone(),
            cfg_attrs: cfg_attrs_of(&item.source),
        });
    }
}

fn is_binary_crate_root_like(parent: &Path, file_name: &str) -> bool {
    if file_name == "build.rs" {
        return true;
    }
    matches!(
        parent.file_name().and_then(|s| s.to_str()),
        Some("bin" | "examples" | "benches" | "bench")
    )
}

fn is_cargo_crate_root_like(parent: &Path, file_name: &str) -> bool {
    if file_name == "build.rs" {
        return true;
    }
    matches!(
        parent.file_name().and_then(|s| s.to_str()),
        Some("bin" | "examples" | "tests" | "benches" | "bench")
    )
}

fn should_split_in_place(parent: &Path, original: &Path, target_dir_name: &str) -> bool {
    !parent.join(target_dir_name).is_dir() && directory_contains_only_target_file(parent, original)
}

fn directory_contains_only_target_file(parent: &Path, original: &Path) -> bool {
    let Ok(entries) = fs::read_dir(parent) else {
        return false;
    };
    let mut saw_original = false;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_file() {
            if same_path(&path, original) {
                saw_original = true;
            } else {
                return false;
            }
        }
    }
    saw_original
}

pub fn write_plan(
    original: &Path,
    plan: &Plan,
    items: &[ParsedItem],
    opts: &WriteOptions,
) -> Result<WriteReport> {
    let layout = SplitLayout::for_path(original)?;
    let stem = layout.primary_bucket.as_str();
    let adjusted_plan = force_facade_pinned_items(plan, items, stem, layout.keep_main_in_facade);
    let plan = &adjusted_plan;

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
    let target_dir = layout.target_dir.clone();
    // A sibling module already living in the target directory can refer to
    // private names from this file via `use super::{name};`. Those names are
    // not present in the split input's reference graph, so retain the
    // sibling-facing facade bindings explicitly before moving their owners.
    let existing_sibling_imports = existing_sibling_super_imports(&target_dir, &layout);

    // 1) Backup FIRST. If this fails, we abort with no destruction.
    let backup = make_backup_path(original)?;
    fs::copy(original, &backup)
        .with_context(|| format!("backup {} -> {}", original.display(), backup.display()))?;

    // 2) Source preamble: inner attrs/doc-mod comments. The full set of
    //    `use` items is gathered as ParsedItem refs so we can pick a
    //    minimal subset per bucket below.
    let by_id: BTreeMap<ItemId, &ParsedItem> = items.iter().map(|i| (i.id, i)).collect();
    let inner_attrs = extract_inner_attrs(&original_src);
    let all_uses: Vec<&ParsedItem> = items
        .iter()
        .filter(|i| matches!(i.kind, ItemKind::Use))
        .collect();
    let external_mod_attrs = external_mod_decl_attrs(&by_id);
    let explicit_external_modules: BTreeSet<String> = external_mod_attrs.keys().cloned().collect();
    // Cross-bucket reference graph -> private items that need lifting,
    // plus the explicit imports each consumer needs so the bare-name refs
    // still resolve after the lift. All three derived from one shared
    // `RefContext` so we don't rebuild the lookup maps three times.
    let ctx = RefContext::new(plan, items);
    let mut promote: BTreeSet<ItemId> = compute_promotions(&ctx, items, stem);
    let cross_imports = compute_cross_imports(&ctx, items, &promote, stem);
    let mut facade_imports = compute_facade_imports(&ctx, items, &promote, stem);
    add_existing_sibling_facade_imports(
        &mut promote,
        &mut facade_imports,
        &ctx,
        stem,
        &existing_sibling_imports,
    );
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
    let renames = compute_bucket_renames(plan, items, &all_uses, stem, &explicit_external_modules);
    let assignments = rename_assignments(&plan.assignments, &renames);
    let cross_imports = rename_cross_imports(cross_imports, &renames);
    let facade_imports = rename_facade_imports(facade_imports, &renames);
    let (crate_promote, crate_root_imports) = external_crate_root_imports(
        original,
        &assignments,
        &by_id,
        stem,
        &explicit_external_modules,
    );
    let proc_macro_exports = proc_macro_exports(&assignments, &by_id, stem);

    // Only files that the plan actually materialises can conflict.
    let planned_files: BTreeSet<String> = assignments
        .keys()
        .filter(|&m| m != "mod_root" && m != stem)
        .cloned()
        .collect();
    let included_module_files = include_target_modules(items);
    let included_facade_items = included_facade_items(original, items);
    let macro_defined_names = macro_defined_names_by_source(&assignments, &by_id, stem);
    let trait_method_imports = trait_method_imports_by_method(&assignments, &by_id, stem);
    add_trait_method_promotions(
        &mut promote,
        &assignments,
        &by_id,
        stem,
        &trait_method_imports,
    );
    let parent_generated_items = parent_generated_sibling_items(original);

    // Determine which .rs files already live in the target directory so we
    // can preserve user-created sub-modules and only error when a planned
    // file would collide.
    let existing_modules: BTreeSet<String> = if target_dir.is_dir() {
        fs::read_dir(&target_dir)
            .with_context(|| format!("read_dir {}", target_dir.display()))?
            .filter_map(|e| e.ok())
            .filter_map(|e| {
                let p = e.path();
                if layout.includes_existing_module_file(&p) {
                    p.file_stem().and_then(|s| s.to_str()).map(String::from)
                } else {
                    None
                }
            })
            .filter(|module| !explicit_external_modules.contains(module))
            .filter(|module| !included_module_files.contains(module))
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
    let mut removed_file_backups: Vec<RemovedFileBackup> = Vec::new();
    if opts.force && target_dir.is_dir() {
        for entry in fs::read_dir(&target_dir)
            .with_context(|| format!("read_dir {}", target_dir.display()))?
        {
            let entry =
                entry.with_context(|| format!("read_dir entry in {}", target_dir.display()))?;
            let path = entry.path();
            if layout.includes_existing_module_file(&path) {
                let file_stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
                let is_conflicting = planned_files.contains(file_stem);
                let is_stale_generated = !planned_files.contains(file_stem)
                    && !explicit_external_modules.contains(file_stem)
                    && {
                        fs::read_to_string(&path)
                            .unwrap_or_default()
                            .contains("Auto-generated by r2factor")
                    };
                if is_conflicting || is_stale_generated {
                    let backup = make_backup_path(&path)?;
                    fs::copy(&path, &backup).with_context(|| {
                        format!("backup removed {} -> {}", path.display(), backup.display())
                    })?;
                    removed_file_backups.push(RemovedFileBackup {
                        path: path.clone(),
                        backup,
                    });
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
                if layout.includes_existing_module_file(&p) {
                    p.file_stem().and_then(|s| s.to_str()).map(String::from)
                } else {
                    None
                }
            })
            .filter(|module| !explicit_external_modules.contains(module))
            .filter(|module| !included_module_files.contains(module))
            .collect()
    } else {
        BTreeSet::new()
    };

    // 3) Sub-files.
    fs::create_dir_all(&target_dir).with_context(|| format!("mkdir {}", target_dir.display()))?;

    let mut written: Vec<PathBuf> = Vec::new();
    let mut sub_modules: Vec<String> = Vec::new();
    let mut reexport_modules: BTreeSet<String> = BTreeSet::new();
    let mut facade_root_items: Vec<String> = Vec::new();
    let mut facade_primary: Vec<&ParsedItem> = Vec::new();

    // Preserve existing sub-modules that are not part of the current plan.
    for module in &existing_modules {
        if !planned_files.contains(module) {
            sub_modules.push(module.clone());
            reexport_modules.insert(module.clone());
        }
    }
    let inline_facade_module_names: BTreeSet<String> = items
        .iter()
        .filter(|item| matches!(item.kind, ItemKind::Mod) && !item.is_external_mod_decl())
        .filter_map(|item| {
            let bucket = ctx.bucket_of.get(&item.id)?;
            if bucket.as_str() == "mod_root" || bucket.as_str() == stem {
                Some(item.name.clone())
            } else {
                None
            }
        })
        .collect();
    let local_module_roots: HashSet<String> = planned_files
        .iter()
        .chain(existing_modules.iter())
        .chain(explicit_external_modules.iter())
        .chain(inline_facade_module_names.iter())
        .cloned()
        .collect();

    for (module, ids) in &assignments {
        if module == "mod_root" {
            for id in ids {
                let item = by_id[id];
                if !matches!(item.kind, ItemKind::Use)
                    && !include_targets_planned_module(&item.source, &planned_files)
                {
                    facade_root_items.push(item.source.clone());
                }
            }
            continue;
        }
        if module == stem {
            for id in ids {
                let item = by_id[id];
                if !include_targets_planned_module(&item.source, &planned_files) {
                    facade_primary.push(item);
                }
            }
            continue;
        }
        let path = target_dir.join(format!("{module}.rs"));
        let bucket_idents = uses::bucket_idents_for(ids, &by_id);
        let mut imports = cross_imports.get(module).cloned().unwrap_or_default();
        prune_bucket_defined_imports(&mut imports, ids, &by_id);
        add_included_facade_imports(
            &mut imports,
            bucket_idents.as_ref(),
            &included_facade_items,
            &all_uses,
        );
        add_existing_module_imports(&mut imports, bucket_idents.as_ref(), &existing_modules);
        add_local_module_root_imports(
            &mut imports,
            bucket_idents.as_ref(),
            &local_module_roots,
            &external_mod_attrs,
            module,
            &all_uses,
        );
        add_macro_defined_imports(
            &mut imports,
            bucket_idents.as_ref(),
            &macro_defined_names,
            module,
        );
        add_parent_generated_sibling_imports(
            &mut imports,
            bucket_idents.as_ref(),
            &parent_generated_items,
            module,
            &all_uses,
        );
        add_trait_method_imports(
            &mut imports,
            bucket_idents.as_ref(),
            &trait_method_imports,
            module,
            &all_uses,
        );
        prune_bucket_defined_imports(&mut imports, ids, &by_id);
        let known_names = bucket_known_names(ids, &by_id, &imports);
        let bucket_prelude = bucket_use_prelude_from_idents(
            bucket_idents.as_ref(),
            &all_uses,
            &known_names,
            &local_module_roots,
        );
        let include_path_prefix = original
            .parent()
            .and_then(|parent| (!same_path(&target_dir, parent)).then_some("../"));
        let body = render_sub_file(
            module,
            ids,
            &by_id,
            &bucket_prelude,
            &promote,
            &crate_promote,
            &impl_lifts,
            &field_lifts,
            &imports,
            &local_module_roots,
            include_path_prefix,
        );
        fs::write(&path, body).with_context(|| format!("write {}", path.display()))?;
        written.push(path);
        if module_has_reexportable_items(ids, &by_id) {
            reexport_modules.insert(module.clone());
        }
        sub_modules.push(module.clone());
    }
    sub_modules.sort();
    let facade_uses: Vec<String> = all_uses.iter().map(|u| u.source.clone()).collect();

    let kept_target_dir = if sub_modules.is_empty() && layout.remove_target_dir_if_empty {
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
        &facade_root_items,
        &facade_primary,
        &sub_modules,
        &reexport_modules,
        layout.facade_path_prefix.as_deref(),
        &proc_macro_exports,
        &crate_root_imports,
        &promote,
        &impl_lifts,
        &facade_imports,
    );
    fs::write(original, facade_body)
        .with_context(|| format!("write facade {}", original.display()))?;

    Ok(WriteReport {
        backup,
        removed_file_backups,
        target_dir: kept_target_dir,
        written_files: written,
        facade: original.to_path_buf(),
    })
}

fn same_path(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

fn include_targets_planned_module(src: &str, planned_files: &BTreeSet<String>) -> bool {
    include_literal_paths(src).into_iter().any(|path| {
        Path::new(&path)
            .file_stem()
            .and_then(|stem| stem.to_str())
            .is_some_and(|stem| planned_files.contains(stem))
    })
}

fn include_literal_paths(src: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = src;
    while let Some(include_pos) = rest.find("include!") {
        rest = &rest[include_pos + "include!".len()..];
        let Some(start_rel) = rest.find('"') else {
            break;
        };
        let start = start_rel + 1;
        let Some(end_rel) = rest[start..].find('"') else {
            break;
        };
        let end = start + end_rel;
        out.push(rest[start..end].to_string());
        rest = &rest[end + 1..];
    }
    out
}

fn include_target_modules(items: &[ParsedItem]) -> BTreeSet<String> {
    items
        .iter()
        .flat_map(|item| include_literal_paths(&item.source))
        .filter_map(|path| {
            Path::new(&path)
                .file_stem()
                .and_then(|stem| stem.to_str())
                .map(String::from)
        })
        .collect()
}

fn included_facade_items(original: &Path, items: &[ParsedItem]) -> BTreeMap<String, Vec<String>> {
    let Some(parent) = original.parent() else {
        return BTreeMap::new();
    };
    let mut out: BTreeMap<String, Option<Vec<String>>> = BTreeMap::new();
    for rel in items
        .iter()
        .flat_map(|item| include_literal_paths(&item.source))
    {
        let path = parent.join(rel);
        if path.extension().and_then(|ext| ext.to_str()) != Some("rs") {
            continue;
        }
        let Ok(source) = fs::read_to_string(&path) else {
            continue;
        };
        let Ok(file) = syn::parse_file(&source) else {
            continue;
        };
        for item in &file.items {
            let Some((name, cfg_attrs)) = top_level_item_import(item) else {
                continue;
            };
            match out.get_mut(&name) {
                Some(slot) if slot.as_ref().is_some_and(|existing| existing == &cfg_attrs) => {}
                Some(slot) => *slot = None,
                None => {
                    out.insert(name, Some(cfg_attrs));
                }
            }
        }
    }
    out.into_iter()
        .filter_map(|(name, cfg_attrs)| cfg_attrs.map(|cfg_attrs| (name, cfg_attrs)))
        .collect()
}

fn external_crate_root_imports(
    original: &Path,
    assignments: &BTreeMap<String, Vec<ItemId>>,
    by_id: &BTreeMap<ItemId, &ParsedItem>,
    stem: &str,
    explicit_external_modules: &BTreeSet<String>,
) -> (BTreeSet<ItemId>, BTreeSet<CrossImport>) {
    let refs = external_module_crate_root_refs(original, by_id);
    if refs.is_empty() {
        return (BTreeSet::new(), BTreeSet::new());
    }
    let module_names: BTreeSet<String> = assignments
        .keys()
        .filter(|module| module.as_str() != "mod_root" && module.as_str() != stem)
        .cloned()
        .chain(explicit_external_modules.iter().cloned())
        .collect();
    let mut promote = BTreeSet::new();
    let mut imports = BTreeSet::new();
    for (module, ids) in assignments {
        if module == "mod_root" || module == stem {
            continue;
        }
        for id in ids {
            let item = by_id[id];
            if item.name.is_empty()
                || module_names.contains(item.name.as_str())
                || !refs.contains(item.name.as_str())
                || item.vis != ItemVis::Private
                || !needs_keyword_rewrite(item)
            {
                continue;
            }
            promote.insert(*id);
            imports.insert(CrossImport {
                source_bucket: Some(module.clone()),
                name: item.name.clone(),
                cfg_attrs: cfg_attrs_of(&item.source),
            });
        }
    }
    (promote, imports)
}

fn external_module_crate_root_refs(
    original: &Path,
    by_id: &BTreeMap<ItemId, &ParsedItem>,
) -> BTreeSet<String> {
    let mut refs = BTreeSet::new();
    for path in external_module_paths(original, by_id) {
        let Ok(source) = fs::read_to_string(&path) else {
            continue;
        };
        refs.extend(crate_root_refs_in_source(&source));
    }
    refs
}

fn external_module_paths(original: &Path, by_id: &BTreeMap<ItemId, &ParsedItem>) -> Vec<PathBuf> {
    let Some(parent) = original.parent() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for item in by_id.values().filter(|item| item.is_external_mod_decl()) {
        let Ok(syn::Item::Mod(module)) = syn::parse_str::<syn::Item>(&item.source) else {
            continue;
        };
        if let Some(path) = module
            .attrs
            .iter()
            .find(|attr| attr.path().is_ident("path"))
            .and_then(|attr| attr.parse_args::<syn::LitStr>().ok())
            .map(|lit| parent.join(lit.value()))
        {
            out.push(path);
            continue;
        }
        let file = parent.join(format!("{}.rs", item.name));
        if file.is_file() {
            out.push(file);
            continue;
        }
        let mod_file = parent.join(&item.name).join("mod.rs");
        if mod_file.is_file() {
            out.push(mod_file);
        }
    }
    out
}

fn crate_root_refs_in_source(source: &str) -> BTreeSet<String> {
    let Ok(file) = syn::parse_file(source) else {
        return BTreeSet::new();
    };
    let mut visitor = CrateRootRefVisitor {
        refs: BTreeSet::new(),
    };
    syn::visit::Visit::visit_file(&mut visitor, &file);
    visitor.refs
}

struct CrateRootRefVisitor {
    refs: BTreeSet<String>,
}

impl<'ast> syn::visit::Visit<'ast> for CrateRootRefVisitor {
    fn visit_item_use(&mut self, item: &'ast syn::ItemUse) {
        collect_crate_use_tree(&item.tree, false, &mut self.refs);
    }

    fn visit_path(&mut self, path: &'ast syn::Path) {
        let mut segments = path.segments.iter();
        if segments
            .next()
            .is_some_and(|segment| segment.ident == "crate")
            && let Some(segment) = segments.next()
        {
            self.refs.insert(segment.ident.to_string());
        }
        syn::visit::visit_path(self, path);
    }
}

fn collect_crate_use_tree(tree: &syn::UseTree, under_crate: bool, refs: &mut BTreeSet<String>) {
    match tree {
        syn::UseTree::Path(path) if path.ident == "crate" && !under_crate => {
            collect_crate_use_tree(path.tree.as_ref(), true, refs);
        }
        syn::UseTree::Path(path) if under_crate => {
            refs.insert(path.ident.to_string());
        }
        syn::UseTree::Path(path) => collect_crate_use_tree(path.tree.as_ref(), false, refs),
        syn::UseTree::Name(name) if under_crate => {
            refs.insert(name.ident.to_string());
        }
        syn::UseTree::Rename(rename) if under_crate => {
            refs.insert(rename.ident.to_string());
        }
        syn::UseTree::Group(group) => {
            for item in &group.items {
                collect_crate_use_tree(item, under_crate, refs);
            }
        }
        _ => {}
    }
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
fn bucket_use_prelude_from_idents(
    idents: Option<&HashSet<String>>,
    all_uses: &[&ParsedItem],
    known_names: &HashSet<String>,
    local_module_roots: &HashSet<String>,
) -> String {
    let Some(idents) = idents else {
        return all_uses
            .iter()
            .map(|u| uses::rebase_use_for_subfile(&u.source))
            .collect::<Vec<_>>()
            .join("\n");
    };
    uses::render_uses_for_with_local_roots(all_uses, idents, known_names, true, local_module_roots)
}

fn add_existing_module_imports(
    imports: &mut BTreeSet<CrossImport>,
    idents: Option<&HashSet<String>>,
    existing_modules: &BTreeSet<String>,
) {
    let Some(idents) = idents else {
        return;
    };
    for module in existing_modules {
        if idents.contains(module) {
            imports.insert(CrossImport {
                source_bucket: None,
                name: module.clone(),
                cfg_attrs: Vec::new(),
            });
        }
    }
}

fn add_included_facade_imports(
    imports: &mut BTreeSet<CrossImport>,
    idents: Option<&HashSet<String>>,
    included_facade_items: &BTreeMap<String, Vec<String>>,
    all_uses: &[&ParsedItem],
) {
    let Some(idents) = idents else {
        return;
    };
    let imported_names: HashSet<String> =
        imports.iter().map(|import| import.name.clone()).collect();
    let use_bound_names: HashSet<String> = all_uses
        .iter()
        .flat_map(|use_item| uses::use_bindings(use_item))
        .collect();
    for (name, cfg_attrs) in included_facade_items {
        if idents.contains(name)
            && !imported_names.contains(name.as_str())
            && !use_bound_names.contains(name.as_str())
        {
            imports.insert(CrossImport {
                source_bucket: None,
                name: name.clone(),
                cfg_attrs: cfg_attrs.clone(),
            });
        }
    }
}

fn prune_bucket_defined_imports(
    imports: &mut BTreeSet<CrossImport>,
    ids: &[ItemId],
    by_id: &BTreeMap<ItemId, &ParsedItem>,
) {
    let local_names: HashSet<String> = ids
        .iter()
        .filter_map(|id| {
            let name = &by_id[id].name;
            (!name.is_empty()).then(|| name.clone())
        })
        .collect();
    if local_names.is_empty() {
        return;
    }
    imports.retain(|import| !local_names.contains(import.name.as_str()));
}

fn add_local_module_root_imports(
    imports: &mut BTreeSet<CrossImport>,
    idents: Option<&HashSet<String>>,
    local_module_roots: &HashSet<String>,
    external_mod_attrs: &BTreeMap<String, Vec<String>>,
    current_module: &str,
    all_uses: &[&ParsedItem],
) {
    let Some(idents) = idents else {
        return;
    };
    let imported_names: HashSet<String> =
        imports.iter().map(|import| import.name.clone()).collect();
    let use_bound_names: HashSet<String> = all_uses
        .iter()
        .flat_map(|use_item| uses::use_bindings(use_item))
        .collect();
    for module in local_module_roots {
        if module != current_module
            && idents.contains(module)
            && !imported_names.contains(module.as_str())
            && !use_bound_names.contains(module.as_str())
        {
            imports.insert(CrossImport {
                source_bucket: None,
                name: module.clone(),
                cfg_attrs: external_mod_attrs.get(module).cloned().unwrap_or_default(),
            });
        }
    }
}

fn add_macro_defined_imports(
    imports: &mut BTreeSet<CrossImport>,
    idents: Option<&HashSet<String>>,
    macro_defined_names: &BTreeMap<String, Option<String>>,
    current_module: &str,
) {
    let Some(idents) = idents else {
        return;
    };
    for (name, source_bucket) in macro_defined_names {
        if idents.contains(name) && source_bucket.as_deref() != Some(current_module) {
            imports.insert(CrossImport {
                source_bucket: source_bucket.clone(),
                name: name.clone(),
                cfg_attrs: Vec::new(),
            });
        }
    }
}

#[derive(Clone, Debug)]
struct ParentGeneratedItemImport {
    source_bucket: String,
    cfg_attrs: Vec<String>,
}

#[derive(Clone, Debug)]
struct TraitMethodImport {
    source_bucket: Option<String>,
    name: String,
    item_id: ItemId,
    cfg_attrs: Vec<String>,
}

fn add_trait_method_imports(
    imports: &mut BTreeSet<CrossImport>,
    idents: Option<&HashSet<String>>,
    trait_method_imports: &BTreeMap<String, Vec<TraitMethodImport>>,
    current_module: &str,
    all_uses: &[&ParsedItem],
) {
    let Some(idents) = idents else {
        return;
    };
    let imported_names: HashSet<String> =
        imports.iter().map(|import| import.name.clone()).collect();
    let use_bound_names: HashSet<String> = all_uses
        .iter()
        .flat_map(|use_item| uses::use_bindings(use_item))
        .collect();
    for method in idents {
        let Some(candidates) = trait_method_imports.get(method) else {
            continue;
        };
        for candidate in candidates {
            if candidate.source_bucket.as_deref() == Some(current_module)
                || imported_names.contains(candidate.name.as_str())
                || use_bound_names.contains(candidate.name.as_str())
            {
                continue;
            }
            imports.insert(CrossImport {
                source_bucket: candidate.source_bucket.clone(),
                name: candidate.name.clone(),
                cfg_attrs: candidate.cfg_attrs.clone(),
            });
        }
    }
}

fn trait_method_imports_by_method(
    assignments: &BTreeMap<String, Vec<ItemId>>,
    by_id: &BTreeMap<ItemId, &ParsedItem>,
    stem: &str,
) -> BTreeMap<String, Vec<TraitMethodImport>> {
    let mut out: BTreeMap<String, Vec<TraitMethodImport>> = BTreeMap::new();
    for (bucket, ids) in assignments {
        if bucket == "mod_root" {
            continue;
        }
        let source_bucket = (bucket != stem).then(|| bucket.clone());
        for id in ids {
            let item = by_id[id];
            if !matches!(item.kind, ItemKind::Trait) {
                continue;
            }
            let methods = trait_method_names(&item.source);
            if methods.is_empty() {
                continue;
            }
            let import = TraitMethodImport {
                source_bucket: source_bucket.clone(),
                name: item.name.clone(),
                item_id: item.id,
                cfg_attrs: cfg_attrs_of(&item.source),
            };
            for method in methods {
                out.entry(method).or_default().push(import.clone());
            }
        }
    }
    out
}

fn add_trait_method_promotions(
    promote: &mut BTreeSet<ItemId>,
    assignments: &BTreeMap<String, Vec<ItemId>>,
    by_id: &BTreeMap<ItemId, &ParsedItem>,
    stem: &str,
    trait_method_imports: &BTreeMap<String, Vec<TraitMethodImport>>,
) {
    for (bucket, ids) in assignments {
        if bucket == "mod_root" || bucket == stem {
            continue;
        }
        let Some(idents) = uses::bucket_idents_for(ids, by_id) else {
            continue;
        };
        for method in idents {
            let Some(candidates) = trait_method_imports.get(&method) else {
                continue;
            };
            for candidate in candidates {
                if candidate.source_bucket.as_deref() == Some(bucket)
                    || candidate.source_bucket.is_none()
                {
                    continue;
                }
                let item = by_id[&candidate.item_id];
                if item.vis == ItemVis::Private && needs_keyword_rewrite(item) {
                    promote.insert(candidate.item_id);
                }
            }
        }
    }
}

fn trait_method_names(source: &str) -> Vec<String> {
    let Ok(syn::Item::Trait(item)) = syn::parse_str::<syn::Item>(source) else {
        return Vec::new();
    };
    item.items
        .iter()
        .filter_map(|item| {
            let syn::TraitItem::Fn(item) = item else {
                return None;
            };
            Some(item.sig.ident.to_string())
        })
        .collect()
}

fn add_parent_generated_sibling_imports(
    imports: &mut BTreeSet<CrossImport>,
    idents: Option<&HashSet<String>>,
    parent_generated_items: &BTreeMap<String, Option<ParentGeneratedItemImport>>,
    current_module: &str,
    all_uses: &[&ParsedItem],
) {
    let Some(idents) = idents else {
        return;
    };
    if !all_uses
        .iter()
        .any(|use_item| is_super_glob_use(&use_item.source))
    {
        return;
    }
    let imported_names: HashSet<String> =
        imports.iter().map(|import| import.name.clone()).collect();
    let use_bound_names: HashSet<String> = all_uses
        .iter()
        .flat_map(|use_item| uses::use_bindings(use_item))
        .collect();
    for (name, import) in parent_generated_items {
        if !idents.contains(name)
            || imported_names.contains(name.as_str())
            || use_bound_names.contains(name.as_str())
        {
            continue;
        }
        let Some(import) = import else {
            continue;
        };
        if import.source_bucket == current_module {
            continue;
        }
        imports.insert(CrossImport {
            source_bucket: Some(import.source_bucket.clone()),
            name: name.clone(),
            cfg_attrs: import.cfg_attrs.clone(),
        });
    }
}

fn parent_generated_sibling_items(
    original: &Path,
) -> BTreeMap<String, Option<ParentGeneratedItemImport>> {
    let mut out: BTreeMap<String, Option<ParentGeneratedItemImport>> = BTreeMap::new();
    let Some(parent) = original.parent() else {
        return out;
    };
    let Ok(entries) = fs::read_dir(parent) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if same_path(&path, original) || path.extension().and_then(|s| s.to_str()) != Some("rs") {
            continue;
        }
        let Some(module) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        let Ok(source) = fs::read_to_string(&path) else {
            continue;
        };
        if !source.contains("Auto-generated by r2factor") {
            continue;
        }
        let Ok(file) = syn::parse_file(&source) else {
            continue;
        };
        for item in &file.items {
            let Some((name, cfg_attrs)) = visible_top_level_item_import(item) else {
                continue;
            };
            let import = ParentGeneratedItemImport {
                source_bucket: format!("super::{module}"),
                cfg_attrs,
            };
            match out.get_mut(&name) {
                Some(slot)
                    if slot
                        .as_ref()
                        .is_some_and(|existing| existing.source_bucket == import.source_bucket) => {
                }
                Some(slot) => *slot = None,
                None => {
                    out.insert(name, Some(import));
                }
            }
        }
    }
    out
}

fn visible_top_level_item_import(item: &syn::Item) -> Option<(String, Vec<String>)> {
    let (name, visibility, attrs) = match item {
        syn::Item::Const(item) => (&item.ident, &item.vis, &item.attrs),
        syn::Item::Enum(item) => (&item.ident, &item.vis, &item.attrs),
        syn::Item::Fn(item) => (&item.sig.ident, &item.vis, &item.attrs),
        syn::Item::Static(item) => (&item.ident, &item.vis, &item.attrs),
        syn::Item::Struct(item) => (&item.ident, &item.vis, &item.attrs),
        syn::Item::Trait(item) => (&item.ident, &item.vis, &item.attrs),
        syn::Item::Type(item) => (&item.ident, &item.vis, &item.attrs),
        syn::Item::Union(item) => (&item.ident, &item.vis, &item.attrs),
        _ => return None,
    };
    if !is_visible_from_descendant(visibility) {
        return None;
    }
    Some((name.to_string(), cfg_attrs_from_attrs(attrs)))
}

fn top_level_item_import(item: &syn::Item) -> Option<(String, Vec<String>)> {
    let (name, attrs) = match item {
        syn::Item::Const(item) => (&item.ident, &item.attrs),
        syn::Item::Enum(item) => (&item.ident, &item.attrs),
        syn::Item::Fn(item) => (&item.sig.ident, &item.attrs),
        syn::Item::Static(item) => (&item.ident, &item.attrs),
        syn::Item::Struct(item) => (&item.ident, &item.attrs),
        syn::Item::Trait(item) => (&item.ident, &item.attrs),
        syn::Item::Type(item) => (&item.ident, &item.attrs),
        syn::Item::Union(item) => (&item.ident, &item.attrs),
        _ => return None,
    };
    Some((name.to_string(), cfg_attrs_from_attrs(attrs)))
}

fn cfg_attrs_from_attrs(attrs: &[syn::Attribute]) -> Vec<String> {
    attrs
        .iter()
        .filter(|attr| attr.path().is_ident("cfg"))
        .map(|attr| attr.to_token_stream().to_string())
        .collect()
}

fn is_visible_from_descendant(vis: &syn::Visibility) -> bool {
    match vis {
        syn::Visibility::Public(_) => true,
        syn::Visibility::Restricted(restricted) => {
            let segments: Vec<String> = restricted
                .path
                .segments
                .iter()
                .map(|segment| segment.ident.to_string())
                .collect();
            segments != ["self"]
        }
        syn::Visibility::Inherited => false,
    }
}

fn is_super_glob_use(source: &str) -> bool {
    let Ok(file) = syn::parse_file(source) else {
        return false;
    };
    file.items.iter().any(|item| {
        let syn::Item::Use(item) = item else {
            return false;
        };
        use_tree_has_super_glob(&item.tree)
    })
}

fn use_tree_has_super_glob(tree: &syn::UseTree) -> bool {
    match tree {
        syn::UseTree::Path(path) if path.ident == "super" => {
            matches!(path.tree.as_ref(), syn::UseTree::Glob(_))
                || use_tree_has_super_glob(path.tree.as_ref())
        }
        syn::UseTree::Path(path) => use_tree_has_super_glob(path.tree.as_ref()),
        syn::UseTree::Group(group) => group.items.iter().any(use_tree_has_super_glob),
        _ => false,
    }
}

fn bucket_known_names(
    ids: &[ItemId],
    by_id: &BTreeMap<ItemId, &ParsedItem>,
    imports: &BTreeSet<CrossImport>,
) -> HashSet<String> {
    let mut names: HashSet<String> = ids
        .iter()
        .filter_map(|id| {
            let name = &by_id[id].name;
            (!name.is_empty()).then(|| name.clone())
        })
        .collect();
    for import in imports {
        names.insert(import.name.clone());
    }
    names
}

fn module_has_reexportable_items(ids: &[ItemId], by_id: &BTreeMap<ItemId, &ParsedItem>) -> bool {
    ids.iter().any(|id| {
        let item = by_id[id];
        !proc_macro_entry::is_proc_macro_entry(item)
            && (item.vis != crate::item::ItemVis::Private
                || matches!(item.kind, ItemKind::ForeignMod))
    })
}

fn external_mod_decl_attrs(by_id: &BTreeMap<ItemId, &ParsedItem>) -> BTreeMap<String, Vec<String>> {
    by_id
        .values()
        .filter(|item| item.is_external_mod_decl())
        .map(|item| (item.name.clone(), cfg_attrs_of(&item.source)))
        .collect()
}

fn macro_defined_names_by_source(
    assignments: &BTreeMap<String, Vec<ItemId>>,
    by_id: &BTreeMap<ItemId, &ParsedItem>,
    stem: &str,
) -> BTreeMap<String, Option<String>> {
    let mut out = BTreeMap::new();
    for (bucket, ids) in assignments {
        let source_bucket = (!(bucket == "mod_root" || bucket == stem)).then(|| bucket.clone());
        for id in ids {
            let item = by_id[id];
            if !matches!(item.kind, ItemKind::Macro) {
                continue;
            }
            for name in macro_defined_names(&item.source) {
                out.insert(name, source_bucket.clone());
            }
        }
    }
    out
}

fn macro_defined_names(source: &str) -> Vec<String> {
    if is_macro_rules_definition_source(source) {
        return Vec::new();
    }
    let Ok(syn::Item::Macro(item)) = syn::parse_str::<syn::Item>(source) else {
        return Vec::new();
    };
    let Some(last) = item.mac.path.segments.last() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    if last.ident == "thread_local" {
        out.extend(thread_local_static_names(&item.mac.tokens));
    }
    out.extend(macro_fn_names(&item.mac.tokens));
    out.sort();
    out.dedup();
    out
}

fn is_macro_rules_definition(item: &ParsedItem) -> bool {
    matches!(item.kind, ItemKind::Macro) && is_macro_rules_definition_source(&item.source)
}

fn is_macro_rules_definition_source(source: &str) -> bool {
    let Ok(syn::Item::Macro(item)) = syn::parse_str::<syn::Item>(source) else {
        return false;
    };
    item.ident.is_some()
        && item
            .mac
            .path
            .segments
            .last()
            .is_some_and(|segment| segment.ident == "macro_rules")
}

fn thread_local_static_names(tokens: &proc_macro2::TokenStream) -> Vec<String> {
    use proc_macro2::TokenTree;

    let mut out = Vec::new();
    let mut expect_name = false;
    for token in tokens.clone() {
        match token {
            TokenTree::Ident(ident) if expect_name => {
                out.push(ident.to_string());
                expect_name = false;
            }
            TokenTree::Ident(ident) if ident == "static" => {
                expect_name = true;
            }
            TokenTree::Group(group) => {
                out.extend(thread_local_static_names(&group.stream()));
                expect_name = false;
            }
            _ => {
                if expect_name {
                    expect_name = false;
                }
            }
        }
    }
    out
}

fn macro_fn_names(tokens: &proc_macro2::TokenStream) -> Vec<String> {
    use proc_macro2::TokenTree;

    let mut out = Vec::new();
    let mut expect_name = false;
    for token in tokens.clone() {
        match token {
            TokenTree::Ident(ident) if expect_name => {
                out.push(ident.to_string());
                expect_name = false;
            }
            TokenTree::Ident(ident) if ident == "fn" => {
                expect_name = true;
            }
            TokenTree::Group(group) => {
                out.extend(macro_fn_names(&group.stream()));
                expect_name = false;
            }
            _ => {
                if expect_name {
                    expect_name = false;
                }
            }
        }
    }
    out
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
    items: &[ParsedItem],
    all_uses: &[&ParsedItem],
    stem: &str,
    explicit_external_modules: &BTreeSet<String>,
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
        for root in uses::use_roots(u) {
            imported.insert(root);
        }
    }
    for item in items {
        for root in uses::path_roots_in_source(&item.source) {
            imported.insert(root);
        }
    }
    let mut renames: BTreeMap<String, String> = BTreeMap::new();
    for bucket in plan.assignments.keys() {
        if bucket == "mod_root" || bucket == stem || bucket == "tests" {
            continue;
        }
        if imported.contains(bucket) || explicit_external_modules.contains(bucket) {
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
        let key = renames
            .get(bucket)
            .cloned()
            .unwrap_or_else(|| bucket.clone());
        out.entry(key).or_default().extend(ids.iter().copied());
    }
    out
}

fn proc_macro_exports(
    assignments: &BTreeMap<String, Vec<ItemId>>,
    by_id: &BTreeMap<ItemId, &ParsedItem>,
    stem: &str,
) -> Vec<String> {
    let mut exports = Vec::new();
    for (module, ids) in assignments {
        if module == "mod_root" || module == stem {
            continue;
        }
        for id in ids {
            let item = by_id[id];
            if proc_macro_entry::is_proc_macro_entry(item)
                && let Some(wrapper) = proc_macro_entry::wrapper_source(&item.source, module)
            {
                exports.push(wrapper);
            }
        }
    }
    exports
}

fn force_facade_pinned_items(
    plan: &Plan,
    items: &[ParsedItem],
    stem: &str,
    keep_main_in_facade: bool,
) -> Plan {
    let by_id: BTreeMap<ItemId, &ParsedItem> = items.iter().map(|it| (it.id, it)).collect();
    let mut adjusted = Plan {
        assignments: BTreeMap::new(),
        rationale: plan.rationale.clone(),
    };
    for (module, ids) in &plan.assignments {
        for id in ids {
            let item = by_id[id];
            if keep_main_in_facade
                && item.name == "main"
                && matches!(item.kind, ItemKind::Fn { .. })
            {
                adjusted.assign(stem, *id, "crate-root main stays in facade");
            } else if matches!(item.kind, ItemKind::ExternCrate) {
                adjusted.assign("mod_root", *id, "extern crate declaration stays in facade");
            } else if is_macro_rules_definition(item) {
                adjusted.assign(
                    "mod_root",
                    *id,
                    "macro_rules definition stays before modules",
                );
            } else if is_codegen_visible_repr_data(item) {
                adjusted.assign(
                    stem,
                    *id,
                    "repr ABI item stays in facade for source-scanning codegen",
                );
            } else if item.is_docify_export() {
                adjusted.assign(
                    stem,
                    *id,
                    "docify export stays in facade so embed paths remain valid",
                );
            } else {
                adjusted
                    .assignments
                    .entry(module.clone())
                    .or_default()
                    .push(*id);
            }
        }
    }
    adjusted
}

fn is_codegen_visible_repr_data(item: &ParsedItem) -> bool {
    if !matches!(
        item.kind,
        ItemKind::Struct | ItemKind::Enum | ItemKind::Union
    ) {
        return false;
    }
    let Ok(parsed) = syn::parse_str::<syn::Item>(&item.source) else {
        return false;
    };
    let attrs = match &parsed {
        syn::Item::Struct(item) => &item.attrs,
        syn::Item::Enum(item) => &item.attrs,
        syn::Item::Union(item) => &item.attrs,
        _ => return false,
    };
    attrs.iter().any(|attr| {
        attr.path().is_ident("repr")
            && attr
                .meta
                .to_token_stream()
                .to_string()
                .split(|ch: char| !ch.is_ascii_alphanumeric() && ch != '_')
                .any(|token| token == "C" || token == "transparent")
    })
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
            let src_new = c
                .source_bucket
                .and_then(|source| renames.get(&source).cloned().or(Some(source)));
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
            let src_new = c
                .source_bucket
                .and_then(|source| renames.get(&source).cloned().or(Some(source)));
            CrossImport {
                source_bucket: src_new,
                name: c.name,
                cfg_attrs: c.cfg_attrs,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_private_helpers_imported_by_existing_sibling_module() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let original = tmp.path().join("sample.rs");
        let source = "fn tet_edges() {}\nfn tet_faces() {}\n";
        fs::write(&original, source).expect("write source");
        let target = tmp.path().join("sample");
        fs::create_dir_all(&target).expect("mkdir target");
        fs::write(
            target.join("recovery.rs"),
            "use super::{tet_edges, tet_faces};\nfn use_helpers() { tet_edges(); tet_faces(); }\n",
        )
        .expect("write sibling");

        let items = crate::item::parse_file(source).expect("parse source");
        let mut plan = Plan::default();
        for item in &items {
            plan.assign("tet", item.id, "test helper bucket");
        }

        write_plan(
            &original,
            &plan,
            &items,
            &WriteOptions {
                force: false,
                recursive_max_lines: None,
            },
        )
        .expect("write split");

        let facade = fs::read_to_string(&original).expect("read facade");
        assert!(facade.contains("use tet::tet_edges;"), "facade={facade}");
        assert!(facade.contains("use tet::tet_faces;"), "facade={facade}");
        let helpers = fs::read_to_string(target.join("tet.rs")).expect("read helper module");
        assert!(
            helpers.contains("pub(super) fn tet_edges"),
            "helpers={helpers}"
        );
        assert!(
            helpers.contains("pub(super) fn tet_faces"),
            "helpers={helpers}"
        );
    }

    #[test]
    fn sibling_import_scan_ignores_nested_super_paths() {
        let mut found = BTreeSet::new();
        let parsed = syn::parse_str::<syn::ItemUse>(
            "use super::{direct, nested::not_direct, other as renamed};",
        )
        .expect("parse import");
        direct_super_use_bindings(&parsed.tree, &mut found);
        assert_eq!(
            found,
            BTreeSet::from(["direct".to_string(), "other".to_string()])
        );
    }
}
