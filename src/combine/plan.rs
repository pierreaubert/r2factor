use anyhow::{Result, bail};
use std::path::{Path, PathBuf};

/// Resolved plan for combining two peer files into a parent module.
#[derive(Debug, Clone)]
pub struct CombinePlan {
    pub file1: PathBuf,
    pub file2: PathBuf,
    pub module_name: String,
    pub target_dir: PathBuf,
    pub facade_path: PathBuf,
    pub parent_module: Option<PathBuf>,
}

/// Validate inputs and build a combine plan (dry-run or write).
pub fn build_plan(file1: &Path, file2: &Path, module_name: Option<&str>) -> Result<CombinePlan> {
    validate_file(file1)?;
    validate_file(file2)?;

    let parent1 = file1.parent().unwrap_or(Path::new("."));
    let parent2 = file2.parent().unwrap_or(Path::new("."));
    if parent1 != parent2 {
        bail!(
            "files must be in the same directory: {} vs {}",
            file1.display(),
            file2.display()
        );
    }

    let name = module_name
        .map(|s| s.to_string())
        .unwrap_or_else(|| {
            file1
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("combined")
                .to_string()
        });

    let target_dir = parent1.join(&name);
    let facade_path = target_dir.join("mod.rs");

    // Discover parent module (lib.rs or nearest mod.rs)
    let parent_module = discover_parent_module(parent1);

    Ok(CombinePlan {
        file1: file1.to_path_buf(),
        file2: file2.to_path_buf(),
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
        bail!("cannot combine crate entry points (lib.rs / main.rs): {}", path.display());
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
