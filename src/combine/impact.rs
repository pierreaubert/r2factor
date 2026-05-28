use anyhow::{Context, Result};
use quote::ToTokens;
use regex::Regex;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use syn::spanned::Spanned;
use syn::visit::Visit;

use crate::tokensave::Tokensave;

#[derive(Debug, Clone, Serialize)]
pub struct ImpactReport {
    pub available: bool,
    pub source: String,
    pub message: String,
    pub consumers: Vec<ConsumerImpact>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ConsumerImpact {
    pub file: PathBuf,
    pub line: usize,
    pub old: String,
    pub new: String,
    pub symbol: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ConsumerRewritePlan {
    pub rewrites: Vec<PlannedConsumerRewrite>,
    pub skipped: Vec<SkippedConsumerRewrite>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PlannedConsumerRewrite {
    pub file: PathBuf,
    pub replacements: usize,
    pub hunks: Vec<ConsumerRewriteHunk>,
    pub new_source: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ConsumerRewriteReport {
    pub file: PathBuf,
    pub replacements: usize,
    pub hunks: Vec<ConsumerRewriteHunk>,
    pub backup: PathBuf,
}

#[derive(Debug, Clone, Serialize)]
pub struct ConsumerRewriteHunk {
    pub line: usize,
    pub old: String,
    pub new: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SkippedConsumerRewrite {
    pub file: PathBuf,
    pub line: usize,
    pub old: String,
    pub reason: String,
}

#[derive(Debug, Clone)]
struct SymbolMove {
    old_module: String,
    symbol: String,
    new_module: String,
}

pub fn generate_impact_report_many(
    files: &[PathBuf],
    new_module: &str,
    use_tokensave: bool,
) -> Result<Option<ImpactReport>> {
    if !use_tokensave {
        return Ok(None);
    }
    Ok(Some(tokensave_impact_report(files, new_module)?))
}

pub fn plan_consumer_rewrites_many(
    files: &[PathBuf],
    parent_module: Option<&Path>,
    target_dir: &Path,
    new_module: &str,
) -> Result<ConsumerRewritePlan> {
    let Some(first_file) = files.first() else {
        return Ok(ConsumerRewritePlan {
            rewrites: Vec::new(),
            skipped: Vec::new(),
        });
    };
    let root = rewrite_root(first_file);
    let moves = symbol_moves(files, new_module)?;
    let mut excluded_files = BTreeSet::new();
    for file in files {
        excluded_files.insert(canonical_or_self(file));
    }
    if let Some(parent) = parent_module {
        excluded_files.insert(canonical_or_self(parent));
    }
    let target_dir = canonical_or_self(target_dir);

    let mut rewrites = Vec::new();
    let mut skipped = Vec::new();
    for path in rust_files_under(&root)? {
        let canonical = canonical_or_self(&path);
        if excluded_files.contains(&canonical) || canonical.starts_with(&target_dir) {
            continue;
        }
        let src = fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
        let (new_source, replacements) = rewrite_source(&src, &moves)?;
        skipped.extend(skipped_rewrite_candidates(&path, &new_source, &moves)?);
        if replacements > 0 {
            let hunks = rewrite_hunks(&src, &new_source);
            rewrites.push(PlannedConsumerRewrite {
                file: path,
                replacements,
                hunks,
                new_source,
            });
        }
    }
    Ok(ConsumerRewritePlan { rewrites, skipped })
}

fn tokensave_impact_report(files: &[PathBuf], new_module: &str) -> Result<ImpactReport> {
    let Some(first_file) = files.first() else {
        return Ok(ImpactReport {
            available: true,
            source: "tokensave".to_string(),
            message: "No input files provided.".to_string(),
            consumers: Vec::new(),
        });
    };
    let Some(root) = Tokensave::locate(first_file) else {
        return Ok(ImpactReport {
            available: false,
            source: "tokensave".to_string(),
            message: "Impact report requires a .tokensave/tokensave.db index.".to_string(),
            consumers: Vec::new(),
        });
    };
    let ts = match Tokensave::open_safe(&root) {
        Ok(ts) => ts,
        Err(e) => {
            return Ok(ImpactReport {
                available: false,
                source: "tokensave".to_string(),
                message: format!("Impact report requires a readable tokensave index: {e}"),
                consumers: Vec::new(),
            });
        }
    };

    let moves = symbol_moves(files, new_module)?;
    if moves.is_empty() {
        return Ok(ImpactReport {
            available: true,
            source: "tokensave".to_string(),
            message: "No public symbols found in the input files.".to_string(),
            consumers: Vec::new(),
        });
    }

    let mut by_symbol: BTreeMap<String, Vec<&SymbolMove>> = BTreeMap::new();
    for mv in &moves {
        by_symbol.entry(mv.symbol.clone()).or_default().push(mv);
    }

    let symbols: Vec<String> = by_symbol.keys().cloned().collect();
    let old_files: Vec<String> = files.iter().map(|file| rel_path(&root, file)).collect();
    let rows = query_unresolved_refs(&ts, &symbols, &old_files)?;
    let mut consumers = Vec::new();
    let mut seen = BTreeSet::new();
    for row in rows {
        let Some(moves) = by_symbol.get(&row.reference_name) else {
            continue;
        };
        let abs_path = root.join(&row.file_path);
        let line_text = read_line(&abs_path, row.line).unwrap_or_default();
        for mv in moves {
            for (old, new) in rewrite_patterns(mv).into_iter().take(4) {
                if !line_text.contains(&old) {
                    continue;
                }
                let key = (row.file_path.clone(), row.line, old.clone(), new.clone());
                if seen.insert(key) {
                    consumers.push(ConsumerImpact {
                        file: abs_path.clone(),
                        line: row.line,
                        old,
                        new,
                        symbol: row.reference_name.clone(),
                    });
                }
            }
        }
    }

    let message = if consumers.is_empty() {
        "No consumer path rewrites found in tokensave unresolved-reference lines.".to_string()
    } else {
        format!("{} consumer path rewrite(s) found.", consumers.len())
    };
    Ok(ImpactReport {
        available: true,
        source: "tokensave".to_string(),
        message,
        consumers,
    })
}

struct RefRow {
    reference_name: String,
    file_path: String,
    line: usize,
}

fn query_unresolved_refs(
    ts: &Tokensave,
    symbols: &[String],
    old_files: &[String],
) -> Result<Vec<RefRow>> {
    if symbols.is_empty() {
        return Ok(Vec::new());
    }
    let placeholders = (0..symbols.len())
        .map(|_| "?")
        .collect::<Vec<_>>()
        .join(", ");
    let excluded = if old_files.is_empty() {
        String::new()
    } else {
        let placeholders = (0..old_files.len())
            .map(|_| "?")
            .collect::<Vec<_>>()
            .join(", ");
        format!(" AND file_path NOT IN ({placeholders})")
    };
    let sql = format!(
        "SELECT reference_name, file_path, line \
         FROM unresolved_refs \
         WHERE reference_name IN ({placeholders}){excluded} \
         ORDER BY file_path, line \
         LIMIT 2000"
    );
    let mut params: Vec<libsql::Value> = symbols
        .iter()
        .map(|s| libsql::Value::Text(s.clone()))
        .collect();
    params.extend(old_files.iter().cloned().map(libsql::Value::Text));

    ts.rt.block_on(async {
        let mut rows = ts
            .db
            .conn()
            .query(&sql, libsql::params_from_iter(params))
            .await
            .context("query tokensave unresolved refs")?;
        let mut out = Vec::new();
        while let Some(row) = rows.next().await.context("read tokensave row")? {
            out.push(RefRow {
                reference_name: row.get::<String>(0).unwrap_or_default(),
                file_path: row.get::<String>(1).unwrap_or_default(),
                line: row.get::<i64>(2).unwrap_or_default() as usize,
            });
        }
        anyhow::Ok(out)
    })
}

fn symbol_moves(files: &[PathBuf], new_module: &str) -> Result<Vec<SymbolMove>> {
    let mut out = Vec::new();
    for file in files {
        let old_module = file
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or_default()
            .to_string();
        let src = fs::read_to_string(file).with_context(|| format!("read {}", file.display()))?;
        let ast = syn::parse_file(&src).with_context(|| format!("parse {}", file.display()))?;
        for item in ast.items {
            if let Some(symbol) = public_item_name(&item) {
                out.push(SymbolMove {
                    old_module: old_module.clone(),
                    symbol,
                    new_module: new_module.to_string(),
                });
            }
        }
    }
    Ok(out)
}

fn public_item_name(item: &syn::Item) -> Option<String> {
    let public = |vis: &syn::Visibility| {
        matches!(
            vis,
            syn::Visibility::Public(_) | syn::Visibility::Restricted(_)
        )
    };
    match item {
        syn::Item::Const(i) if public(&i.vis) => Some(i.ident.to_string()),
        syn::Item::Enum(i) if public(&i.vis) => Some(i.ident.to_string()),
        syn::Item::Fn(i) if public(&i.vis) => Some(i.sig.ident.to_string()),
        syn::Item::Mod(i) if public(&i.vis) => Some(i.ident.to_string()),
        syn::Item::Static(i) if public(&i.vis) => Some(i.ident.to_string()),
        syn::Item::Struct(i) if public(&i.vis) => Some(i.ident.to_string()),
        syn::Item::Trait(i) if public(&i.vis) => Some(i.ident.to_string()),
        syn::Item::TraitAlias(i) if public(&i.vis) => Some(i.ident.to_string()),
        syn::Item::Type(i) if public(&i.vis) => Some(i.ident.to_string()),
        syn::Item::Union(i) if public(&i.vis) => Some(i.ident.to_string()),
        syn::Item::Macro(i) => i.ident.as_ref().map(ToString::to_string),
        _ => None,
    }
}

fn rewrite_source(src: &str, moves: &[SymbolMove]) -> Result<(String, usize)> {
    match syn::parse_file(src) {
        Ok(ast) => rewrite_source_ast(src, &ast, moves),
        Err(_) => rewrite_source_text_fallback(src, moves),
    }
}

fn rewrite_source_ast(src: &str, ast: &syn::File, moves: &[SymbolMove]) -> Result<(String, usize)> {
    let (out, ast_replacements) = rewrite_ast_paths(src, ast, moves);
    let (out, use_replacements) = rewrite_use_imports(&out, moves)?;
    Ok((out, ast_replacements + use_replacements))
}

fn rewrite_source_text_fallback(src: &str, moves: &[SymbolMove]) -> Result<(String, usize)> {
    let mut out = src.to_string();
    let mut replacements = 0;
    for mv in moves {
        for (old, new) in rewrite_patterns(mv).into_iter().take(6) {
            let n = out.matches(&old).count();
            if n > 0 {
                out = out.replace(&old, &new);
                replacements += n;
            }
        }
        let bare = Regex::new(&format!(
            r"(^|[^A-Za-z0-9_:]){}::",
            regex::escape(&mv.old_module)
        ))?;
        let new_bare = format!("${{1}}{}::{}::", mv.new_module, mv.old_module);
        let n = bare.find_iter(&out).count();
        if n > 0 {
            out = bare.replace_all(&out, new_bare.as_str()).into_owned();
            replacements += n;
        }
    }
    let (rewritten_uses, use_replacements) = rewrite_use_imports(&out, moves)?;
    out = rewritten_uses;
    replacements += use_replacements;
    Ok((out, replacements))
}

#[derive(Debug, Clone, Eq, PartialEq, Ord, PartialOrd)]
struct SourceEdit {
    start: usize,
    end: usize,
    replacement: String,
}

fn rewrite_ast_paths(src: &str, ast: &syn::File, moves: &[SymbolMove]) -> (String, usize) {
    let line_starts = line_starts(src);
    let mut visitor = PathRewriteCollector {
        src,
        line_starts: &line_starts,
        moves,
        edits: Vec::new(),
    };
    visitor.visit_file(ast);
    apply_source_edits(src, visitor.edits)
}

struct PathRewriteCollector<'a> {
    src: &'a str,
    line_starts: &'a [usize],
    moves: &'a [SymbolMove],
    edits: Vec<SourceEdit>,
}

impl<'ast> Visit<'ast> for PathRewriteCollector<'_> {
    fn visit_path(&mut self, path: &'ast syn::Path) {
        if let Some(replacement) = rewritten_path(path, self.moves)
            && let Some((start, end)) = span_offsets(path.span(), self.line_starts, self.src.len())
            && start < end
            && self
                .src
                .get(start..end)
                .is_some_and(|old| old != replacement)
        {
            self.edits.push(SourceEdit {
                start,
                end,
                replacement,
            });
        }
        syn::visit::visit_path(self, path);
    }
}

fn rewritten_path(path: &syn::Path, moves: &[SymbolMove]) -> Option<String> {
    let first = path.segments.first()?;
    let first_name = first.ident.to_string();
    let mut rewritten = path.clone();

    if is_root_segment(&first.ident) {
        let second = path.segments.iter().nth(1)?;
        if moves.iter().any(|mv| second.ident == mv.new_module) {
            return None;
        }
        let mv = moves.iter().find(|mv| second.ident == mv.old_module)?;
        rewritten.segments.insert(1, path_segment(&mv.new_module));
    } else {
        let mv = moves.iter().find(|mv| first_name == mv.old_module)?;
        rewritten.segments.insert(0, path_segment(&mv.new_module));
    }

    Some(rewritten.to_token_stream().to_string())
}

fn path_segment(name: &str) -> syn::PathSegment {
    syn::PathSegment {
        ident: syn::Ident::new(name, proc_macro2::Span::call_site()),
        arguments: syn::PathArguments::None,
    }
}

fn apply_source_edits(src: &str, mut edits: Vec<SourceEdit>) -> (String, usize) {
    if edits.is_empty() {
        return (src.to_string(), 0);
    }
    edits.sort();
    edits.dedup();

    let mut filtered = Vec::new();
    let mut last_end = 0;
    for edit in edits {
        if edit.start < last_end {
            continue;
        }
        last_end = edit.end;
        filtered.push(edit);
    }

    let replacements = filtered.len();
    let mut out = src.to_string();
    for edit in filtered.into_iter().rev() {
        out.replace_range(edit.start..edit.end, &edit.replacement);
    }
    (out, replacements)
}

fn span_offsets(
    span: proc_macro2::Span,
    line_starts: &[usize],
    src_len: usize,
) -> Option<(usize, usize)> {
    let start = offset_for(span.start(), line_starts, src_len)?;
    let end = offset_for(span.end(), line_starts, src_len)?;
    Some((start, end))
}

fn line_starts(src: &str) -> Vec<usize> {
    let mut starts = vec![0];
    for (idx, byte) in src.bytes().enumerate() {
        if byte == b'\n' {
            starts.push(idx + 1);
        }
    }
    starts
}

fn offset_for(
    location: proc_macro2::LineColumn,
    line_starts: &[usize],
    src_len: usize,
) -> Option<usize> {
    let line = location.line.checked_sub(1)?;
    let start = *line_starts.get(line)?;
    let offset = start.checked_add(location.column)?;
    (offset <= src_len).then_some(offset)
}

fn rewrite_use_imports(src: &str, moves: &[SymbolMove]) -> Result<(String, usize)> {
    let mut out = String::with_capacity(src.len());
    let mut replacements = 0;

    for segment in src.split_inclusive('\n') {
        let (line, newline) = segment
            .strip_suffix('\n')
            .map(|line| (line, "\n"))
            .unwrap_or((segment, ""));
        let trimmed = line.trim_start();
        let indent_len = line.len() - trimmed.len();
        let indent = &line[..indent_len];

        if (trimmed.starts_with("use ") || trimmed.starts_with("pub use "))
            && trimmed.ends_with(';')
            && !trimmed.contains("//")
        {
            let mut item: syn::ItemUse =
                syn::parse_str(trimmed).with_context(|| format!("parse use item `{trimmed}`"))?;
            let n = rewrite_use_tree_after_root(&mut item.tree, moves);
            if n > 0 {
                out.push_str(indent);
                out.push_str(&item.to_token_stream().to_string());
                out.push_str(newline);
                replacements += n;
                continue;
            }
        }

        out.push_str(segment);
    }

    Ok((out, replacements))
}

fn rewrite_use_tree_after_root(tree: &mut syn::UseTree, moves: &[SymbolMove]) -> usize {
    match tree {
        syn::UseTree::Path(path) if is_root_segment(&path.ident) => {
            rewrite_use_subtree(&mut path.tree, moves)
        }
        _ => 0,
    }
}

fn rewrite_use_subtree(tree: &mut syn::UseTree, moves: &[SymbolMove]) -> usize {
    match tree {
        syn::UseTree::Path(path) => {
            if moves.iter().any(|mv| path.ident == mv.new_module) {
                0
            } else if let Some(mv) = moves.iter().find(|mv| path.ident == mv.old_module) {
                let leaf = (*path.tree).clone();
                *tree = prefixed_use_tree(&mv.new_module, leaf);
                1
            } else {
                rewrite_use_subtree(&mut path.tree, moves)
            }
        }
        syn::UseTree::Name(name) => {
            if let Some(mv) = moves.iter().find(|mv| name.ident == mv.old_module) {
                let leaf = syn::UseTree::Name(name.clone());
                *tree = prefixed_use_tree(&mv.new_module, leaf);
                1
            } else {
                0
            }
        }
        syn::UseTree::Rename(rename) => {
            if let Some(mv) = moves.iter().find(|mv| rename.ident == mv.old_module) {
                let leaf = syn::UseTree::Rename(rename.clone());
                *tree = prefixed_use_tree(&mv.new_module, leaf);
                1
            } else {
                0
            }
        }
        syn::UseTree::Group(group) => group
            .items
            .iter_mut()
            .map(|tree| rewrite_use_subtree(tree, moves))
            .sum(),
        _ => 0,
    }
}

fn prefixed_use_tree(module: &str, leaf: syn::UseTree) -> syn::UseTree {
    syn::UseTree::Path(syn::UsePath {
        ident: syn::Ident::new(module, leaf.span()),
        colon2_token: Default::default(),
        tree: Box::new(leaf),
    })
}

fn is_root_segment(ident: &syn::Ident) -> bool {
    ident == "crate" || ident == "super" || ident == "self"
}

fn rewrite_hunks(old: &str, new: &str) -> Vec<ConsumerRewriteHunk> {
    old.lines()
        .zip(new.lines())
        .enumerate()
        .filter(|(_, (old, new))| old != new)
        .map(|(idx, (old, new))| ConsumerRewriteHunk {
            line: idx + 1,
            old: old.to_string(),
            new: new.to_string(),
        })
        .collect()
}

fn skipped_rewrite_candidates(
    path: &Path,
    src: &str,
    moves: &[SymbolMove],
) -> Result<Vec<SkippedConsumerRewrite>> {
    let mut skipped = Vec::new();
    let mut seen = BTreeSet::new();
    for (idx, line) in src.lines().enumerate() {
        for mv in moves {
            for old in skipped_module_patterns(mv) {
                let pattern = Regex::new(&format!(
                    r"(^|[^A-Za-z0-9_:]){}([^A-Za-z0-9_:]|$)",
                    regex::escape(&old)
                ))?;
                if !pattern.is_match(line) {
                    continue;
                }
                let key = (idx + 1, old.clone());
                if seen.insert(key) {
                    skipped.push(SkippedConsumerRewrite {
                        file: path.to_path_buf(),
                        line: idx + 1,
                        old,
                        reason: "module path has no item segment; skipped by conservative consumer rewriter"
                            .to_string(),
                    });
                }
            }
        }
    }
    Ok(skipped)
}

fn rewrite_patterns(mv: &SymbolMove) -> Vec<(String, String)> {
    let old = &mv.old_module;
    let new = format!("{}::{}", mv.new_module, old);
    vec![
        (format!("crate::{old}::"), format!("crate::{new}::")),
        (format!("super::{old}::"), format!("super::{new}::")),
        (format!("self::{old}::"), format!("self::{new}::")),
        (format!("crate::{{{old}::"), format!("crate::{{{new}::")),
        (format!("super::{{{old}::"), format!("super::{{{new}::")),
        (format!("self::{{{old}::"), format!("self::{{{new}::")),
        (format!("use {old}::"), format!("use {new}::")),
    ]
}

fn skipped_module_patterns(mv: &SymbolMove) -> Vec<String> {
    let old = &mv.old_module;
    vec![
        format!("crate::{old}"),
        format!("super::{old}"),
        format!("self::{old}"),
    ]
}

fn rewrite_root(file: &Path) -> PathBuf {
    let mut cur = file
        .canonicalize()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
        .or_else(|| file.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."));
    loop {
        if cur.join("Cargo.toml").is_file() {
            return cur;
        }
        if !cur.pop() {
            return file.parent().unwrap_or(Path::new(".")).to_path_buf();
        }
    }
}

fn rust_files_under(root: &Path) -> Result<Vec<PathBuf>> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
        for entry in fs::read_dir(dir).with_context(|| format!("read_dir {}", dir.display()))? {
            let entry = entry?;
            let path = entry.path();
            let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
            if path.is_dir() {
                if matches!(name, ".git" | ".tokensave" | "target") {
                    continue;
                }
                walk(&path, out)?;
            } else if path.extension().is_some_and(|ext| ext == "rs") && !name.ends_with(".bak") {
                out.push(path);
            }
        }
        Ok(())
    }
    let mut out = Vec::new();
    walk(root, &mut out)?;
    Ok(out)
}

fn rel_path(root: &Path, path: &Path) -> String {
    path.canonicalize()
        .ok()
        .and_then(|p| p.strip_prefix(root).ok().map(Path::to_path_buf))
        .unwrap_or_else(|| path.to_path_buf())
        .to_string_lossy()
        .to_string()
}

fn read_line(path: &Path, line: usize) -> Result<String> {
    let src = fs::read_to_string(path)?;
    Ok(src
        .lines()
        .nth(line.saturating_sub(1))
        .unwrap_or_default()
        .to_string())
}

fn canonical_or_self(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}
