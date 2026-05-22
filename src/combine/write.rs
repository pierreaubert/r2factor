use anyhow::{Context, Result, bail};
use std::fs;
use std::path::PathBuf;

use super::parent::make_backup_path;

#[derive(Debug, serde::Serialize)]
pub struct CombineWriteReport {
    pub module_name: String,
    pub facade_path: PathBuf,
    pub moved_files: Vec<MovedFile>,
    pub parent_update: Option<ParentUpdate>,
    pub backups: Vec<PathBuf>,
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

pub struct WriteOptions {
    pub force: bool,
}

/// Execute the combine write: create directory, write facade, move files,
/// update parent module, create backups.
pub fn execute_write(
    plan: &super::plan::CombinePlan,
    facade_src: &str,
    file1_src: &str,
    file2_src: &str,
    parent_src: Option<&str>,
    opts: &WriteOptions,
) -> Result<CombineWriteReport> {
    // Check target dir
    if plan.target_dir.exists() && !opts.force {
        bail!(
            "target directory already exists: {}. Use --force to overwrite.",
            plan.target_dir.display()
        );
    }

    // Create target dir
    if plan.target_dir.exists() {
        // With --force, remove existing .rs files in target dir
        for entry in fs::read_dir(&plan.target_dir)
            .with_context(|| format!("read_dir {}", plan.target_dir.display()))?
        {
            let entry = entry?;
            let path = entry.path();
            if path.is_file() && path.extension().map_or(false, |e| e == "rs") {
                fs::remove_file(&path)
                    .with_context(|| format!("remove {}", path.display()))?;
            }
        }
    } else {
        fs::create_dir_all(&plan.target_dir)
            .with_context(|| format!("mkdir {}", plan.target_dir.display()))?;
    }

    let mut backups = Vec::new();

    // Backup and write parent module
    let parent_update = if let Some(parent_path) = &plan.parent_module {
        if let Some(parent_src) = parent_src {
            let backup = make_backup_path(parent_path)?;
            if parent_path.exists() {
                fs::copy(parent_path, &backup)
                    .with_context(|| format!("backup {}", parent_path.display()))?;
                backups.push(backup);
            }
            fs::write(parent_path, parent_src)
                .with_context(|| format!("write {}", parent_path.display()))?;

            let stem1 = plan.file1.file_stem().unwrap().to_str().unwrap();
            let stem2 = plan.file2.file_stem().unwrap().to_str().unwrap();
            Some(ParentUpdate {
                path: parent_path.clone(),
                add: format!("mod {};", plan.module_name),
                remove: vec![format!("mod {};", stem1), format!("mod {};", stem2)],
            })
        } else {
            None
        }
    } else {
        None
    };

    // Write facade
    fs::write(&plan.facade_path, facade_src)
        .with_context(|| format!("write facade {}", plan.facade_path.display()))?;

    // Backup and move files
    let mut moved_files = Vec::new();

    let file1_name = plan.file1.file_name().unwrap();
    let file1_dst = plan.target_dir.join(file1_name);
    let backup1 = make_backup_path(&plan.file1)?;
    fs::copy(&plan.file1, &backup1)
        .with_context(|| format!("backup {}", plan.file1.display()))?;
    backups.push(backup1);
    fs::write(&file1_dst, file1_src)
        .with_context(|| format!("write {}", file1_dst.display()))?;
    fs::remove_file(&plan.file1)
        .with_context(|| format!("remove {}", plan.file1.display()))?;
    moved_files.push(MovedFile {
        from: plan.file1.clone(),
        to: file1_dst,
    });

    let file2_name = plan.file2.file_name().unwrap();
    let file2_dst = plan.target_dir.join(file2_name);
    let backup2 = make_backup_path(&plan.file2)?;
    fs::copy(&plan.file2, &backup2)
        .with_context(|| format!("backup {}", plan.file2.display()))?;
    backups.push(backup2);
    fs::write(&file2_dst, file2_src)
        .with_context(|| format!("write {}", file2_dst.display()))?;
    fs::remove_file(&plan.file2)
        .with_context(|| format!("remove {}", plan.file2.display()))?;
    moved_files.push(MovedFile {
        from: plan.file2.clone(),
        to: file2_dst,
    });

    Ok(CombineWriteReport {
        module_name: plan.module_name.clone(),
        facade_path: plan.facade_path.clone(),
        moved_files,
        parent_update,
        backups,
    })
}
