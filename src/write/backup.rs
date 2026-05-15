use anyhow::{Result, anyhow};
use std::path::{Path, PathBuf};

/// Build a sibling path of the original file with a `.bak` suffix —
/// `foo.rs` → `foo.rs.bak`. Idempotent across runs: the facade-marker
/// guard refuses to re-split a generated facade, so the `.bak` always
/// holds the user's original source. If the user really wants a fresh
/// backup they can delete `.bak` first; otherwise re-running on the
/// original (after restoring from `.bak`) just overwrites with the same
/// bytes.
pub fn make_backup_path(original: &Path) -> Result<PathBuf> {
    let mut path = original.to_path_buf();
    let name = original
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| anyhow!("invalid filename {}", original.display()))?;
    path.set_file_name(format!("{name}.bak"));
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backup_path_appends_dot_bak() {
        let p = make_backup_path(Path::new("/tmp/foo.rs")).unwrap();
        assert_eq!(p.to_string_lossy(), "/tmp/foo.rs.bak");
    }

    #[test]
    fn backup_path_handles_no_extension() {
        let p = make_backup_path(Path::new("/tmp/foo")).unwrap();
        assert_eq!(p.to_string_lossy(), "/tmp/foo.bak");
    }
}
