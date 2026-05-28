use anyhow::{Context, Result};
use serde::Serialize;
use std::fs;
use std::path::{Path, PathBuf};

use crate::tokensave::Tokensave;

#[derive(Debug, Clone)]
pub struct SuggestOptions {
    pub min_score: usize,
}

impl Default for SuggestOptions {
    fn default() -> Self {
        Self { min_score: 1 }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct CombineSuggestReport {
    pub directory: PathBuf,
    pub tokensave: TokensaveSuggestionStatus,
    pub suggestions: Vec<CombineGroupSuggestion>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TokensaveSuggestionStatus {
    pub available: bool,
    pub project_root: Option<PathBuf>,
    pub message: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct CombineGroupSuggestion {
    pub files: Vec<PathBuf>,
    pub module_name: String,
    pub score: usize,
    pub reasons: Vec<String>,
}

struct PeerModule {
    path: PathBuf,
    stem: String,
    source: String,
}

pub fn suggest_groups(path: &Path, opts: &SuggestOptions) -> Result<CombineSuggestReport> {
    let directory = if path.is_file() {
        path.parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf()
    } else {
        path.to_path_buf()
    };
    let directory = directory
        .canonicalize()
        .with_context(|| format!("canonicalize {}", directory.display()))?;
    let modules = peer_modules(&directory)?;
    let tokensave = tokensave_status(&directory);
    let mut suggestions = Vec::new();

    for i in 0..modules.len() {
        for j in (i + 1)..modules.len() {
            let left = &modules[i];
            let right = &modules[j];
            let mut score = 0;
            let mut reasons = Vec::new();

            let left_to_right = module_ref_count(&left.source, &right.stem);
            if left_to_right > 0 {
                score += left_to_right;
                reasons.push(format!(
                    "{} references `{}` {} time(s)",
                    left.path.display(),
                    right.stem,
                    left_to_right
                ));
            }
            let right_to_left = module_ref_count(&right.source, &left.stem);
            if right_to_left > 0 {
                score += right_to_left;
                reasons.push(format!(
                    "{} references `{}` {} time(s)",
                    right.path.display(),
                    left.stem,
                    right_to_left
                ));
            }

            if common_prefix_score(&left.stem, &right.stem) {
                score += 1;
                reasons.push(format!(
                    "`{}` and `{}` share a stem prefix",
                    left.stem, right.stem
                ));
            }

            if score >= opts.min_score {
                suggestions.push(CombineGroupSuggestion {
                    files: vec![left.path.clone(), right.path.clone()],
                    module_name: suggested_module_name(&left.stem, &right.stem),
                    score,
                    reasons,
                });
            }
        }
    }

    suggestions.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then_with(|| a.module_name.cmp(&b.module_name))
            .then_with(|| a.files.cmp(&b.files))
    });

    Ok(CombineSuggestReport {
        directory,
        tokensave,
        suggestions,
    })
}

fn peer_modules(directory: &Path) -> Result<Vec<PeerModule>> {
    let mut modules = Vec::new();
    for entry in
        fs::read_dir(directory).with_context(|| format!("read_dir {}", directory.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
        if !path.is_file()
            || path.extension().is_none_or(|ext| ext != "rs")
            || name.ends_with(".bak")
            || matches!(name, "lib.rs" | "main.rs" | "mod.rs")
        {
            continue;
        }
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or_default()
            .to_string();
        let source =
            fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
        modules.push(PeerModule { path, stem, source });
    }
    modules.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(modules)
}

fn tokensave_status(path: &Path) -> TokensaveSuggestionStatus {
    let Some(project_root) = Tokensave::locate(path) else {
        return TokensaveSuggestionStatus {
            available: false,
            project_root: None,
            message: "No .tokensave/tokensave.db index found; using local source references."
                .to_string(),
        };
    };

    match Tokensave::open_safe(&project_root) {
        Ok(_) => TokensaveSuggestionStatus {
            available: true,
            project_root: Some(project_root),
            message: "TokenSave index is readable; local source-reference scoring is available."
                .to_string(),
        },
        Err(e) => TokensaveSuggestionStatus {
            available: false,
            project_root: Some(project_root),
            message: format!(
                "TokenSave index could not be opened ({e}); using local source references."
            ),
        },
    }
}

fn module_ref_count(source: &str, module: &str) -> usize {
    let patterns = [
        format!("crate::{module}::"),
        format!("crate :: {module} ::"),
        format!("super::{module}::"),
        format!("super :: {module} ::"),
        format!("self::{module}::"),
        format!("self :: {module} ::"),
        format!("{module}::"),
        format!("{module} ::"),
    ];
    patterns
        .iter()
        .map(|pattern| source.matches(pattern).count())
        .sum()
}

fn common_prefix_score(left: &str, right: &str) -> bool {
    let left = left.split('_').next().unwrap_or(left);
    let right = right.split('_').next().unwrap_or(right);
    left.len() >= 3 && left == right
}

fn suggested_module_name(left: &str, right: &str) -> String {
    let left_prefix = left.split('_').next().unwrap_or(left);
    let right_prefix = right.split('_').next().unwrap_or(right);
    if left_prefix.len() >= 3 && left_prefix == right_prefix {
        left_prefix.to_string()
    } else {
        left.to_string()
    }
}
