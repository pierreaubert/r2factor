//! Materialize a `Plan` to disk. The orchestrator [`write_plan`] handles
//! ordering (backup before destruction, sub-files before facade) and stale-
//! file cleanup; the rendering helpers live in submodules.

mod backup;
mod facade;
mod preamble;
mod subfile;

use crate::item::{ItemId, ItemKind, ParsedItem};
use crate::plan::Plan;
use anyhow::{Context, Result, anyhow, bail};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use backup::make_backup_path;
use facade::render_facade;
use preamble::extract_inner_attrs;
use subfile::render_sub_file;

pub struct WriteOptions {
    pub force: bool,
}

#[derive(Debug)]
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
    let target_dir = parent.join(stem);
    if target_dir.exists() && !opts.force {
        bail!(
            "target dir {} already exists; pass --force to overwrite",
            target_dir.display()
        );
    }

    // 1) Backup FIRST. If this fails, we abort with no destruction.
    let backup = make_backup_path(original)?;
    fs::copy(original, &backup).with_context(|| {
        format!("backup {} -> {}", original.display(), backup.display())
    })?;

    // With --force, wipe a previous split's leftover .rs files so the
    // generated tree matches the new plan. We only remove top-level .rs
    // files (not subdirs the user may have added) to stay conservative.
    if opts.force && target_dir.is_dir() {
        purge_stale_rs(&target_dir)?;
    }

    // 2) Source preamble: inner attrs/doc-mod comments, plus the file's `use`s.
    let by_id: BTreeMap<ItemId, &ParsedItem> = items.iter().map(|i| (i.id, i)).collect();
    let original_src = fs::read_to_string(original)?;
    let inner_attrs = extract_inner_attrs(&original_src);
    let use_prelude: String = items
        .iter()
        .filter(|i| matches!(i.kind, ItemKind::Use))
        .map(|i| i.source.clone())
        .collect::<Vec<_>>()
        .join("\n");

    // 3) Sub-files.
    fs::create_dir_all(&target_dir)
        .with_context(|| format!("mkdir {}", target_dir.display()))?;

    let mut written: Vec<PathBuf> = Vec::new();
    let mut sub_modules: Vec<String> = Vec::new();
    let mut facade_uses: Vec<&ParsedItem> = Vec::new();
    let mut facade_primary: Vec<&ParsedItem> = Vec::new();

    for (module, ids) in &plan.assignments {
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
        let body = render_sub_file(ids, &by_id, &use_prelude);
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

    // 4) Facade: replace the original file.
    let facade_body = render_facade(&inner_attrs, &facade_uses, &facade_primary, &sub_modules);
    fs::write(original, facade_body)
        .with_context(|| format!("write facade {}", original.display()))?;

    Ok(WriteReport {
        backup,
        target_dir: kept_target_dir,
        written_files: written,
        facade: original.to_path_buf(),
    })
}

fn purge_stale_rs(dir: &Path) -> Result<()> {
    for entry in fs::read_dir(dir).with_context(|| format!("read_dir {}", dir.display()))? {
        let entry = entry.with_context(|| format!("read_dir entry in {}", dir.display()))?;
        let path = entry.path();
        if path.is_file() && path.extension().is_some_and(|ext| ext == "rs") {
            fs::remove_file(&path)
                .with_context(|| format!("remove stale {}", path.display()))?;
        }
    }
    Ok(())
}
