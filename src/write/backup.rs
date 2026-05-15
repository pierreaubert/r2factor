use anyhow::{Result, anyhow};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Build a sibling path of the original file with a `.r2factor.bak.<ts>`
/// suffix. Using the wall-clock seconds (epoch 0 on failure) keeps backups
/// unique across runs without coordinating with the filesystem.
pub fn make_backup_path(original: &Path) -> Result<PathBuf> {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let mut path = original.to_path_buf();
    let name = original
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| anyhow!("invalid filename {}", original.display()))?;
    path.set_file_name(format!("{name}.r2factor.bak.{ts}"));
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backup_path_appends_timestamp() {
        let p = make_backup_path(Path::new("/tmp/foo.rs")).unwrap();
        let s = p.to_string_lossy();
        assert!(s.starts_with("/tmp/foo.rs.r2factor.bak."));
    }
}
