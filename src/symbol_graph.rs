//! Symbol evidence graph for whole-codebase refactoring.
//!
//! Tokensave provides the strongest cross-file signal. AST and rg findings are
//! retained as candidates so reports can be honest about confidence.

use crate::discover::{Discovery, GlobalItemId};
use crate::graph;
use crate::tokensave::Tokensave;
use crate::workspace_model::{FileId, WorkspaceModel};
use anyhow::{Context, Result};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::PathBuf;
use syn::spanned::Spanned;
use syn::visit::Visit;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceSource {
    Tokensave,
    AstScoped,
    RgCandidate,
    HeuristicName,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceConfidence {
    Exact,
    Semantic,
    ScopedCandidate,
    TextCandidate,
    Ambiguous,
    Unresolved,
}

#[derive(Debug, Clone, Serialize)]
pub struct SymbolRef {
    pub item_id: Option<GlobalItemId>,
    pub name: String,
    pub module_path: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SymbolEdge {
    pub from: SymbolRef,
    pub to: Option<SymbolRef>,
    pub source: EvidenceSource,
    pub confidence: EvidenceConfidence,
    pub file: FileId,
    pub line: usize,
    pub original_text: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SymbolGraph {
    pub tokensave: TokenSaveStatus,
    pub edges: Vec<SymbolEdge>,
    pub diagnostics: Vec<SymbolDiagnostic>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum TokenSaveStatus {
    Available {
        root: PathBuf,
        semantic_edges: usize,
    },
    Unavailable {
        message: String,
    },
}

#[derive(Debug, Clone, Serialize)]
pub struct SymbolDiagnostic {
    pub kind: String,
    pub message: String,
    pub path: Option<PathBuf>,
}

pub fn build_symbol_graph(
    workspace: &WorkspaceModel,
    discovery: &Discovery,
    use_tokensave: bool,
    use_rg_candidates: bool,
) -> SymbolGraph {
    let mut diagnostics = Vec::new();
    let mut edges = ast_and_heuristic_edges(workspace, discovery);
    let tokensave = if use_tokensave {
        match tokensave_edges(workspace, discovery) {
            Ok((status, mut ts_edges)) => {
                let semantic_edges = ts_edges.len();
                edges.append(&mut ts_edges);
                match status {
                    TokenSaveStatus::Available { root, .. } => TokenSaveStatus::Available {
                        root,
                        semantic_edges,
                    },
                    other => other,
                }
            }
            Err(e) => {
                diagnostics.push(SymbolDiagnostic {
                    kind: "tokensave_error".to_string(),
                    message: e.to_string(),
                    path: Some(discovery.root.clone()),
                });
                TokenSaveStatus::Unavailable {
                    message: e.to_string(),
                }
            }
        }
    } else {
        TokenSaveStatus::Unavailable {
            message: "tokensave disabled".to_string(),
        }
    };

    if use_rg_candidates {
        match rg_candidate_edges(workspace, discovery) {
            Ok(mut rg_edges) => edges.append(&mut rg_edges),
            Err(e) => diagnostics.push(SymbolDiagnostic {
                kind: "rg_unavailable".to_string(),
                message: e.to_string(),
                path: Some(discovery.root.clone()),
            }),
        }
    }

    edges.sort_by(|a, b| {
        (
            a.file,
            a.line,
            &a.from.name,
            a.to.as_ref().map(|t| t.name.as_str()).unwrap_or(""),
            a.source,
            a.confidence,
        )
            .cmp(&(
                b.file,
                b.line,
                &b.from.name,
                b.to.as_ref().map(|t| t.name.as_str()).unwrap_or(""),
                b.source,
                b.confidence,
            ))
    });

    SymbolGraph {
        tokensave,
        edges,
        diagnostics,
    }
}

impl SymbolGraph {
    pub fn has_write_grade_cross_file_evidence(&self) -> bool {
        self.edges.iter().any(|edge| {
            matches!(
                edge.confidence,
                EvidenceConfidence::Exact | EvidenceConfidence::Semantic
            )
        })
    }
}

fn ast_and_heuristic_edges(workspace: &WorkspaceModel, discovery: &Discovery) -> Vec<SymbolEdge> {
    let mut out = Vec::new();
    let mut name_index: BTreeMap<&str, Vec<GlobalItemId>> = BTreeMap::new();
    for item in &discovery.items {
        if !item.name.is_empty() {
            name_index.entry(&item.name).or_default().push(item.id);
        }
    }

    for edge in &discovery.edges {
        let from = &discovery.items[edge.from];
        let to = &discovery.items[edge.to];
        let confidence = if from.file_id == to.file_id {
            EvidenceConfidence::ScopedCandidate
        } else {
            EvidenceConfidence::TextCandidate
        };
        let source = if from.file_id == to.file_id {
            EvidenceSource::AstScoped
        } else {
            EvidenceSource::HeuristicName
        };
        out.push(SymbolEdge {
            from: symbol_ref(workspace, discovery, from.id),
            to: Some(symbol_ref(workspace, discovery, to.id)),
            source,
            confidence,
            file: from.file_id,
            line: from.line_start,
            original_text: Some(to.name.clone()),
        });
    }

    let global_names: HashSet<String> = discovery
        .items
        .iter()
        .filter(|i| !i.name.is_empty())
        .map(|i| i.name.clone())
        .collect();
    for file in &discovery.files {
        let Some(file_id) = discovery.files.iter().position(|f| f.path == file.path) else {
            continue;
        };
        for item in &file.items {
            let refs = graph::collect_refs(&item.source, &global_names, &item.name);
            for name in refs {
                let Some(candidates) = name_index.get(name.as_str()) else {
                    continue;
                };
                if candidates.len() > 1 {
                    out.push(SymbolEdge {
                        from: SymbolRef {
                            item_id: Some(item.id),
                            name: item.name.clone(),
                            module_path: workspace.module_for_file(file_id).map(|m| m.path.clone()),
                        },
                        to: None,
                        source: EvidenceSource::HeuristicName,
                        confidence: EvidenceConfidence::Ambiguous,
                        file: file_id,
                        line: item.line_start,
                        original_text: Some(name),
                    });
                }
            }
        }
    }
    out
}

fn tokensave_edges(
    workspace: &WorkspaceModel,
    discovery: &Discovery,
) -> Result<(TokenSaveStatus, Vec<SymbolEdge>)> {
    let root = match Tokensave::locate(&discovery.root) {
        Some(root) => root,
        None => {
            return Ok((
                TokenSaveStatus::Unavailable {
                    message: "no .tokensave/tokensave.db found".to_string(),
                },
                Vec::new(),
            ));
        }
    };
    let ts = Tokensave::open_safe(&root)?;
    let mut node_to_global: HashMap<String, GlobalItemId> = HashMap::new();

    for (file_id, file) in discovery.files.iter().enumerate() {
        let abs = file
            .path
            .canonicalize()
            .unwrap_or_else(|_| file.path.clone());
        let rel = abs
            .strip_prefix(&root)
            .map(|p| p.to_path_buf())
            .unwrap_or(abs);
        let rel_str = rel.to_string_lossy().to_string();
        let nodes: Vec<tokensave::types::Node> = ts
            .rt
            .block_on(ts.db.get_nodes_by_file(&rel_str))
            .context("get_nodes_by_file")?;
        for item in &file.items {
            if let Some(node_id) = crate::tokensave::matching::best_node_for_item(item, &nodes)
                && let Some(global) = discovery
                    .items
                    .iter()
                    .find(|g| g.file_id == file_id && g.local_id == item.id)
            {
                node_to_global.insert(node_id, global.id);
            }
        }
    }

    if node_to_global.is_empty() {
        return Ok((
            TokenSaveStatus::Available {
                root,
                semantic_edges: 0,
            },
            Vec::new(),
        ));
    }

    let node_ids: Vec<String> = node_to_global.keys().cloned().collect();
    let db_edges: Vec<tokensave::types::Edge> = ts
        .rt
        .block_on(ts.db.get_internal_edges(&node_ids))
        .context("get_internal_edges")?;
    let mut out = Vec::new();
    for edge in db_edges {
        let Some(&from_id) = node_to_global.get(&edge.source) else {
            continue;
        };
        let Some(&to_id) = node_to_global.get(&edge.target) else {
            continue;
        };
        if from_id == to_id {
            continue;
        }
        let from = &discovery.items[from_id];
        let confidence = if edge.line.is_some() {
            EvidenceConfidence::Exact
        } else {
            EvidenceConfidence::Semantic
        };
        out.push(SymbolEdge {
            from: symbol_ref(workspace, discovery, from_id),
            to: Some(symbol_ref(workspace, discovery, to_id)),
            source: EvidenceSource::Tokensave,
            confidence,
            file: from.file_id,
            line: edge
                .line
                .map(|line| line as usize)
                .unwrap_or(from.line_start),
            original_text: Some(format!("{:?}: {}", edge.kind, discovery.items[to_id].name)),
        });
    }

    let public_names: Vec<String> = discovery
        .items
        .iter()
        .filter(|item| !item.name.is_empty())
        .map(|item| item.name.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    if let Ok(mut unresolved) = query_unresolved_ref_edges(&ts, workspace, discovery, &public_names)
    {
        out.append(&mut unresolved);
    }
    Ok((
        TokenSaveStatus::Available {
            root,
            semantic_edges: out.len(),
        },
        out,
    ))
}

struct UnresolvedRefRow {
    reference_name: String,
    file_path: String,
    line: usize,
}

fn query_unresolved_ref_edges(
    ts: &Tokensave,
    workspace: &WorkspaceModel,
    discovery: &Discovery,
    symbols: &[String],
) -> Result<Vec<SymbolEdge>> {
    if symbols.is_empty() {
        return Ok(Vec::new());
    }
    let limited_symbols: Vec<String> = symbols.iter().take(200).cloned().collect();
    let placeholders = (0..limited_symbols.len())
        .map(|_| "?")
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "SELECT reference_name, file_path, line \
         FROM unresolved_refs \
         WHERE reference_name IN ({placeholders}) \
         ORDER BY file_path, line \
         LIMIT 2000"
    );
    let params = limited_symbols
        .iter()
        .cloned()
        .map(libsql::Value::Text)
        .collect::<Vec<_>>();
    let rows = ts.rt.block_on(async {
        let mut rows = ts
            .db
            .conn()
            .query(&sql, libsql::params_from_iter(params))
            .await
            .context("query tokensave unresolved refs")?;
        let mut out = Vec::new();
        while let Some(row) = rows.next().await.context("read tokensave unresolved ref")? {
            out.push(UnresolvedRefRow {
                reference_name: row.get::<String>(0).unwrap_or_default(),
                file_path: row.get::<String>(1).unwrap_or_default(),
                line: row.get::<i64>(2).unwrap_or_default() as usize,
            });
        }
        anyhow::Ok(out)
    })?;

    let name_index = discovery.name_index();
    let mut out = Vec::new();
    for row in rows {
        let abs_path = ts.project_root.join(&row.file_path);
        let file_id = workspace
            .file_for_path(&abs_path)
            .map(|file| file.id)
            .unwrap_or(usize::MAX);
        let candidates = name_index
            .get(row.reference_name.as_str())
            .map(|ids| ids.iter().copied().collect::<Vec<_>>())
            .unwrap_or_default();
        let to = match candidates.as_slice() {
            [id] => Some(symbol_ref(workspace, discovery, *id)),
            _ => None,
        };
        out.push(SymbolEdge {
            from: SymbolRef {
                item_id: None,
                name: row.reference_name.clone(),
                module_path: None,
            },
            to,
            source: EvidenceSource::Tokensave,
            confidence: EvidenceConfidence::Unresolved,
            file: file_id,
            line: row.line,
            original_text: read_line(&abs_path, row.line),
        });
    }
    Ok(out)
}

fn read_line(path: &PathBuf, line: usize) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()?
        .lines()
        .nth(line.saturating_sub(1))
        .map(str::to_string)
}

fn rg_candidate_edges(
    workspace: &WorkspaceModel,
    discovery: &Discovery,
) -> Result<Vec<SymbolEdge>> {
    let status = std::process::Command::new("rg")
        .arg("--version")
        .output()
        .context("invoke rg --version")?;
    if !status.status.success() {
        anyhow::bail!("rg is not available");
    }

    let public_names: BTreeSet<String> = discovery
        .items
        .iter()
        .filter(|item| {
            !item.name.is_empty()
                && matches!(
                    item.vis,
                    crate::item::ItemVis::Public | crate::item::ItemVis::Crate
                )
        })
        .map(|item| item.name.clone())
        .collect();
    let mut out = Vec::new();
    for name in public_names.into_iter().take(50) {
        let output = std::process::Command::new("rg")
            .arg("--line-number")
            .arg("--fixed-strings")
            .arg("--glob")
            .arg("*.rs")
            .arg("--glob")
            .arg("!target/**")
            .arg("--glob")
            .arg("!.git/**")
            .arg("--glob")
            .arg("!.tokensave/**")
            .arg(&name)
            .arg(&discovery.root)
            .output()
            .with_context(|| format!("run rg for {name}"))?;
        if !output.status.success() {
            continue;
        }
        for line in String::from_utf8_lossy(&output.stdout).lines().take(200) {
            if let Some((path, rest)) = line.split_once(':')
                && let Some((line_no, text)) = rest.split_once(':')
            {
                let path = PathBuf::from(path);
                let file_id = workspace
                    .file_for_path(&path)
                    .map(|f| f.id)
                    .unwrap_or(usize::MAX);
                out.push(SymbolEdge {
                    from: SymbolRef {
                        item_id: None,
                        name: name.clone(),
                        module_path: None,
                    },
                    to: None,
                    source: EvidenceSource::RgCandidate,
                    confidence: rg_hit_confidence(&path, line_no.parse().unwrap_or(0), &name),
                    file: file_id,
                    line: line_no.parse().unwrap_or(0),
                    original_text: Some(text.to_string()),
                });
            }
        }
    }
    Ok(out)
}

fn rg_hit_confidence(path: &PathBuf, line: usize, name: &str) -> EvidenceConfidence {
    let Ok(src) = std::fs::read_to_string(path) else {
        return EvidenceConfidence::TextCandidate;
    };
    let Ok(ast) = syn::parse_file(&src) else {
        return EvidenceConfidence::TextCandidate;
    };
    let mut visitor = PathLineVisitor {
        name,
        line,
        found: false,
    };
    visitor.visit_file(&ast);
    if visitor.found {
        EvidenceConfidence::ScopedCandidate
    } else {
        EvidenceConfidence::TextCandidate
    }
}

struct PathLineVisitor<'a> {
    name: &'a str,
    line: usize,
    found: bool,
}

impl<'ast> Visit<'ast> for PathLineVisitor<'_> {
    fn visit_path(&mut self, path: &'ast syn::Path) {
        if !self.found
            && path.span().start().line == self.line
            && path
                .segments
                .iter()
                .any(|segment| segment.ident == self.name)
        {
            self.found = true;
            return;
        }
        syn::visit::visit_path(self, path);
    }
}

fn symbol_ref(
    workspace: &WorkspaceModel,
    discovery: &Discovery,
    item_id: GlobalItemId,
) -> SymbolRef {
    let item = &discovery.items[item_id];
    SymbolRef {
        item_id: Some(item_id),
        name: item.name.clone(),
        module_path: workspace
            .module_for_file(item.file_id)
            .map(|module| module.path.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duplicate_names_become_ambiguous_evidence() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("a.rs"), "pub fn dup() {}\n").unwrap();
        std::fs::write(tmp.path().join("b.rs"), "pub fn dup() {}\n").unwrap();
        std::fs::write(tmp.path().join("c.rs"), "pub fn caller() { dup(); }\n").unwrap();
        let workspace = crate::workspace_model::build_workspace_model(tmp.path()).unwrap();
        let discovery = crate::discover::discover_dir(tmp.path(), false).unwrap();

        let graph = build_symbol_graph(&workspace, &discovery, false, false);

        assert!(
            graph
                .edges
                .iter()
                .any(|edge| edge.confidence == EvidenceConfidence::Ambiguous)
        );
    }

    #[test]
    fn rg_candidates_filter_comments_and_strings() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("lib.rs"),
            r#"
pub fn target() {}
pub fn caller() {
    target();
    let _s = "target";
    // target
}
"#,
        )
        .unwrap();
        let workspace = crate::workspace_model::build_workspace_model(tmp.path()).unwrap();
        let discovery = crate::discover::discover_dir(tmp.path(), false).unwrap();

        let graph = build_symbol_graph(&workspace, &discovery, false, true);
        let rg_edges: Vec<_> = graph
            .edges
            .iter()
            .filter(|edge| edge.source == EvidenceSource::RgCandidate)
            .collect();

        assert!(rg_edges.iter().any(|edge| {
            edge.original_text
                .as_deref()
                .is_some_and(|text| text.contains("target();"))
                && edge.confidence == EvidenceConfidence::ScopedCandidate
        }));
        assert!(rg_edges.iter().any(|edge| {
            edge.original_text
                .as_deref()
                .is_some_and(|text| text.contains("\"target\""))
                && edge.confidence == EvidenceConfidence::TextCandidate
        }));
        assert!(rg_edges.iter().any(|edge| {
            edge.original_text
                .as_deref()
                .is_some_and(|text| text.contains("// target"))
                && edge.confidence == EvidenceConfidence::TextCandidate
        }));
    }
}
