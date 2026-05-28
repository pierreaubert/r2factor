use anyhow::{Context, Result, bail};
use serde::Serialize;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

#[derive(Debug, Clone, Serialize)]
pub struct BackupEntry {
    pub backup: PathBuf,
    pub restore_target: PathBuf,
    pub size_bytes: u64,
    pub modified_unix_secs: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RestoreReport {
    pub backup: PathBuf,
    pub restored: PathBuf,
    pub replaced_existing: bool,
    pub bytes_copied: u64,
}

pub fn list_backups(path: &Path) -> Result<Vec<BackupEntry>> {
    let mut out = Vec::new();
    if path.is_file() {
        if is_backup(path) {
            out.push(backup_entry(path)?);
        }
    } else {
        walk(path, &mut out)?;
    }
    out.sort_by(|a, b| a.backup.cmp(&b.backup));
    Ok(out)
}

pub fn restore_backup(backup: &Path, force: bool) -> Result<RestoreReport> {
    if !is_backup(backup) {
        bail!("not a .bak file: {}", backup.display());
    }
    if !backup.is_file() {
        bail!("backup file does not exist: {}", backup.display());
    }

    let target = restore_target(backup)?;
    let replaced_existing = target.exists();
    if replaced_existing && !force {
        bail!(
            "restore target already exists: {}. Use --force to overwrite.",
            target.display()
        );
    }

    let bytes_copied = fs::copy(backup, &target)
        .with_context(|| format!("restore {} -> {}", backup.display(), target.display()))?;
    Ok(RestoreReport {
        backup: backup.to_path_buf(),
        restored: target,
        replaced_existing,
        bytes_copied,
    })
}

pub fn human_list(entries: &[BackupEntry]) -> String {
    if entries.is_empty() {
        return "No .bak files found.\n".to_string();
    }

    let mut out = String::new();
    for entry in entries {
        out.push_str(&format!(
            "{} -> {} ({} bytes)\n",
            entry.backup.display(),
            entry.restore_target.display(),
            entry.size_bytes
        ));
    }
    out
}

pub fn human_restore(report: &RestoreReport) -> String {
    let mode = if report.replaced_existing {
        "replaced"
    } else {
        "restored"
    };
    format!(
        "[restore] {} {} from {} ({} bytes)\n",
        mode,
        report.restored.display(),
        report.backup.display(),
        report.bytes_copied
    )
}

fn walk(dir: &Path, out: &mut Vec<BackupEntry>) -> Result<()> {
    for entry in fs::read_dir(dir).with_context(|| format!("read_dir {}", dir.display()))? {
        let entry = entry?;
        let path = entry.path();
        let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
        if path.is_dir() {
            if matches!(name, ".git" | ".tokensave" | "target") {
                continue;
            }
            walk(&path, out)?;
        } else if is_backup(&path) {
            out.push(backup_entry(&path)?);
        }
    }
    Ok(())
}

fn backup_entry(path: &Path) -> Result<BackupEntry> {
    let metadata = fs::metadata(path).with_context(|| format!("stat {}", path.display()))?;
    let modified_unix_secs = metadata
        .modified()
        .ok()
        .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_secs());
    Ok(BackupEntry {
        backup: path.to_path_buf(),
        restore_target: restore_target(path)?,
        size_bytes: metadata.len(),
        modified_unix_secs,
    })
}

fn restore_target(backup: &Path) -> Result<PathBuf> {
    let name = backup
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| anyhow::anyhow!("invalid backup filename {}", backup.display()))?;
    let Some(original_name) = name.strip_suffix(".bak") else {
        bail!("not a .bak file: {}", backup.display());
    };
    Ok(backup.with_file_name(original_name))
}

fn is_backup(path: &Path) -> bool {
    path.file_name()
        .and_then(|s| s.to_str())
        .is_some_and(|name| name.ends_with(".bak"))
}
