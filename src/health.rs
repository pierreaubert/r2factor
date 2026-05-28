use anyhow::Result;
use serde::Serialize;
use std::path::{Path, PathBuf};

use crate::tokensave::Tokensave;

#[derive(Debug, Serialize)]
pub struct HealthReport {
    pub root: PathBuf,
    pub cargo_toml: Option<PathBuf>,
    pub tokensave: TokensaveHealth,
    pub local_path_dependencies: Vec<String>,
    pub warnings: Vec<String>,
    pub suggestions: Vec<FixSuggestion>,
}

#[derive(Debug, Serialize)]
pub struct TokensaveHealth {
    pub available: bool,
    pub root: Option<PathBuf>,
    pub node_count: u64,
    pub edge_count: u64,
    pub file_count: u64,
    pub last_updated: u64,
    pub message: String,
}

#[derive(Debug, Serialize)]
pub struct FixSuggestion {
    pub id: String,
    pub title: String,
    pub detail: String,
    pub command: Option<String>,
}

pub fn check(path: &Path) -> Result<HealthReport> {
    let root = project_root(path);
    let cargo_toml = root
        .join("Cargo.toml")
        .is_file()
        .then(|| root.join("Cargo.toml"));
    let tokensave = tokensave_health(path);
    let local_path_dependencies = cargo_toml
        .as_ref()
        .map(|p| local_path_deps(p).unwrap_or_default())
        .unwrap_or_default();

    let mut warnings = Vec::new();
    let mut suggestions = Vec::new();
    if cargo_toml.is_none() {
        warnings.push("No Cargo.toml found above the check path.".to_string());
        suggestions.push(FixSuggestion {
            id: "cargo_root_missing".to_string(),
            title: "Run check from a Cargo project".to_string(),
            detail: "Pass a path inside a Rust crate or workspace so r2factor can find Cargo.toml."
                .to_string(),
            command: None,
        });
    }
    if !tokensave.available {
        warnings.push(tokensave.message.clone());
        suggestions.push(tokensave_fix(&root, "tokensave_unavailable"));
    } else if tokensave.edge_count == 0 {
        warnings.push(
            "TokenSave index has zero edges; clustering and impact previews may be weaker."
                .to_string(),
        );
        suggestions.push(tokensave_fix(&root, "tokensave_zero_edges"));
    }
    if !local_path_dependencies.is_empty() {
        warnings.push(format!(
            "{} local path dependenc{} found; fresh clones may need those paths.",
            local_path_dependencies.len(),
            if local_path_dependencies.len() == 1 {
                "y"
            } else {
                "ies"
            }
        ));
        suggestions.push(FixSuggestion {
            id: "local_path_dependencies".to_string(),
            title: "Verify local path dependencies".to_string(),
            detail: "Make sure each local dependency path exists on this machine, or replace it with a registry/git dependency before sharing the project."
                .to_string(),
            command: None,
        });
    }

    Ok(HealthReport {
        root,
        cargo_toml,
        tokensave,
        local_path_dependencies,
        warnings,
        suggestions,
    })
}

pub fn human_report(report: &HealthReport) -> String {
    let mut out = String::new();
    out.push_str("r2factor check\n\n");
    out.push_str(&format!("root: {}\n", report.root.display()));
    match &report.cargo_toml {
        Some(path) => out.push_str(&format!("cargo: {}\n", path.display())),
        None => out.push_str("cargo: unavailable\n"),
    }
    if report.tokensave.available {
        out.push_str(&format!(
            "tokensave: {} files, {} nodes, {} edges (root {})\n",
            report.tokensave.file_count,
            report.tokensave.node_count,
            report.tokensave.edge_count,
            report.tokensave.root.as_ref().unwrap().display()
        ));
    } else {
        out.push_str(&format!("tokensave: {}\n", report.tokensave.message));
    }
    for dep in &report.local_path_dependencies {
        out.push_str(&format!("local-path-dep: {dep}\n"));
    }
    for warning in &report.warnings {
        out.push_str(&format!("warning: {warning}\n"));
    }
    for suggestion in &report.suggestions {
        out.push_str(&format!(
            "suggestion: {} — {}\n",
            suggestion.id, suggestion.title
        ));
        out.push_str(&format!("  {}\n", suggestion.detail));
        if let Some(command) = &suggestion.command {
            out.push_str(&format!("  command: {command}\n"));
        }
    }
    out
}

fn tokensave_fix(root: &Path, id: &str) -> FixSuggestion {
    FixSuggestion {
        id: id.to_string(),
        title: "Refresh the TokenSave index".to_string(),
        detail: "A fresh TokenSave index improves clustering, impact previews, and consumer rewrite discovery."
            .to_string(),
        command: Some(format!("tokensave index {}", root.display())),
    }
}

fn tokensave_health(path: &Path) -> TokensaveHealth {
    let Some(root) = Tokensave::locate(path) else {
        return TokensaveHealth {
            available: false,
            root: None,
            node_count: 0,
            edge_count: 0,
            file_count: 0,
            last_updated: 0,
            message: "No .tokensave/tokensave.db found above the check path.".to_string(),
        };
    };
    let ts = match Tokensave::open_safe(&root) {
        Ok(ts) => ts,
        Err(e) => {
            return TokensaveHealth {
                available: false,
                root: Some(root),
                node_count: 0,
                edge_count: 0,
                file_count: 0,
                last_updated: 0,
                message: format!("Could not open TokenSave DB: {e}"),
            };
        }
    };
    match ts.rt.block_on(ts.db.get_stats()) {
        Ok(stats) => TokensaveHealth {
            available: true,
            root: Some(root),
            node_count: stats.node_count,
            edge_count: stats.edge_count,
            file_count: stats.file_count,
            last_updated: stats.last_updated,
            message: "ok".to_string(),
        },
        Err(e) => TokensaveHealth {
            available: false,
            root: Some(root),
            node_count: 0,
            edge_count: 0,
            file_count: 0,
            last_updated: 0,
            message: format!("Could not read TokenSave stats: {e}"),
        },
    }
}

fn local_path_deps(cargo_toml: &Path) -> Result<Vec<String>> {
    let src = std::fs::read_to_string(cargo_toml)?;
    Ok(src
        .lines()
        .filter(|line| line.contains("path = "))
        .map(|line| line.trim().to_string())
        .collect())
}

fn project_root(path: &Path) -> PathBuf {
    let mut cur = path
        .canonicalize()
        .ok()
        .and_then(|p| {
            if p.is_file() {
                p.parent().map(Path::to_path_buf)
            } else {
                Some(p)
            }
        })
        .or_else(|| path.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."));
    loop {
        if cur.join("Cargo.toml").is_file() {
            return cur;
        }
        if !cur.pop() {
            return path.to_path_buf();
        }
    }
}
