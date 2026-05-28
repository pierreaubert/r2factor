use anyhow::{Result, bail};
use std::path::{Path, PathBuf};

/// Resolved plan for combining two peer files into a parent module.
#[derive(Debug, Clone)]
pub struct CombinePlan {
    pub files: Vec<PathBuf>,
    pub module_name: String,
    pub target_dir: PathBuf,
    pub facade_path: PathBuf,
    pub parent_module: Option<PathBuf>,
}

pub fn build_plan_many(files: &[PathBuf], module_name: Option<&str>) -> Result<CombinePlan> {
    if files.len() < 2 {
        bail!("combine requires at least two .rs files");
    }
    for file in files {
        validate_file(file)?;
    }

    let parent = files[0].parent().unwrap_or(Path::new("."));
    for file in &files[1..] {
        let file_parent = file.parent().unwrap_or(Path::new("."));
        if parent != file_parent {
            bail!(
                "files must be in the same directory: {} vs {}",
                files[0].display(),
                file.display()
            );
        }
    }

    let name = module_name.map(|s| s.to_string()).unwrap_or_else(|| {
        files[0]
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("combined")
            .to_string()
    });

    let target_dir = parent.join(&name);
    let facade_path = target_dir.join("mod.rs");

    // Discover parent module (lib.rs or nearest mod.rs)
    let parent_module = discover_parent_module(parent);

    Ok(CombinePlan {
        files: files.to_vec(),
        module_name: name,
        target_dir,
        facade_path,
        parent_module,
    })
}

fn validate_file(path: &Path) -> Result<()> {
    let name = path
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| anyhow::anyhow!("invalid file name: {}", path.display()))?;

    if !name.ends_with(".rs") {
        bail!("not a .rs file: {}", path.display());
    }
    if name == "lib.rs" || name == "main.rs" {
        bail!(
            "cannot combine crate entry points (lib.rs / main.rs): {}",
            path.display()
        );
    }
    if !path.exists() {
        bail!("file does not exist: {}", path.display());
    }
    Ok(())
}

fn discover_parent_module(dir: &Path) -> Option<PathBuf> {
    // Look for lib.rs in the same directory
    let lib_rs = dir.join("lib.rs");
    if lib_rs.is_file() {
        return Some(lib_rs);
    }
    // Look for mod.rs in the same directory
    let mod_rs = dir.join("mod.rs");
    if mod_rs.is_file() {
        return Some(mod_rs);
    }
    // Look for a sibling .rs file that might be the parent facade
    // Heuristic: if the dir is inside a module (e.g., src/foo/bar.rs),
    // the parent might be src/foo.rs or src/foo/mod.rs
    if let Some(parent_dir) = dir.parent() {
        let dir_name = dir.file_name()?.to_str()?;
        let parent_facade = parent_dir.join(format!("{dir_name}.rs"));
        if parent_facade.is_file() {
            return Some(parent_facade);
        }
        let parent_mod = parent_dir.join(format!("{dir_name}/mod.rs"));
        if parent_mod.is_file() {
            return Some(parent_mod);
        }
    }
    None
}
