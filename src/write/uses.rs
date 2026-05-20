//! Build a minimal per-bucket `use` prelude. The original implementation
//! dumped every `use` from the source file into every sub-file, which
//! caused dead-import warnings on every build. Here we look at the names
//! each `use` introduces into scope and keep only those a given bucket
//! actually references.
//!
//! This module also rebases relative `use` paths for sub-files. Sub-files
//! live one level deeper than the original (parent/child.rs becomes
//! parent/child/<bkt>.rs), so a `use super::foo;` that worked in the
//! original needs to become `use super::super::foo;` from a sub-file —
//! `super` from the deeper module points at the facade, not at the parent.

use crate::item::{ItemId, ItemKind, ParsedItem};
use crate::promote::line_col_to_byte_offset;
use quote::ToTokens;
use std::collections::{BTreeMap, HashSet};
use syn::UseTree;
use syn::visit::{self, Visit};

/// Sentinel used in the binding set of a `use foo::*` to mean "we can't
/// enumerate what this brings in" — buckets that match anything via this
/// keep the glob unconditionally.
const GLOB: &str = "*";

/// Names a `use` item brings into the local scope.
///
/// `use foo::Bar;`         -> ["Bar"]
/// `use foo::{Bar, Baz};`  -> ["Bar", "Baz"]
/// `use foo::Bar as Qux;`  -> ["Qux"]
/// `use foo::*;`           -> ["*"]   (sentinel; treat as wildcard match)
pub fn use_bindings(use_item: &ParsedItem) -> Vec<String> {
    debug_assert!(matches!(use_item.kind, ItemKind::Use));
    let parsed: syn::ItemUse = match syn::parse_str(&use_item.source) {
        Ok(p) => p,
        Err(_) => return Vec::new(),
    };
    let mut out = Vec::new();
    collect_tree(&parsed.tree, &mut out);
    out
}

fn collect_tree(tree: &UseTree, out: &mut Vec<String>) {
    match tree {
        UseTree::Path(p) => collect_tree(&p.tree, out),
        UseTree::Name(n) => out.push(n.ident.to_string()),
        UseTree::Rename(r) => out.push(r.rename.to_string()),
        UseTree::Group(g) => {
            for t in &g.items {
                collect_tree(t, out);
            }
        }
        UseTree::Glob(_) => out.push(GLOB.to_string()),
    }
}

/// Every identifier referenced anywhere inside a chunk of Rust source.
/// Parses the chunk as a `syn::File`; on parse failure (which shouldn't
/// happen since the source came from `parse_file` originally) we err on
/// the side of "keep everything" by returning a wildcard set.
pub fn bucket_idents(bucket_source: &str) -> Option<HashSet<String>> {
    let file = syn::parse_file(bucket_source).ok()?;
    let mut v = IdentCollector {
        found: HashSet::new(),
    };
    visit::visit_file(&mut v, &file);
    Some(v.found)
}

struct IdentCollector {
    found: HashSet<String>,
}

impl<'ast> Visit<'ast> for IdentCollector {
    fn visit_ident(&mut self, ident: &'ast proc_macro2::Ident) {
        self.found.insert(ident.to_string());
    }
}

/// Concatenate the source of every non-`use` item in `bucket_ids` and run
/// the ident collector over it. Returns `None` on syn parse failure, which
/// the caller should treat as "fall back to the global prelude".
pub fn bucket_idents_for(
    bucket_ids: &[ItemId],
    by_id: &BTreeMap<ItemId, &ParsedItem>,
) -> Option<HashSet<String>> {
    let mut buf = String::new();
    for id in bucket_ids {
        let it = by_id[id];
        if matches!(it.kind, ItemKind::Use) {
            continue;
        }
        buf.push_str(&it.source);
        buf.push('\n');
    }
    bucket_idents(&buf)
}

/// Render only the bindings from `all_uses` that are actually referenced by
/// `idents`. This also prunes inside grouped imports
/// (`use foo::{A, B};` -> `use foo::A;` when only `A` is used).
pub fn render_uses_for(
    all_uses: &[&ParsedItem],
    idents: &HashSet<String>,
    known_names: &HashSet<String>,
    rebase: bool,
) -> String {
    let mut provided = known_names.clone();
    for builtin in [
        "Self", "Ok", "Err", "Some", "None", "Option", "Result", "Vec", "Box", "String", "Default",
    ] {
        provided.insert(builtin.to_string());
    }
    for u in all_uses {
        for binding in use_bindings(u) {
            if binding != GLOB {
                provided.insert(binding);
            }
        }
    }
    let keep_globs = idents
        .iter()
        .any(|ident| is_type_like_ident(ident) && !provided.contains(ident));

    all_uses
        .iter()
        .filter_map(|u| filter_use_for(u, idents, keep_globs))
        .map(|src| {
            if rebase {
                rebase_use_for_subfile(&src)
            } else {
                src
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn filter_use_for(
    use_item: &ParsedItem,
    idents: &HashSet<String>,
    keep_globs: bool,
) -> Option<String> {
    let bindings = use_bindings(use_item);
    if !bindings.is_empty()
        && bindings
            .iter()
            .all(|name| name == GLOB && keep_globs || binding_is_used(name, idents))
    {
        return Some(use_item.source.clone());
    }
    let mut parsed: syn::ItemUse = syn::parse_str(&use_item.source).ok()?;
    parsed.tree = filter_tree(parsed.tree, idents, keep_globs)?;
    let mut rendered = parsed.to_token_stream().to_string();
    if !rendered.ends_with(';') {
        rendered.push(';');
    }
    Some(rendered)
}

fn filter_tree(tree: UseTree, idents: &HashSet<String>, keep_globs: bool) -> Option<UseTree> {
    match tree {
        UseTree::Path(mut p) => {
            p.tree = Box::new(filter_tree(*p.tree, idents, keep_globs)?);
            Some(UseTree::Path(p))
        }
        UseTree::Name(n) => {
            binding_is_used(&n.ident.to_string(), idents).then_some(UseTree::Name(n))
        }
        UseTree::Rename(r) => idents
            .contains(&r.rename.to_string())
            .then_some(UseTree::Rename(r)),
        UseTree::Group(mut g) => {
            g.items = g
                .items
                .into_iter()
                .filter_map(|tree| filter_tree(tree, idents, keep_globs))
                .collect();
            (!g.items.is_empty()).then_some(UseTree::Group(g))
        }
        UseTree::Glob(g) => keep_globs.then_some(UseTree::Glob(g)),
    }
}

fn is_type_like_ident(ident: &str) -> bool {
    ident
        .chars()
        .next()
        .is_some_and(|c| c == '_' || c.is_ascii_uppercase())
}

fn binding_is_used(binding: &str, idents: &HashSet<String>) -> bool {
    idents.contains(binding) || trait_method_hint_is_used(binding, idents)
}

fn trait_method_hint_is_used(binding: &str, idents: &HashSet<String>) -> bool {
    match binding {
        "AsGenericAudioBufferRef" => idents.contains("as_generic_audio_buffer_ref"),
        "AudioMut" => idents.contains("plane_mut") || idents.contains("plane_pair_mut"),
        "MediaSource" => idents.contains("is_seekable"),
        "ReadBytes" => {
            idents.contains("pos")
                || idents.contains("ignore_bytes")
                || idents.contains("read_byte")
                || idents.contains("read_i16")
                || idents.contains("read_u16")
                || idents.contains("read_u32")
                || idents.contains("read_quad_bytes")
        }
        "Seek" => idents.contains("seek"),
        _ => false,
    }
}

/// Rewrite a single `use` item so it works from inside a sub-file. Sub-files
/// sit one module-level deeper than the original, so paths anchored at
/// `super` or `self` need to be pushed one level up:
///
/// * `use super::foo;`  ->  `use super::super::foo;`
/// * `use self::foo;`   ->  `use super::foo;`
///
/// `crate::…`, leading-`::` absolute paths, and external-crate paths are
/// already absolute (or resolved via the extern-crate table) so they stay
/// put. The rewrite is byte-level so attribute formatting, trailing
/// comments, and unusual whitespace survive verbatim.
pub fn rebase_use_for_subfile(use_src: &str) -> String {
    let Ok(item) = syn::parse_str::<syn::ItemUse>(use_src) else {
        return use_src.to_string();
    };
    let inner_tree = match &item.tree {
        UseTree::Path(p) => p,
        _ => return use_src.to_string(),
    };
    let ident_str = inner_tree.ident.to_string();
    let action = match ident_str.as_str() {
        "super" => Action::WrapSuper,
        "self" => Action::ReplaceSelfWithSuper,
        _ => return use_src.to_string(),
    };
    let span = inner_tree.ident.span();
    let start = span.start();
    let Some(pos) = line_col_to_byte_offset(use_src, start.line, start.column) else {
        return use_src.to_string();
    };
    match action {
        Action::WrapSuper => {
            // Insert `super::` immediately before the existing `super`.
            let mut out = String::with_capacity(use_src.len() + "super::".len());
            out.push_str(&use_src[..pos]);
            out.push_str("super::");
            out.push_str(&use_src[pos..]);
            out
        }
        Action::ReplaceSelfWithSuper => {
            // Replace exactly the 4 bytes of "self" with "super".
            let end = pos + "self".len();
            if end > use_src.len() || &use_src[pos..end] != "self" {
                return use_src.to_string();
            }
            let mut out = String::with_capacity(use_src.len() + 1);
            out.push_str(&use_src[..pos]);
            out.push_str("super");
            out.push_str(&use_src[end..]);
            out
        }
    }
}

enum Action {
    WrapSuper,
    ReplaceSelfWithSuper,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rebase_super_path() {
        assert_eq!(
            rebase_use_for_subfile("use super::foo::Bar;"),
            "use super::super::foo::Bar;"
        );
    }

    #[test]
    fn rebase_super_group() {
        // The group binding doesn't change — only the leading `super` does.
        assert_eq!(
            rebase_use_for_subfile("use super::{a, b};"),
            "use super::super::{a, b};"
        );
    }

    #[test]
    fn rebase_super_with_rename() {
        assert_eq!(
            rebase_use_for_subfile("use super::Foo as Bar;"),
            "use super::super::Foo as Bar;"
        );
    }

    #[test]
    fn rebase_self_path() {
        // `self::X` in the original = `child::X` (current module). After
        // split, `child` is the facade, which from a sub-file's POV is
        // `super`. So `self::X` becomes `super::X`.
        assert_eq!(
            rebase_use_for_subfile("use self::foo::Bar;"),
            "use super::foo::Bar;"
        );
    }

    #[test]
    fn rebase_crate_path_unchanged() {
        assert_eq!(
            rebase_use_for_subfile("use crate::foo::Bar;"),
            "use crate::foo::Bar;"
        );
    }

    #[test]
    fn rebase_extern_crate_path_unchanged() {
        assert_eq!(
            rebase_use_for_subfile("use std::collections::HashMap;"),
            "use std::collections::HashMap;"
        );
    }

    #[test]
    fn rebase_pub_use_super() {
        // Visibility annotation comes before the `use` keyword; rebase
        // should still find `super` and wrap.
        assert_eq!(
            rebase_use_for_subfile("pub use super::foo::Bar;"),
            "pub use super::super::foo::Bar;"
        );
    }

    #[test]
    fn rebase_invalid_input_passes_through() {
        // If syn can't parse, return verbatim — better to ship the
        // original line than a corrupted one.
        assert_eq!(rebase_use_for_subfile("not a use stmt"), "not a use stmt");
    }
}
