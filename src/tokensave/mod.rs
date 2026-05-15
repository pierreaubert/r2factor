//! Thin wrapper over the local tokensave SQLite graph. The DB lives at
//! `<project_root>/.tokensave/tokensave.db`; we open it on demand and use
//! it to fold cross-symbol edges into our intra-file ref graph.

mod evidence;
mod matching;

use crate::item::ParsedItem;
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use tokensave::db::Database;
use tokio::runtime::{Builder, Runtime};

pub use evidence::CrossFileEvidence;

pub struct Tokensave {
    pub(super) db: Database,
    pub(super) project_root: PathBuf,
    pub(super) rt: Runtime,
}

impl Tokensave {
    /// Walk up from `start` looking for `.tokensave/tokensave.db`. Returns
    /// the project root (parent of `.tokensave`).
    pub fn locate(start: &Path) -> Option<PathBuf> {
        let mut cur = start
            .canonicalize()
            .ok()
            .and_then(|p| p.parent().map(Path::to_path_buf))
            .or_else(|| start.parent().map(Path::to_path_buf))?;
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

    pub fn evidence_for_file(
        &self,
        file_path: &Path,
        items: &[ParsedItem],
    ) -> Result<CrossFileEvidence> {
        evidence::evidence_for_file(self, file_path, items)
    }
}
