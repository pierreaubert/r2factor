use anyhow::{Context, Result, bail};
use std::fs;
use std::path::PathBuf;

use super::impact::{ConsumerRewritePlan, ConsumerRewriteReport, SkippedConsumerRewrite};
use super::parent::make_backup_path;

#[derive(Debug, serde::Serialize)]
pub struct CombineWriteReport {
    pub module_name: String,
    pub facade_path: PathBuf,
    pub moved_files: Vec<MovedFile>,
    pub parent_update: Option<ParentUpdate>,
    pub consumer_rewrites: Vec<ConsumerRewriteReport>,
    pub skipped_consumer_rewrites: Vec<SkippedConsumerRewrite>,
    pub backups: Vec<PathBuf>,
    pub manifest: OperationManifest,
}

#[derive(Debug, serde::Serialize)]
pub struct MovedFile {
    pub from: PathBuf,
    pub to: PathBuf,
}

#[derive(Debug, serde::Serialize)]
pub struct ParentUpdate {
    pub path: PathBuf,
    pub add: String,
    pub remove: Vec<String>,
}

#[derive(Debug, Default, serde::Serialize)]
pub struct OperationManifest {
    pub created_dirs: Vec<PathBuf>,
    pub written_files: Vec<PathBuf>,
    pub removed_files: Vec<PathBuf>,
    pub updated_files: Vec<PathBuf>,
    pub preserved_files: Vec<PathBuf>,
}

pub struct WriteOptions {
    pub force: bool,
}

#[derive(Clone)]
struct BackupRecord {
    path: PathBuf,
    backup: PathBuf,
    existed_before: bool,
}

#[derive(Default)]
struct OperationState {
    backups: Vec<BackupRecord>,
    manifest: OperationManifest,
}

struct WritePayload<'a> {
    facade_src: &'a str,
    file_srcs: &'a [String],
    parent_src: Option<&'a str>,
    consumer_rewrite_plan: &'a ConsumerRewritePlan,
}

impl OperationState {
    fn backup_path(&mut self, path: PathBuf) -> Result<PathBuf> {
        let backup = make_backup_path(&path)?;
        let existed_before = path.exists();
        if existed_before {
            fs::copy(&path, &backup)
                .with_context(|| format!("backup {} -> {}", path.display(), backup.display()))?;
        }
        self.backups.push(BackupRecord {
            path,
            backup: backup.clone(),
            existed_before,
        });
        Ok(backup)
    }

    fn write_file(&mut self, path: PathBuf, content: &str) -> Result<()> {
        fs::write(&path, content).with_context(|| format!("write {}", path.display()))?;
        self.manifest.written_files.push(path);
        Ok(())
    }

    fn remove_file(&mut self, path: PathBuf) -> Result<()> {
        fs::remove_file(&path).with_context(|| format!("remove {}", path.display()))?;
        self.manifest.removed_files.push(path);
        Ok(())
    }

    fn rollback(&self) {
        for backup in self.backups.iter().rev() {
            if backup.existed_before {
                let _ = fs::copy(&backup.backup, &backup.path);
            } else {
                let _ = fs::remove_file(&backup.path);
            }
        }
        for path in self.manifest.written_files.iter().rev() {
            if !self
                .backups
                .iter()
                .any(|b| b.path == *path && b.existed_before)
            {
                let _ = fs::remove_file(path);
            }
        }
        for dir in self.manifest.created_dirs.iter().rev() {
            let _ = fs::remove_dir(dir);
        }
    }

    fn backup_paths(&self) -> Vec<PathBuf> {
        self.backups
            .iter()
            .filter(|b| b.existed_before)
            .map(|b| b.backup.clone())
            .collect()
    }

    fn is_clean(&self) -> bool {
        self.backups.is_empty()
            && self.manifest.created_dirs.is_empty()
            && self.manifest.written_files.is_empty()
            && self.manifest.removed_files.is_empty()
    }
}

/// Execute the combine write with conservative overwrite semantics. Force mode
/// only overwrites the files this operation plans to write; unrelated files in
/// the target directory are preserved.
pub fn execute_write(
    plan: &super::plan::CombinePlan,
    facade_src: &str,
    file_srcs: &[String],
    parent_src: Option<&str>,
    consumer_rewrite_plan: &ConsumerRewritePlan,
    opts: &WriteOptions,
) -> Result<CombineWriteReport> {
    let mut op = OperationState::default();
    let payload = WritePayload {
        facade_src,
        file_srcs,
        parent_src,
        consumer_rewrite_plan,
    };
    match execute_write_inner(plan, &payload, opts, &mut op) {
        Ok(report) => Ok(report),
        Err(e) => {
            if op.is_clean() {
                Err(e)
            } else {
                op.rollback();
                Err(e.context("combine write failed; attempted rollback from backups"))
            }
        }
    }
}

fn execute_write_inner(
    plan: &super::plan::CombinePlan,
    payload: &WritePayload<'_>,
    opts: &WriteOptions,
    op: &mut OperationState,
) -> Result<CombineWriteReport> {
    let file_dsts: Vec<PathBuf> = plan
        .files
        .iter()
        .map(|file| {
            file.file_name()
                .map(|name| plan.target_dir.join(name))
                .context("input file has no filename")
        })
        .collect::<Result<_>>()?;
    let mut planned_targets = Vec::with_capacity(file_dsts.len() + 1);
    planned_targets.push(plan.facade_path.clone());
    planned_targets.extend(file_dsts.iter().cloned());

    if plan.target_dir.exists() && !opts.force {
        bail!(
            "target directory already exists: {}. Use --force to overwrite.",
            plan.target_dir.display()
        );
    }
    if !plan.target_dir.exists() {
        fs::create_dir_all(&plan.target_dir)
            .with_context(|| format!("mkdir {}", plan.target_dir.display()))?;
        op.manifest.created_dirs.push(plan.target_dir.clone());
    } else {
        for entry in fs::read_dir(&plan.target_dir)
            .with_context(|| format!("read_dir {}", plan.target_dir.display()))?
        {
            let entry = entry?;
            let path = entry.path();
            if path.is_file() && !planned_targets.iter().any(|p| same_path(p, &path)) {
                op.manifest.preserved_files.push(path);
            }
        }
    }

    let mut backups = Vec::new();
    for path in &planned_targets {
        if path.exists() {
            backups.push(op.backup_path(path.clone())?);
        }
    }

    let parent_update = if let Some(parent_path) = &plan.parent_module {
        if let Some(parent_src) = payload.parent_src {
            backups.push(op.backup_path(parent_path.clone())?);
            op.write_file(parent_path.clone(), parent_src)?;
            op.manifest.updated_files.push(parent_path.clone());

            let stems = plan
                .files
                .iter()
                .filter_map(|file| file.file_stem()?.to_str())
                .collect::<Vec<_>>();
            Some(ParentUpdate {
                path: parent_path.clone(),
                add: format!("mod {};", plan.module_name),
                remove: stems.iter().map(|stem| format!("mod {stem};")).collect(),
            })
        } else {
            None
        }
    } else {
        None
    };

    op.write_file(plan.facade_path.clone(), payload.facade_src)?;

    for ((file, dst), src) in plan
        .files
        .iter()
        .zip(file_dsts.iter())
        .zip(payload.file_srcs.iter())
    {
        backups.push(op.backup_path(file.clone())?);
        op.write_file(dst.clone(), src)?;
        op.remove_file(file.clone())?;
    }

    let mut consumer_reports = Vec::new();
    for rewrite in &payload.consumer_rewrite_plan.rewrites {
        let backup = op.backup_path(rewrite.file.clone())?;
        op.write_file(rewrite.file.clone(), &rewrite.new_source)?;
        op.manifest.updated_files.push(rewrite.file.clone());
        consumer_reports.push(ConsumerRewriteReport {
            file: rewrite.file.clone(),
            replacements: rewrite.replacements,
            hunks: rewrite.hunks.clone(),
            backup,
        });
    }

    let moved_files = plan
        .files
        .iter()
        .cloned()
        .zip(file_dsts)
        .map(|(from, to)| MovedFile { from, to })
        .collect();

    let manifest = std::mem::take(&mut op.manifest);
    let mut all_backups = op.backup_paths();
    all_backups.extend(backups);
    all_backups.sort();
    all_backups.dedup();

    Ok(CombineWriteReport {
        module_name: plan.module_name.clone(),
        facade_path: plan.facade_path.clone(),
        moved_files,
        parent_update,
        consumer_rewrites: consumer_reports,
        skipped_consumer_rewrites: payload.consumer_rewrite_plan.skipped.clone(),
        backups: all_backups,
        manifest,
    })
}

fn same_path(a: &std::path::Path, b: &std::path::Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}
