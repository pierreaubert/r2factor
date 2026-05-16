//! Flatten a consolidated single-file module by dissolving top-level inline
//! `mod bucket { ... }` blocks into the parent scope with mechanical renames.
//!
//! This is deliberately the single-file post-pass only. It rewrites the merged
//! file itself and leaves repo-wide consumer rewrites to a later
//! tokensave-backed mode.

use anyhow::{Context, Result, bail};
use proc_macro2::Span;
use quote::ToTokens;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use syn::spanned::Spanned;
use syn::visit::{self, Visit};

use crate::promote::line_col_to_byte_offset;

#[derive(Debug, serde::Serialize)]
pub struct FlattenReport {
    pub target: PathBuf,
    pub backup: Option<PathBuf>,
    pub rewrites: usize,
    pub warnings: Vec<String>,
    pub source_bytes: usize,
}

pub struct FlattenOptions {
    pub write: bool,
}

#[derive(Debug, Clone)]
struct Replacement {
    start: usize,
    end: usize,
    text: String,
}

type RenameMap = BTreeMap<(String, String), String>;
type ImportAliasMap = BTreeMap<(String, String), String>;

pub fn flatten_dry_run(path: &Path) -> Result<String> {
    let src = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let (flattened, _) = flatten_source(&src)?;
    Ok(flattened)
}

pub fn flatten_write(path: &Path, opts: &FlattenOptions) -> Result<FlattenReport> {
    let src = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let (flattened, details) = flatten_source(&src)?;
    let source_bytes = flattened.len();

    if !opts.write {
        return Ok(FlattenReport {
            target: path.to_path_buf(),
            backup: None,
            rewrites: details.rewrite_count,
            warnings: details.warnings,
            source_bytes,
        });
    }

    let mut backup = path.to_path_buf();
    let name = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("flattened.rs");
    backup.set_file_name(format!("{name}.bak"));
    fs::copy(path, &backup)
        .with_context(|| format!("backup {} -> {}", path.display(), backup.display()))?;
    fs::write(path, flattened).with_context(|| format!("write {}", path.display()))?;

    Ok(FlattenReport {
        target: path.to_path_buf(),
        backup: Some(backup),
        rewrites: details.rewrite_count,
        warnings: details.warnings,
        source_bytes,
    })
}

struct FlattenDetails {
    rewrite_count: usize,
    warnings: Vec<String>,
}

fn flatten_source(src: &str) -> Result<(String, FlattenDetails)> {
    let file = syn::parse_file(src).context("parse Rust source")?;
    let inline_mods = top_level_inline_mods(&file);
    if inline_mods.is_empty() {
        bail!("no top-level inline modules found to flatten");
    }

    let rename_map = build_rename_map(&inline_mods);
    if rename_map.is_empty() {
        bail!("inline modules contain no named items to flatten");
    }

    let inline_ranges = inline_content_ranges(src, &inline_mods)?;
    let mut warnings = Vec::new();
    let mut replacements = Vec::new();
    let import_aliases = build_import_alias_map(&file, &rename_map);
    collect_decl_replacements(src, &inline_mods, &rename_map, &mut replacements);
    collect_pub_super_visibility_replacements(src, &inline_ranges, &mut replacements);
    collect_path_replacements(
        src,
        &file,
        &rename_map,
        &import_aliases,
        &inline_ranges,
        &mut replacements,
    );
    collect_use_replacements(
        src,
        &file,
        &rename_map,
        &inline_ranges,
        &mut replacements,
        &mut warnings,
    );

    let mut out = String::new();
    let inner_attrs = inner_attrs_source(src, &file);
    if !inner_attrs.is_empty() {
        out.push_str(inner_attrs.trim_end());
        out.push_str("\n\n");
    }

    for item in &file.items {
        if let syn::Item::Mod(m) = item
            && m.content.is_some()
            && let Some((content_start, content_end)) = inline_ranges.get(&m.ident.to_string())
        {
            let rendered = render_inline_mod_body(
                src,
                m,
                *content_start,
                *content_end,
                &replacements,
                &mut warnings,
            )?;
            if !rendered.trim().is_empty() {
                out.push_str(rendered.trim_end());
                out.push_str("\n\n");
            }
            continue;
        }

        let (start, end) = item_byte_range(src, item)?;
        let rendered = apply_replacements_to_slice(src, start, end, &replacements);
        if !rendered.trim().is_empty() {
            out.push_str(rendered.trim_end());
            out.push_str("\n\n");
        }
    }

    let (out, cleanup_rewrites) = remove_duplicate_plain_uses(&out)?;

    Ok((
        out,
        FlattenDetails {
            rewrite_count: replacements.len() + cleanup_rewrites,
            warnings,
        },
    ))
}

fn top_level_inline_mods(file: &syn::File) -> Vec<&syn::ItemMod> {
    file.items
        .iter()
        .filter_map(|item| match item {
            syn::Item::Mod(m) if m.content.is_some() => Some(m),
            _ => None,
        })
        .collect()
}

fn build_rename_map(mods: &[&syn::ItemMod]) -> RenameMap {
    let mut map = RenameMap::new();
    for m in mods {
        let bucket = m.ident.to_string();
        let Some((_, items)) = &m.content else {
            continue;
        };
        for item in items {
            if let Some((name, _)) = named_item(item) {
                map.insert((bucket.clone(), name.clone()), format!("{bucket}_{name}"));
            }
        }
    }
    map
}

fn collect_decl_replacements(
    src: &str,
    mods: &[&syn::ItemMod],
    map: &RenameMap,
    out: &mut Vec<Replacement>,
) {
    for m in mods {
        let bucket = m.ident.to_string();
        let Some((_, items)) = &m.content else {
            continue;
        };
        for item in items {
            let Some((name, span)) = named_item(item) else {
                continue;
            };
            let Some(new_name) = map.get(&(bucket.clone(), name.clone())) else {
                continue;
            };
            if let Some((start, end)) = span_range(src, span) {
                out.push(Replacement {
                    start,
                    end,
                    text: new_name.clone(),
                });
            }
        }
    }
}

fn collect_pub_super_visibility_replacements(
    src: &str,
    inline_ranges: &BTreeMap<String, (usize, usize)>,
    out: &mut Vec<Replacement>,
) {
    let Ok(file) = syn::parse_file(src) else {
        return;
    };
    let mut visitor = PubSuperVisibilityVisitor {
        src,
        inline_ranges,
        out,
    };
    visitor.visit_file(&file);
}

struct PubSuperVisibilityVisitor<'a> {
    src: &'a str,
    inline_ranges: &'a BTreeMap<String, (usize, usize)>,
    out: &'a mut Vec<Replacement>,
}

impl<'ast> Visit<'ast> for PubSuperVisibilityVisitor<'_> {
    fn visit_visibility(&mut self, vis: &'ast syn::Visibility) {
        let syn::Visibility::Restricted(restricted) = vis else {
            return;
        };
        if restricted.in_token.is_some() || !restricted.path.is_ident("super") {
            return;
        }
        let Some((start, end)) = span_range(self.src, restricted.span()) else {
            return;
        };
        if !self.is_inside_inline_mod(start) {
            return;
        }
        out_replacement(self.out, start, end, "");
    }
}

impl PubSuperVisibilityVisitor<'_> {
    fn is_inside_inline_mod(&self, offset: usize) -> bool {
        self.inline_ranges
            .values()
            .any(|(start, end)| (*start..=*end).contains(&offset))
    }
}

fn collect_path_replacements(
    src: &str,
    file: &syn::File,
    map: &RenameMap,
    import_aliases: &ImportAliasMap,
    inline_ranges: &BTreeMap<String, (usize, usize)>,
    out: &mut Vec<Replacement>,
) {
    let mut visitor = PathRewriteVisitor {
        src,
        map,
        import_aliases,
        inline_ranges,
        out,
        local_bindings: Vec::new(),
    };
    visitor.visit_file(file);
}

struct PathRewriteVisitor<'a> {
    src: &'a str,
    map: &'a RenameMap,
    import_aliases: &'a ImportAliasMap,
    inline_ranges: &'a BTreeMap<String, (usize, usize)>,
    out: &'a mut Vec<Replacement>,
    local_bindings: Vec<(String, usize)>,
}

impl<'ast> Visit<'ast> for PathRewriteVisitor<'_> {
    fn visit_item_fn(&mut self, node: &'ast syn::ItemFn) {
        let saved = self.local_bindings.len();
        self.collect_fn_arg_bindings(&node.sig.inputs);
        visit::visit_item_fn(self, node);
        self.local_bindings.truncate(saved);
    }

    fn visit_impl_item_fn(&mut self, node: &'ast syn::ImplItemFn) {
        let saved = self.local_bindings.len();
        self.collect_fn_arg_bindings(&node.sig.inputs);
        visit::visit_impl_item_fn(self, node);
        self.local_bindings.truncate(saved);
    }

    fn visit_block(&mut self, node: &'ast syn::Block) {
        let saved = self.local_bindings.len();
        visit::visit_block(self, node);
        self.local_bindings.truncate(saved);
    }

    fn visit_local(&mut self, node: &'ast syn::Local) {
        if let Some(init) = &node.init {
            self.visit_expr(&init.expr);
        }
        self.visit_pat(&node.pat);
        self.collect_pat_bindings(&node.pat);
    }

    fn visit_expr_for_loop(&mut self, node: &'ast syn::ExprForLoop) {
        self.visit_expr(&node.expr);
        self.visit_pat(&node.pat);
        let saved = self.local_bindings.len();
        self.collect_pat_bindings(&node.pat);
        self.visit_block(&node.body);
        self.local_bindings.truncate(saved);
    }

    fn visit_arm(&mut self, node: &'ast syn::Arm) {
        self.visit_pat(&node.pat);
        let saved = self.local_bindings.len();
        self.collect_pat_bindings(&node.pat);
        if let Some((_, guard)) = &node.guard {
            self.visit_expr(guard);
        }
        self.visit_expr(&node.body);
        self.local_bindings.truncate(saved);
    }

    fn visit_expr_closure(&mut self, node: &'ast syn::ExprClosure) {
        let saved = self.local_bindings.len();
        for input in &node.inputs {
            self.visit_pat(input);
            self.collect_pat_bindings(input);
        }
        self.visit_expr(&node.body);
        self.local_bindings.truncate(saved);
    }

    fn visit_path(&mut self, path: &'ast syn::Path) {
        self.collect_for_path(path);
        visit::visit_path(self, path);
    }
}

impl PathRewriteVisitor<'_> {
    fn collect_fn_arg_bindings(
        &mut self,
        inputs: &syn::punctuated::Punctuated<syn::FnArg, syn::token::Comma>,
    ) {
        for input in inputs {
            if let syn::FnArg::Typed(arg) = input {
                self.collect_pat_bindings(&arg.pat);
            }
        }
    }

    fn collect_pat_bindings(&mut self, pat: &syn::Pat) {
        let mut collector = BindingCollector {
            src: self.src,
            bindings: &mut self.local_bindings,
        };
        collector.visit_pat(pat);
    }

    fn collect_for_path(&mut self, path: &syn::Path) {
        let segments: Vec<&syn::PathSegment> = path.segments.iter().collect();
        if segments.is_empty() {
            return;
        }
        let path_start = span_start(self.src, path.span()).unwrap_or(usize::MAX);
        let current_bucket = self.bucket_for_offset(path_start).map(str::to_string);

        if let Some(bucket) = current_bucket.as_deref() {
            self.collect_unwrapped_prefix_rewrite(&segments);
            self.collect_same_bucket_rewrite(bucket, &segments);
        }
        self.collect_bucket_pair_rewrites(&segments);
    }

    fn bucket_for_offset(&self, offset: usize) -> Option<&str> {
        self.inline_ranges
            .iter()
            .find_map(|(bucket, (start, end))| {
                if (*start..=*end).contains(&offset) {
                    Some(bucket.as_str())
                } else {
                    None
                }
            })
    }

    fn collect_unwrapped_prefix_rewrite(&mut self, segments: &[&syn::PathSegment]) {
        if segments.len() < 2 || segments[0].ident != "super" {
            return;
        }
        let Some(start) = span_start(self.src, segments[0].ident.span()) else {
            return;
        };
        let Some(end) = span_start(self.src, segments[1].ident.span()) else {
            return;
        };
        self.out.push(Replacement {
            start,
            end,
            text: String::new(),
        });
    }

    fn collect_same_bucket_rewrite(&mut self, bucket: &str, segments: &[&syn::PathSegment]) {
        let (replace_start, replace_end, name) = if segments.len() == 1 {
            let ident = &segments[0].ident;
            let Some(start) = span_start(self.src, ident.span()) else {
                return;
            };
            let Some(end) = span_end(self.src, ident.span()) else {
                return;
            };
            (start, end, ident.to_string())
        } else if segments.len() >= 2 && segments[0].ident == "self" {
            let Some(start) = span_start(self.src, segments[0].ident.span()) else {
                return;
            };
            let Some(end) = span_end(self.src, segments[1].ident.span()) else {
                return;
            };
            (start, end, segments[1].ident.to_string())
        } else {
            return;
        };

        if let Some(new_name) = self
            .map
            .get(&(bucket.to_string(), name.clone()))
            .or_else(|| self.import_aliases.get(&(bucket.to_string(), name)))
        {
            if segments.len() == 1
                && self.has_visible_local_binding(&segments[0].ident, replace_start)
            {
                return;
            }
            self.out.push(Replacement {
                start: replace_start,
                end: replace_end,
                text: new_name.clone(),
            });
        }
    }

    fn has_visible_local_binding(&self, ident: &syn::Ident, offset: usize) -> bool {
        let name = ident.to_string();
        self.local_bindings
            .iter()
            .any(|(local, binding_offset)| local == &name && *binding_offset <= offset)
    }

    fn collect_bucket_pair_rewrites(&mut self, segments: &[&syn::PathSegment]) {
        for pair in segments.windows(2) {
            let bucket = pair[0].ident.to_string();
            let name = pair[1].ident.to_string();
            let Some(new_name) = self.map.get(&(bucket, name)) else {
                continue;
            };
            let Some(start) = span_start(self.src, pair[0].ident.span()) else {
                continue;
            };
            let Some(end) = span_end(self.src, pair[1].ident.span()) else {
                continue;
            };
            self.out.push(Replacement {
                start,
                end,
                text: new_name.clone(),
            });
        }
    }
}

struct BindingCollector<'a, 'b> {
    src: &'a str,
    bindings: &'b mut Vec<(String, usize)>,
}

impl<'ast> Visit<'ast> for BindingCollector<'_, '_> {
    fn visit_pat_ident(&mut self, node: &'ast syn::PatIdent) {
        if let Some(start) = span_start(self.src, node.ident.span()) {
            self.bindings.push((node.ident.to_string(), start));
        }
        visit::visit_pat_ident(self, node);
    }
}

fn collect_use_replacements(
    src: &str,
    file: &syn::File,
    map: &RenameMap,
    inline_ranges: &BTreeMap<String, (usize, usize)>,
    out: &mut Vec<Replacement>,
    warnings: &mut Vec<String>,
) {
    for item in &file.items {
        collect_item_use_replacements(src, item, map, inline_ranges, out, warnings);
        if let syn::Item::Mod(m) = item
            && let Some((_, items)) = &m.content
        {
            for inner in items {
                collect_item_use_replacements(src, inner, map, inline_ranges, out, warnings);
            }
        }
    }
}

fn collect_item_use_replacements(
    src: &str,
    item: &syn::Item,
    map: &RenameMap,
    inline_ranges: &BTreeMap<String, (usize, usize)>,
    out: &mut Vec<Replacement>,
    warnings: &mut Vec<String>,
) {
    let syn::Item::Use(item_use) = item else {
        return;
    };
    let Ok((item_start, item_end)) = item_byte_range(src, item) else {
        return;
    };
    let use_start = span_start(src, item_use.use_token.span).unwrap_or(item_start);
    let current_bucket = inline_ranges.iter().find_map(|(bucket, (start, end))| {
        if (*start..=*end).contains(&item_start) {
            Some(bucket.as_str())
        } else {
            None
        }
    });

    if let Some(bucket) = current_bucket
        && let Some(replacement) =
            simple_inline_import_removal(item_use, map, bucket, item_start, item_end)
    {
        out.push(replacement);
        return;
    }

    if let Some(_bucket) = current_bucket
        && let Some(replacement) =
            simple_parent_import_rewrite(src, item_use, map, item_start, item_end, use_start)
    {
        out.push(replacement);
        return;
    }

    if let Some(replacement) =
        simple_bucket_group_use_replacement(src, item_use, map, item_start, item_end, use_start)
    {
        out.push(replacement);
        return;
    }

    if let Some(replacement) =
        simple_glob_use_replacement(src, item_use, map, item_start, item_end, use_start)
    {
        out.push(replacement);
        return;
    }

    collect_use_tree_replacements(src, &item_use.tree, map, current_bucket, out, warnings);
}

fn build_import_alias_map(file: &syn::File, map: &RenameMap) -> ImportAliasMap {
    let mut out = ImportAliasMap::new();
    for item in &file.items {
        let syn::Item::Mod(m) = item else {
            continue;
        };
        let Some((_, items)) = &m.content else {
            continue;
        };
        let bucket = m.ident.to_string();
        for inner in items {
            let syn::Item::Use(item_use) = inner else {
                continue;
            };
            let Some(aliases) = simple_inline_import_aliases(&item_use.tree, map) else {
                continue;
            };
            for (local, renamed) in aliases {
                out.insert((bucket.clone(), local), renamed);
            }
        }
    }
    out
}

fn simple_inline_import_alias(tree: &syn::UseTree, map: &RenameMap) -> Option<(String, String)> {
    simple_inline_import_aliases(tree, map)?.into_iter().next()
}

fn simple_inline_import_aliases(
    tree: &syn::UseTree,
    map: &RenameMap,
) -> Option<Vec<(String, String)>> {
    let syn::UseTree::Path(super_path) = tree else {
        return None;
    };
    if super_path.ident != "super" {
        return None;
    }
    aliases_from_super_import_child(super_path.tree.as_ref(), map)
}

fn aliases_from_super_import_child(
    tree: &syn::UseTree,
    map: &RenameMap,
) -> Option<Vec<(String, String)>> {
    match tree {
        syn::UseTree::Path(bucket_path) => {
            let source_bucket = bucket_path.ident.to_string();
            aliases_from_bucket_import_child(&source_bucket, bucket_path.tree.as_ref(), map)
        }
        syn::UseTree::Group(group) => {
            let mut out = Vec::new();
            for child in &group.items {
                out.extend(aliases_from_super_import_child(child, map)?);
            }
            (!out.is_empty()).then_some(out)
        }
        _ => None,
    }
}

fn aliases_from_bucket_import_child(
    source_bucket: &str,
    tree: &syn::UseTree,
    map: &RenameMap,
) -> Option<Vec<(String, String)>> {
    match tree {
        syn::UseTree::Name(n) => {
            let imported = n.ident.to_string();
            let renamed = map.get(&(source_bucket.to_string(), imported.clone()))?;
            Some(vec![(imported, renamed.clone())])
        }
        syn::UseTree::Rename(r) => {
            let imported = r.ident.to_string();
            let renamed = map.get(&(source_bucket.to_string(), imported))?;
            Some(vec![(r.rename.to_string(), renamed.clone())])
        }
        syn::UseTree::Group(group) => {
            let mut out = Vec::new();
            for child in &group.items {
                out.extend(aliases_from_bucket_import_child(source_bucket, child, map)?);
            }
            (!out.is_empty()).then_some(out)
        }
        _ => None,
    }
}

fn simple_inline_import_removal(
    item_use: &syn::ItemUse,
    map: &RenameMap,
    _bucket: &str,
    item_start: usize,
    item_end: usize,
) -> Option<Replacement> {
    simple_inline_import_alias(&item_use.tree, map)?;
    Some(Replacement {
        start: item_start,
        end: item_end,
        text: String::new(),
    })
}

fn simple_parent_import_rewrite(
    src: &str,
    item_use: &syn::ItemUse,
    map: &RenameMap,
    item_start: usize,
    item_end: usize,
    use_start: usize,
) -> Option<Replacement> {
    let syn::UseTree::Path(super_path) = &item_use.tree else {
        return None;
    };
    if super_path.ident != "super" {
        return None;
    }
    match super_path.tree.as_ref() {
        syn::UseTree::Name(_) => Some(Replacement {
            start: item_start,
            end: item_end,
            text: String::new(),
        }),
        syn::UseTree::Glob(_) => Some(Replacement {
            start: item_start,
            end: item_end,
            text: String::new(),
        }),
        syn::UseTree::Rename(r) => Some(Replacement {
            start: item_start,
            end: item_end,
            text: format!(
                "{}use self::{} as {};",
                &src[item_start..use_start],
                r.ident,
                r.rename
            ),
        }),
        syn::UseTree::Path(p) => {
            if map.keys().any(|(bucket, _)| bucket == &p.ident.to_string()) {
                return None;
            }
            Some(Replacement {
                start: item_start,
                end: item_end,
                text: format!(
                    "{}use self::{};",
                    &src[item_start..use_start],
                    p.to_token_stream()
                ),
            })
        }
        syn::UseTree::Group(group) => {
            let mut kept = Vec::new();
            for child in &group.items {
                match child {
                    syn::UseTree::Name(_) | syn::UseTree::Glob(_) => {}
                    syn::UseTree::Rename(r) => {
                        kept.push(format!("{} as {}", r.ident, r.rename));
                    }
                    syn::UseTree::Path(p) => {
                        if map.keys().any(|(bucket, _)| bucket == &p.ident.to_string()) {
                            return None;
                        }
                        kept.push(p.to_token_stream().to_string());
                    }
                    syn::UseTree::Group(_) => return None,
                }
            }
            if kept.is_empty() {
                Some(Replacement {
                    start: item_start,
                    end: item_end,
                    text: String::new(),
                })
            } else {
                Some(Replacement {
                    start: item_start,
                    end: item_end,
                    text: format!(
                        "{}use self::{{{}}};",
                        &src[item_start..use_start],
                        kept.join(", ")
                    ),
                })
            }
        }
    }
}

fn collect_use_tree_replacements(
    src: &str,
    tree: &syn::UseTree,
    map: &RenameMap,
    current_bucket: Option<&str>,
    out: &mut Vec<Replacement>,
    warnings: &mut Vec<String>,
) {
    fn walk(
        src: &str,
        tree: &syn::UseTree,
        map: &RenameMap,
        current_bucket: Option<&str>,
        prefix: &mut Vec<syn::Ident>,
        out: &mut Vec<Replacement>,
        warnings: &mut Vec<String>,
    ) {
        match tree {
            syn::UseTree::Path(p) => {
                prefix.push(p.ident.clone());
                walk(src, &p.tree, map, current_bucket, prefix, out, warnings);
                prefix.pop();
            }
            syn::UseTree::Name(n) => {
                collect_use_name_or_rename(src, prefix, &n.ident, None, map, current_bucket, out);
            }
            syn::UseTree::Rename(r) => {
                collect_use_name_or_rename(
                    src,
                    prefix,
                    &r.ident,
                    Some(&r.rename),
                    map,
                    current_bucket,
                    out,
                );
            }
            syn::UseTree::Glob(g) => {
                if !prefix.is_empty() {
                    warnings.push(format!(
                        "left glob import `{}` unchanged; expand it before flattening for precise aliases",
                        use_prefix_to_string(prefix, Some(g.star_token.span))
                    ));
                }
            }
            syn::UseTree::Group(g) => {
                if !prefix.is_empty() {
                    warnings.push(format!(
                        "left grouped import under `{}` unchanged; expand it before flattening for precise aliases",
                        prefix.iter().map(ToString::to_string).collect::<Vec<_>>().join("::")
                    ));
                    return;
                }
                for child in &g.items {
                    walk(src, child, map, current_bucket, prefix, out, warnings);
                }
            }
        }
    }

    let mut prefix = Vec::new();
    walk(src, tree, map, current_bucket, &mut prefix, out, warnings);
}

fn collect_use_name_or_rename(
    src: &str,
    prefix: &[syn::Ident],
    ident: &syn::Ident,
    rename: Option<&syn::Ident>,
    map: &RenameMap,
    current_bucket: Option<&str>,
    out: &mut Vec<Replacement>,
) {
    if let Some(bucket) = current_bucket
        && prefix.len() == 2
        && prefix[0] == "super"
    {
        let imported_name = ident.to_string();
        let source_bucket = prefix[1].to_string();
        if let Some(new_name) = map.get(&(source_bucket, imported_name.clone())) {
            let Some(start) = span_start(src, prefix[0].span()) else {
                return;
            };
            let Some(end) = span_end(src, ident.span()) else {
                return;
            };
            let alias = rename.map(ToString::to_string).unwrap_or(imported_name);
            out.push(Replacement {
                start,
                end,
                text: format!("self::{new_name} as {alias}"),
            });
            return;
        }

        if let Some(new_name) = map.get(&(bucket.to_string(), imported_name.clone())) {
            let Some(start) = span_start(src, prefix[0].span()) else {
                return;
            };
            let Some(end) = span_end(src, ident.span()) else {
                return;
            };
            let alias = rename.map(ToString::to_string).unwrap_or(imported_name);
            out.push(Replacement {
                start,
                end,
                text: format!("self::{new_name} as {alias}"),
            });
            return;
        }
    }

    if prefix.len() == 1 {
        let bucket = prefix[0].to_string();
        let imported_name = ident.to_string();
        if let Some(new_name) = map.get(&(bucket, imported_name.clone())) {
            let Some(start) = span_start(src, prefix[0].span()) else {
                return;
            };
            let Some(end) = span_end(src, ident.span()) else {
                return;
            };
            let alias = rename.map(ToString::to_string).unwrap_or(imported_name);
            out.push(Replacement {
                start,
                end,
                text: format!("self::{new_name} as {alias}"),
            });
            return;
        }
    }

    let mut full: Vec<&syn::Ident> = prefix.iter().collect();
    full.push(ident);
    for pair in full.windows(2) {
        let bucket = pair[0].to_string();
        let name = pair[1].to_string();
        let Some(new_name) = map.get(&(bucket, name)) else {
            continue;
        };
        let Some(start) = span_start(src, pair[0].span()) else {
            continue;
        };
        let Some(end) = span_end(src, pair[1].span()) else {
            continue;
        };
        out.push(Replacement {
            start,
            end,
            text: new_name.clone(),
        });
    }
}

fn simple_glob_use_replacement(
    src: &str,
    item_use: &syn::ItemUse,
    map: &RenameMap,
    item_start: usize,
    item_end: usize,
    use_start: usize,
) -> Option<Replacement> {
    let syn::UseTree::Path(p) = &item_use.tree else {
        return None;
    };
    let syn::UseTree::Glob(_) = p.tree.as_ref() else {
        return None;
    };
    let bucket = p.ident.to_string();
    let names: Vec<String> = map
        .iter()
        .filter_map(|((b, name), renamed)| (b == &bucket).then_some(format!("{renamed} as {name}")))
        .collect();
    if names.is_empty() {
        return None;
    }
    let prefix = &src[item_start..use_start];
    Some(Replacement {
        start: item_start,
        end: item_end,
        text: format!("{prefix}use self::{{{}}};", names.join(", ")),
    })
}

fn simple_bucket_group_use_replacement(
    src: &str,
    item_use: &syn::ItemUse,
    map: &RenameMap,
    item_start: usize,
    item_end: usize,
    use_start: usize,
) -> Option<Replacement> {
    let syn::UseTree::Path(p) = &item_use.tree else {
        return None;
    };
    let syn::UseTree::Group(group) = p.tree.as_ref() else {
        return None;
    };
    let bucket = p.ident.to_string();
    let mut names = Vec::new();
    for child in &group.items {
        match child {
            syn::UseTree::Name(n) => {
                let original = n.ident.to_string();
                let renamed = map.get(&(bucket.clone(), original.clone()))?;
                names.push(format!("{renamed} as {original}"));
            }
            syn::UseTree::Rename(r) => {
                let original = r.ident.to_string();
                let renamed = map.get(&(bucket.clone(), original))?;
                names.push(format!("{renamed} as {}", r.rename));
            }
            _ => return None,
        }
    }
    if names.is_empty() {
        return None;
    }
    let prefix = &src[item_start..use_start];
    Some(Replacement {
        start: item_start,
        end: item_end,
        text: format!("{prefix}use self::{{{}}};", names.join(", ")),
    })
}

fn render_inline_mod_body(
    src: &str,
    m: &syn::ItemMod,
    content_start: usize,
    content_end: usize,
    replacements: &[Replacement],
    warnings: &mut Vec<String>,
) -> Result<String> {
    let cfg_attrs = cfg_attr_sources(src, m);
    let unsupported_attrs: Vec<String> = m
        .attrs
        .iter()
        .filter(|a| !a.path().is_ident("cfg"))
        .map(|a| a.path().to_token_stream().to_string())
        .collect();
    if !unsupported_attrs.is_empty() {
        warnings.push(format!(
            "dropped attributes on inline mod `{}`: {}",
            m.ident,
            unsupported_attrs.join(", ")
        ));
    }

    if cfg_attrs.is_empty() {
        let body = apply_replacements_to_slice(src, content_start, content_end, replacements);
        return Ok(dedent_inline_body(&body));
    }

    let Some((_, items)) = &m.content else {
        return Ok(String::new());
    };
    let mut out = String::new();
    for item in items {
        let (start, end) = item_byte_range(src, item)?;
        for attr in &cfg_attrs {
            out.push_str(attr);
            out.push('\n');
        }
        let rendered = apply_replacements_to_slice(src, start, end, replacements);
        out.push_str(rendered.trim_end());
        out.push_str("\n\n");
    }
    Ok(out)
}

fn dedent_inline_body(body: &str) -> String {
    let trimmed = body.trim_matches('\n');
    let mut out = String::new();
    for line in trimmed.lines() {
        let dedented = line
            .strip_prefix("    ")
            .or_else(|| line.strip_prefix('\t'))
            .unwrap_or(line);
        out.push_str(dedented);
        out.push('\n');
    }
    out
}

fn apply_replacements_to_slice(
    src: &str,
    start: usize,
    end: usize,
    replacements: &[Replacement],
) -> String {
    let mut relevant: Vec<&Replacement> = replacements
        .iter()
        .filter(|r| start <= r.start && r.end <= end && r.start <= r.end)
        .collect();
    relevant.sort_by_key(|r| {
        (
            r.start,
            std::cmp::Reverse(r.end),
            std::cmp::Reverse(replacement_priority(r)),
        )
    });

    let mut non_overlapping: Vec<&Replacement> = Vec::new();
    let mut covered_until = start;
    for r in relevant {
        if r.start < covered_until {
            continue;
        }
        covered_until = r.end;
        non_overlapping.push(r);
    }

    let mut out = String::with_capacity(end - start);
    let mut cursor = start;
    for r in non_overlapping {
        out.push_str(&src[cursor..r.start]);
        out.push_str(&r.text);
        cursor = r.end;
    }
    out.push_str(&src[cursor..end]);
    out
}

fn remove_duplicate_plain_uses(src: &str) -> Result<(String, usize)> {
    let file = syn::parse_file(src).context("parse flattened Rust source")?;
    let mut seen = BTreeMap::new();
    let mut replacements = Vec::new();

    for item in &file.items {
        let syn::Item::Use(item_use) = item else {
            continue;
        };
        if !item_use.attrs.is_empty() || !matches!(item_use.vis, syn::Visibility::Inherited) {
            continue;
        }
        let (start, end) = item_byte_range(src, item)?;
        let rendered = src[start..end].trim().to_string();
        if seen.insert(rendered, ()).is_some() {
            replacements.push(Replacement {
                start,
                end,
                text: String::new(),
            });
        }
    }

    let rewrite_count = replacements.len();
    Ok((
        apply_replacements_to_slice(src, 0, src.len(), &replacements),
        rewrite_count,
    ))
}

fn replacement_priority(r: &Replacement) -> u8 {
    if r.text.contains(" as ") || r.text.starts_with("self::") {
        2
    } else {
        1
    }
}

fn inline_content_ranges(
    src: &str,
    mods: &[&syn::ItemMod],
) -> Result<BTreeMap<String, (usize, usize)>> {
    let mut out = BTreeMap::new();
    for m in mods {
        let (start, end) = item_byte_range_for_mod_contents(src, m)?;
        out.insert(m.ident.to_string(), (start, end));
    }
    Ok(out)
}

fn item_byte_range_for_mod_contents(src: &str, m: &syn::ItemMod) -> Result<(usize, usize)> {
    let (item_start, item_end) = item_mod_byte_range(src, m)?;
    let item_src = &src[item_start..item_end];
    let open = item_src
        .find('{')
        .with_context(|| format!("inline mod `{}` has no opening brace", m.ident))?;
    let close = item_src
        .rfind('}')
        .with_context(|| format!("inline mod `{}` has no closing brace", m.ident))?;
    Ok((item_start + open + 1, item_start + close))
}

fn item_mod_byte_range(src: &str, m: &syn::ItemMod) -> Result<(usize, usize)> {
    let start_span = m
        .attrs
        .iter()
        .map(|a| a.span())
        .next()
        .unwrap_or_else(|| m.span());
    let start = span_start(src, start_span).context("mod start span unavailable")?;
    let end = span_end(src, m.span()).context("mod end span unavailable")?;
    Ok(expand_to_line_bounds(src, start, end))
}

fn item_byte_range(src: &str, item: &syn::Item) -> Result<(usize, usize)> {
    let start_span = item_attrs(item)
        .iter()
        .map(|a| a.span())
        .next()
        .unwrap_or_else(|| item.span());
    let start = span_start(src, start_span).context("item start span unavailable")?;
    let end = span_end(src, item.span()).context("item end span unavailable")?;
    Ok(expand_to_line_bounds(src, start, end))
}

fn expand_to_line_bounds(src: &str, start: usize, end: usize) -> (usize, usize) {
    let line_start = src[..start].rfind('\n').map_or(0, |idx| idx + 1);
    let line_end = src[end..].find('\n').map_or(src.len(), |idx| end + idx);
    (line_start, line_end)
}

fn named_item(item: &syn::Item) -> Option<(String, Span)> {
    match item {
        syn::Item::Const(i) => Some((i.ident.to_string(), i.ident.span())),
        syn::Item::Enum(i) => Some((i.ident.to_string(), i.ident.span())),
        syn::Item::Fn(i) => Some((i.sig.ident.to_string(), i.sig.ident.span())),
        syn::Item::Macro(i) => i
            .ident
            .as_ref()
            .map(|ident| (ident.to_string(), ident.span())),
        syn::Item::Static(i) => Some((i.ident.to_string(), i.ident.span())),
        syn::Item::Struct(i) => Some((i.ident.to_string(), i.ident.span())),
        syn::Item::Trait(i) => Some((i.ident.to_string(), i.ident.span())),
        syn::Item::TraitAlias(i) => Some((i.ident.to_string(), i.ident.span())),
        syn::Item::Type(i) => Some((i.ident.to_string(), i.ident.span())),
        syn::Item::Union(i) => Some((i.ident.to_string(), i.ident.span())),
        _ => None,
    }
}

fn item_attrs(item: &syn::Item) -> &[syn::Attribute] {
    match item {
        syn::Item::Const(i) => &i.attrs,
        syn::Item::Enum(i) => &i.attrs,
        syn::Item::ExternCrate(i) => &i.attrs,
        syn::Item::Fn(i) => &i.attrs,
        syn::Item::ForeignMod(i) => &i.attrs,
        syn::Item::Impl(i) => &i.attrs,
        syn::Item::Macro(i) => &i.attrs,
        syn::Item::Mod(i) => &i.attrs,
        syn::Item::Static(i) => &i.attrs,
        syn::Item::Struct(i) => &i.attrs,
        syn::Item::Trait(i) => &i.attrs,
        syn::Item::TraitAlias(i) => &i.attrs,
        syn::Item::Type(i) => &i.attrs,
        syn::Item::Union(i) => &i.attrs,
        syn::Item::Use(i) => &i.attrs,
        syn::Item::Verbatim(_) => &[],
        _ => &[],
    }
}

fn cfg_attr_sources(src: &str, m: &syn::ItemMod) -> Vec<String> {
    m.attrs
        .iter()
        .filter(|a| a.path().is_ident("cfg"))
        .filter_map(|a| span_range(src, a.span()).map(|(s, e)| src[s..e].to_string()))
        .collect()
}

fn inner_attrs_source(src: &str, file: &syn::File) -> String {
    let inner: Vec<&syn::Attribute> = file
        .attrs
        .iter()
        .filter(|a| matches!(a.style, syn::AttrStyle::Inner(_)))
        .collect();
    if inner.is_empty() {
        return String::new();
    }
    let end = inner
        .iter()
        .filter_map(|a| span_end(src, a.span()))
        .max()
        .unwrap_or(0);
    src[..end].to_string()
}

fn span_range(src: &str, span: Span) -> Option<(usize, usize)> {
    Some((span_start(src, span)?, span_end(src, span)?))
}

fn out_replacement(out: &mut Vec<Replacement>, start: usize, end: usize, text: &str) {
    out.push(Replacement {
        start,
        end,
        text: text.to_string(),
    });
}

fn span_start(src: &str, span: Span) -> Option<usize> {
    let start = span.start();
    line_col_to_byte_offset(src, start.line, start.column)
}

fn span_end(src: &str, span: Span) -> Option<usize> {
    let end = span.end();
    line_col_to_byte_offset(src, end.line, end.column)
}

fn use_prefix_to_string(prefix: &[syn::Ident], _star: Option<Span>) -> String {
    format!(
        "{}::*",
        prefix
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("::")
    )
}
