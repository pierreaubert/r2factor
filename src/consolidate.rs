//! Consolidate a module spread across `facade.rs + sub_dir/` (the r2factor
//! split shape, but also any hand-written equivalent) into a single
//! `.rs` file using **inline modules** — each sub-file becomes a
//! `mod <name> { ... }` block inside the merged facade.
//!
//! Why inline mods instead of flattening to a single scope:
//!
//! * **No name collisions.** Two sub-files can each define `fn helper()`
//!   without conflicting. A flatten-style merge breaks (E0428) the moment
//!   two siblings share a fn name — which is exactly the pattern that
//!   modules exist to avoid in the first place.
//! * **External paths preserved.** Code outside the module that wrote
//!   `crate::foo::bar::thing` keeps working — `bar` is still a sub-module,
//!   just inlined into `foo.rs` rather than living in `foo/bar.rs`.
//! * **No reverse rewrites needed.** Module depth is unchanged, so
//!   `super::X`, `pub(super)`, `pub(in super::super)`, and cross-bucket
//!   `use super::other::name` all retain their original meaning.
//!
//! Strategy:
//!   1. Discover layout (facade + sub-dir + merge target + bucket names).
//!   2. For each sub-file, build an inline `mod <name> { ... }` block by
//!      reading the file, stripping the auto-gen banner if present, and
//!      indenting the body by 4 spaces.
//!   3. Walk the facade's items in order. Each `mod <bkt>;` is replaced
//!      verbatim with the corresponding inline block, **preserving any
//!      leading attrs/vis** (`#[macro_use] mod macros;` →
//!      `#[macro_use] mod macros { ... }`). Other facade items pass
//!      through unchanged.
//!   4. Backup the facade, write the merged content, clean up the sub-dir
//!      conservatively (only remove the `.rs` files we consumed; leave
//!      anything else — nested dirs, README assets — alone).

use anyhow::{Context, Result, anyhow, bail};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use crate::promote::line_col_to_byte_offset;

/// Where the consolidator decided the input file/dir lives in the module
/// tree, and where the merged output should land.
#[derive(Debug, Clone)]
pub struct Layout {
    /// The current facade file: either `foo.rs` (alongside a `foo/` dir) or
    /// `foo/mod.rs`.
    pub facade: PathBuf,
    /// Directory containing the sub-bucket `.rs` files.
    pub sub_dir: PathBuf,
    /// Where the merged single-file output is written.
    /// * `foo.rs + foo/` → same as `facade`.
    /// * `foo/mod.rs`    → `<parent_of_foo>/foo.rs` (one level up).
    pub merged_target: PathBuf,
    /// Bucket file names (stems, no `.rs`) that we wrap as inline mods.
    pub bucket_names: BTreeSet<String>,
}

/// What [`consolidate_write`] produced.
#[derive(Debug, serde::Serialize)]
pub struct ConsolidateReport {
    pub merged_target: PathBuf,
    pub backup: Option<PathBuf>,
    pub removed_files: Vec<PathBuf>,
    pub source_bytes: usize,
}

pub struct ConsolidateOptions {
    /// Replace the existing files. Without this we just return the merged
    /// source (for the CLI dry-run / MCP preview).
    pub write: bool,
}

/// Public API: produce the merged source for `input_path`. Used by the
/// CLI dry-run and the `consolidate_dry_run` MCP tool.
pub fn consolidate_dry_run(input_path: &Path) -> Result<String> {
    let layout = discover_layout(input_path)?;
    merge_layout(&layout)
}

/// Public API: produce the merged source AND write it to disk, backing up
/// the previous facade and pruning consumed `.rs` files from the sub-dir.
pub fn consolidate_write(
    input_path: &Path,
    opts: &ConsolidateOptions,
) -> Result<ConsolidateReport> {
    let layout = discover_layout(input_path)?;
    let merged = merge_layout(&layout)?;
    let merged_bytes = merged.len();

    if !opts.write {
        return Ok(ConsolidateReport {
            merged_target: layout.merged_target.clone(),
            backup: None,
            removed_files: Vec::new(),
            source_bytes: merged_bytes,
        });
    }

    // Pick a backup destination that won't be inside the sub-dir we're
    // about to prune (the `foo/mod.rs` case used to drop the backup
    // inside `foo/` and then delete it on cleanup — data loss).
    let backup_target = if layout.facade.starts_with(&layout.sub_dir) {
        // facade is `foo/mod.rs`; back up next to where the merged file lands.
        let mut p = layout.merged_target.clone();
        if let Some(name) = layout.merged_target.file_name().and_then(|s| s.to_str()) {
            p.set_file_name(format!("{name}.bak"));
        }
        p
    } else {
        let mut p = layout.facade.clone();
        if let Some(name) = layout.facade.file_name().and_then(|s| s.to_str()) {
            p.set_file_name(format!("{name}.bak"));
        }
        p
    };
    let backup = if layout.facade.exists() {
        fs::copy(&layout.facade, &backup_target).with_context(|| {
            format!(
                "backup {} -> {}",
                layout.facade.display(),
                backup_target.display()
            )
        })?;
        Some(backup_target)
    } else {
        None
    };

    fs::write(&layout.merged_target, &merged)
        .with_context(|| format!("write merged {}", layout.merged_target.display()))?;

    // For the `mod.rs` case, remove the old facade file (it's about to
    // be inside the doomed sub-dir anyway, but the merge target lives
    // elsewhere so we delete it explicitly here too).
    if layout.facade != layout.merged_target && layout.facade.exists() {
        fs::remove_file(&layout.facade).with_context(|| {
            format!("remove old facade {}", layout.facade.display())
        })?;
    }

    let removed_files = prune_consumed_rs(&layout)?;

    Ok(ConsolidateReport {
        merged_target: layout.merged_target,
        backup,
        removed_files,
        source_bytes: merged_bytes,
    })
}

// ---------------------------------------------------------------------------
// Layout discovery
// ---------------------------------------------------------------------------

fn discover_layout(input_path: &Path) -> Result<Layout> {
    let meta = fs::metadata(input_path)
        .with_context(|| format!("stat {}", input_path.display()))?;
    let (facade, sub_dir, merged_target) = if meta.is_file() {
        resolve_from_file(input_path)?
    } else if meta.is_dir() {
        resolve_from_dir(input_path)?
    } else {
        bail!("not a regular file or directory: {}", input_path.display());
    };

    let bucket_names = collect_bucket_names(&sub_dir)?;
    if bucket_names.is_empty() {
        bail!(
            "sub-directory {} has no .rs sub-files — nothing to merge.",
            sub_dir.display()
        );
    }
    Ok(Layout {
        facade,
        sub_dir,
        merged_target,
        bucket_names,
    })
}

fn resolve_from_file(path: &Path) -> Result<(PathBuf, PathBuf, PathBuf)> {
    let name = path
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| anyhow!("unreadable filename: {}", path.display()))?;
    if name == "mod.rs" {
        let foo_dir = path
            .parent()
            .ok_or_else(|| anyhow!("`mod.rs` has no parent dir"))?;
        let parent_of_foo = foo_dir.parent().ok_or_else(|| {
            anyhow!("`mod.rs` parent has no parent (consolidating a crate root?)")
        })?;
        let foo_name = foo_dir
            .file_name()
            .and_then(|s| s.to_str())
            .ok_or_else(|| anyhow!("unreadable module dir name"))?;
        let merged = parent_of_foo.join(format!("{foo_name}.rs"));
        Ok((path.to_path_buf(), foo_dir.to_path_buf(), merged))
    } else if name.ends_with(".rs") {
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .ok_or_else(|| anyhow!("unreadable file stem"))?;
        let parent = path.parent().unwrap_or(Path::new("."));
        let sub_dir = parent.join(stem);
        if !sub_dir.is_dir() {
            bail!(
                "no sibling `{}/` directory next to {}; nothing to consolidate.",
                stem,
                path.display()
            );
        }
        Ok((path.to_path_buf(), sub_dir, path.to_path_buf()))
    } else {
        bail!("not a .rs file: {}", path.display());
    }
}

fn resolve_from_dir(path: &Path) -> Result<(PathBuf, PathBuf, PathBuf)> {
    let mod_rs = path.join("mod.rs");
    if mod_rs.is_file() {
        let parent_of_foo = path.parent().ok_or_else(|| {
            anyhow!("module dir has no parent (consolidating a crate root?)")
        })?;
        let foo_name = path
            .file_name()
            .and_then(|s| s.to_str())
            .ok_or_else(|| anyhow!("unreadable module dir name"))?;
        let merged = parent_of_foo.join(format!("{foo_name}.rs"));
        Ok((mod_rs, path.to_path_buf(), merged))
    } else {
        let foo_name = path
            .file_name()
            .and_then(|s| s.to_str())
            .ok_or_else(|| anyhow!("unreadable module dir name"))?;
        let parent = path
            .parent()
            .ok_or_else(|| anyhow!("module dir has no parent"))?;
        let facade = parent.join(format!("{foo_name}.rs"));
        if !facade.is_file() {
            bail!(
                "directory {} has no `mod.rs` and no sibling `{}.rs` to use as facade.",
                path.display(),
                foo_name
            );
        }
        Ok((facade.clone(), path.to_path_buf(), facade))
    }
}

fn collect_bucket_names(sub_dir: &Path) -> Result<BTreeSet<String>> {
    let mut names = BTreeSet::new();
    for entry in fs::read_dir(sub_dir)
        .with_context(|| format!("read_dir {}", sub_dir.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        if name == "mod.rs" {
            continue; // facade
        }
        if let Some(stem) = name.strip_suffix(".rs") {
            names.insert(stem.to_string());
        }
    }
    Ok(names)
}

// ---------------------------------------------------------------------------
// Merge
// ---------------------------------------------------------------------------

fn merge_layout(layout: &Layout) -> Result<String> {
    let facade_src = fs::read_to_string(&layout.facade)
        .with_context(|| format!("read facade {}", layout.facade.display()))?;
    let facade_file = syn::parse_file(&facade_src)
        .with_context(|| format!("parse facade {}", layout.facade.display()))?;
    let inner_attrs = facade_inner_attrs_source(&facade_src, &facade_file);

    // Build one inline `mod <name> { ... }` block per sub-file. Each block
    // already includes the `mod <name> {` opener, indented body, and `}`.
    let mut inline_blocks: std::collections::BTreeMap<String, String> =
        std::collections::BTreeMap::new();
    for bucket in &layout.bucket_names {
        let path = layout.sub_dir.join(format!("{bucket}.rs"));
        let body = fs::read_to_string(&path)
            .with_context(|| format!("read sub-file {}", path.display()))?;
        inline_blocks.insert(bucket.clone(), build_inline_block(bucket, &body));
    }

    let mut out = String::new();
    if !inner_attrs.is_empty() {
        out.push_str(inner_attrs.trim_end());
        out.push_str("\n\n");
    }

    // Walk facade items in their original order. Replace each `mod <bkt>;`
    // (with its attrs+vis) by the corresponding inline block; pass other
    // items through verbatim.
    for item in &facade_file.items {
        if let syn::Item::Mod(m) = item
            && m.content.is_none()
            && let Some(block) = inline_blocks.remove(&m.ident.to_string())
        {
            let prefix = mod_attr_and_vis_prefix(&facade_src, m);
            if !prefix.is_empty() {
                out.push_str(&prefix);
                if !prefix.ends_with('\n') && !prefix.ends_with(' ') {
                    out.push(' ');
                }
            }
            out.push_str(&block);
            out.push_str("\n\n");
            continue;
        }
        let src = slice_item_source(&facade_src, item);
        if !src.is_empty() {
            out.push_str(&src);
            out.push_str("\n\n");
        }
    }

    // Any sub-files left over (no matching `mod <name>;` in the facade)
    // get appended as their own inline mods so they're not silently lost.
    for (name, block) in inline_blocks {
        eprintln!(
            "[consolidate] note: {name}.rs has no `mod {name};` in facade — appending"
        );
        out.push_str(&block);
        out.push_str("\n\n");
    }
    Ok(out)
}

/// Build `mod <name> { ... }` from a sub-file's verbatim source. Strips
/// the r2factor auto-gen banner if it leads the file, then indents every
/// remaining non-empty line by 4 spaces.
fn build_inline_block(name: &str, body: &str) -> String {
    let cleaned = strip_autogen_banner(body);
    let trimmed = cleaned.trim();
    let mut out = String::with_capacity(trimmed.len() + 32);
    out.push_str("mod ");
    out.push_str(name);
    out.push_str(" {\n");
    for line in trimmed.lines() {
        if line.is_empty() {
            out.push('\n');
        } else {
            out.push_str("    ");
            out.push_str(line);
            out.push('\n');
        }
    }
    out.push('}');
    out
}

/// Drop the auto-generated leading comment(s) we emit on every sub-file
/// (and the facade-marker line) so they don't survive the merge. Plain
/// non-r2factor sub-files have nothing to strip and pass through.
fn strip_autogen_banner(src: &str) -> String {
    let mut lines: Vec<&str> = src.lines().collect();
    while let Some(first) = lines.first() {
        let t = first.trim_start();
        if t.starts_with("// Auto-generated by r2factor")
            || t.starts_with("// r2factor:facade")
            || t.is_empty()
        {
            lines.remove(0);
        } else {
            break;
        }
    }
    lines.join("\n")
}

/// Return the bytes of a `mod foo;` item that precede the `mod` keyword —
/// i.e. attributes (`#[cfg(test)]`, `#[macro_use]`, …) and visibility
/// (`pub`, `pub(crate)`). We splice this prefix in front of the inline
/// block so `#[cfg(test)] mod tests;` becomes `#[cfg(test)] mod tests { ... }`.
fn mod_attr_and_vis_prefix(src: &str, m: &syn::ItemMod) -> String {
    use syn::spanned::Spanned;
    let item_start = m.attrs.iter().map(|a| a.span().start()).min().unwrap_or_else(|| {
        // No attrs — start at the vis span (or mod_token if vis is inherited).
        match &m.vis {
            syn::Visibility::Public(t) => t.span.start(),
            syn::Visibility::Restricted(r) => r.pub_token.span.start(),
            syn::Visibility::Inherited => m.mod_token.span.start(),
        }
    });
    let mod_start = m.mod_token.span.start();
    let Some(s) = line_col_to_byte_offset(src, item_start.line, item_start.column) else {
        return String::new();
    };
    let Some(e) = line_col_to_byte_offset(src, mod_start.line, mod_start.column) else {
        return String::new();
    };
    if s >= e {
        return String::new();
    }
    src[s..e].trim_end().to_string()
}

fn slice_item_source(src: &str, item: &syn::Item) -> String {
    use syn::spanned::Spanned;
    let span = item.span();
    let lines: Vec<&str> = src.lines().collect();
    let start = span.start().line.saturating_sub(1);
    let end = span.end().line.min(lines.len());
    if start >= lines.len() {
        return String::new();
    }
    lines[start..end].join("\n")
}

fn facade_inner_attrs_source(src: &str, file: &syn::File) -> String {
    use syn::spanned::Spanned;
    let inner: Vec<&syn::Attribute> = file
        .attrs
        .iter()
        .filter(|a| matches!(a.style, syn::AttrStyle::Inner(_)))
        .collect();
    if inner.is_empty() {
        return String::new();
    }
    let end_line = inner
        .iter()
        .map(|a| a.span().end().line)
        .max()
        .unwrap_or(0);
    src.lines()
        .take(end_line)
        .collect::<Vec<_>>()
        .join("\n")
}

// ---------------------------------------------------------------------------
// Cleanup
// ---------------------------------------------------------------------------

/// Remove only the `.rs` files in `sub_dir` that we just inlined; leave
/// any nested directories, non-`.rs` files, and the dir itself alone if
/// anything remains. Conservative on purpose — we don't want to eat
/// user-curated assets or deeper module trees.
fn prune_consumed_rs(layout: &Layout) -> Result<Vec<PathBuf>> {
    if !layout.sub_dir.exists() {
        return Ok(Vec::new());
    }
    let mut removed = Vec::new();
    for entry in fs::read_dir(&layout.sub_dir)
        .with_context(|| format!("read_dir {}", layout.sub_dir.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        let is_consumed_rs = if name == "mod.rs" {
            // Consumed if we used it as the facade.
            path == layout.facade
        } else if let Some(stem) = name.strip_suffix(".rs") {
            layout.bucket_names.contains(stem)
        } else {
            false
        };
        if is_consumed_rs {
            fs::remove_file(&path)
                .with_context(|| format!("remove {}", path.display()))?;
            removed.push(path);
        }
    }
    // Try to remove the dir; ignore failure (means it still has nested
    // dirs or other files we deliberately left).
    let _ = fs::remove_dir(&layout.sub_dir);
    Ok(removed)
}
