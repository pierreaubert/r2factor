//! Thin wrapper over the local tokensave SQLite graph. The DB lives at
//! `<project_root>/.tokensave/tokensave.db`; we open it on demand and use
//! it to fold cross-symbol edges into our intra-file ref graph.

mod evidence;
mod matching;

use crate::item::ParsedItem;
use anyhow::{Context, Result};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use tokensave::db::Database;
use tokio::runtime::{Builder, Runtime};

pub use evidence::CrossFileEvidence;

pub struct Tokensave {
    pub(crate) db: Database,
    pub(crate) project_root: PathBuf,
    pub(crate) rt: Runtime,
}

impl Tokensave {
    /// Walk up from `start` looking for `.tokensave/tokensave.db`. Returns
    /// the project root (parent of `.tokensave`).
    pub fn locate(start: &Path) -> Option<PathBuf> {
        let canonical = start.canonicalize().ok();
        let mut cur = canonical
            .as_deref()
            .and_then(|p| {
                if p.is_dir() {
                    Some(p.to_path_buf())
                } else {
                    p.parent().map(Path::to_path_buf)
                }
            })
            .or_else(|| {
                if start.is_dir() {
                    Some(start.to_path_buf())
                } else {
                    start.parent().map(Path::to_path_buf)
                }
            })?;
        loop {
            if cur.join(".tokensave").join("tokensave.db").exists() {
                return Some(cur);
            }
            if !cur.pop() {
                return None;
            }
        }
    }

    pub fn open(project_root: &Path) -> Result<Self> {
        let rt = Builder::new_current_thread()
            .enable_all()
            .build()
            .context("build tokio runtime for tokensave")?;
        let db_path = project_root.join(".tokensave").join("tokensave.db");
        let (db, _migrated) = rt
            .block_on(Database::open(&db_path))
            .with_context(|| format!("open tokensave db {}", db_path.display()))?;
        Ok(Self {
            db,
            project_root: project_root.to_path_buf(),
            rt,
        })
    }

    pub fn open_safe(project_root: &Path) -> Result<Self> {
        let old_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        match catch_unwind(AssertUnwindSafe(|| Self::open(project_root))) {
            Ok(result) => {
                std::panic::set_hook(old_hook);
                result
            }
            Err(_) => {
                std::panic::set_hook(old_hook);
                anyhow::bail!(
                    "tokensave open panicked; the database schema may be newer than the linked tokensave crate"
                )
            }
        }
    }

    pub fn evidence_for_file(
        &self,
        file_path: &Path,
        items: &[ParsedItem],
    ) -> Result<CrossFileEvidence> {
        evidence::evidence_for_file(self, file_path, items)
    }
}
