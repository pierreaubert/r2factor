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

    fn visit_attribute(&mut self, attr: &'ast syn::Attribute) {
        scan_attr_tokens(&attr.meta.to_token_stream(), self);
    }

    fn visit_macro(&mut self, mac: &'ast syn::Macro) {
        visit::visit_path(self, &mac.path);
        scan_macro_tokens(&mac.tokens, self);
    }
}

fn scan_macro_tokens(stream: &proc_macro2::TokenStream, v: &mut IdentCollector) {
    use proc_macro2::TokenTree;
    for tt in stream.clone() {
        match tt {
            TokenTree::Ident(id) => v.visit_ident(&id),
            TokenTree::Group(g) => scan_macro_tokens(&g.stream(), v),
            _ => {}
        }
    }
}

fn scan_attr_tokens(stream: &proc_macro2::TokenStream, v: &mut IdentCollector) {
    use proc_macro2::TokenTree;
    for tt in stream.clone() {
        match tt {
            TokenTree::Ident(id) => v.visit_ident(&id),
            TokenTree::Group(g) => scan_attr_tokens(&g.stream(), v),
            TokenTree::Literal(lit) => scan_attr_string_path_literal(&lit, v),
            _ => {}
        }
    }
}

fn scan_attr_string_path_literal(lit: &proc_macro2::Literal, v: &mut IdentCollector) {
    let Ok(lit) = syn::parse_str::<syn::LitStr>(&lit.to_string()) else {
        return;
    };
    let Ok(path) = syn::parse_str::<syn::Path>(&lit.value()) else {
        return;
    };
    for segment in path.segments {
        v.found.insert(segment.ident.to_string());
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
#[cfg(test)]
pub fn render_uses_for(
    all_uses: &[&ParsedItem],
    idents: &HashSet<String>,
    known_names: &HashSet<String>,
    rebase: bool,
) -> String {
    render_uses_for_with_local_roots(all_uses, idents, known_names, rebase, &HashSet::new())
}

pub fn render_uses_for_with_local_roots(
    all_uses: &[&ParsedItem],
    idents: &HashSet<String>,
    known_names: &HashSet<String>,
    rebase: bool,
    local_module_roots: &HashSet<String>,
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
                rebase_use_for_subfile_with_local_roots(&src, local_module_roots)
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
        UseTree::Rename(r) => {
            let used = if r.rename == "_" {
                binding_is_used(&r.ident.to_string(), idents)
            } else {
                idents.contains(&r.rename.to_string())
            };
            used.then_some(UseTree::Rename(r))
        }
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
        "DeviceTrait" => {
            idents.contains("build_input_stream")
                || idents.contains("build_output_stream")
                || idents.contains("default_input_config")
                || idents.contains("default_output_config")
                || idents.contains("description")
                || idents.contains("id")
                || idents.contains("name")
                || idents.contains("supported_input_configs")
                || idents.contains("supported_output_configs")
        }
        "HostTrait" => {
            idents.contains("default_input_device")
                || idents.contains("default_output_device")
                || idents.contains("devices")
                || idents.contains("input_devices")
                || idents.contains("output_devices")
        }
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
        "Sample" => idents.contains("from_sample") || idents.contains("to_sample"),
        "SerializeStruct" => {
            idents.contains("serialize_field")
                || idents.contains("skip_field")
                || idents.contains("end")
        }
        "Engine" => {
            idents.contains("encode")
                || idents.contains("encode_string")
                || idents.contains("decode")
                || idents.contains("decode_vec")
        }
        "Rng" => {
            idents.contains("random")
                || idents.contains("random_range")
                || idents.contains("random_bool")
                || idents.contains("random_ratio")
                || idents.contains("fill")
        }
        "SeedableRng" => {
            idents.contains("seed_from_u64")
                || idents.contains("from_rng")
                || idents.contains("from_seed")
                || idents.contains("from_os_rng")
        }
        "WriteBytesExt" => {
            idents.contains("write_i8")
                || idents.contains("write_i16")
                || idents.contains("write_i32")
                || idents.contains("write_i64")
                || idents.contains("write_u8")
                || idents.contains("write_u16")
                || idents.contains("write_u32")
                || idents.contains("write_u64")
                || idents.contains("write_f32")
                || idents.contains("write_f64")
        }
        "Zero" => {
            idents.contains("zero") || idents.contains("is_zero") || idents.contains("set_zero")
        }
        "UnicodeSegmentation" => {
            idents.contains("split_word_bound_indices")
                || idents.contains("split_word_bounds")
                || idents.contains("unicode_word_indices")
                || idents.contains("unicode_words")
                || idents.contains("grapheme_indices")
                || idents.contains("graphemes")
        }
        "Resampler" => {
            idents.contains("process")
                || idents.contains("process_into_buffer")
                || idents.contains("reset")
                || idents.contains("input_frames_max")
                || idents.contains("input_frames_next")
                || idents.contains("output_frames_max")
                || idents.contains("output_delay")
                || idents.contains("nbr_channels")
        }
        "ThemeExt" => idents.contains("theme"),
        "AccessibilityExt" => idents.contains("register_accessible"),
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
    rebase_use_for_subfile_with_local_roots(use_src, &HashSet::new())
}

pub fn rebase_use_for_subfile_with_local_roots(
    use_src: &str,
    local_module_roots: &HashSet<String>,
) -> String {
    let Ok(item) = syn::parse_str::<syn::ItemUse>(use_src) else {
        return use_src.to_string();
    };
    let root_ident = match &item.tree {
        UseTree::Path(p) => &p.ident,
        UseTree::Name(n) => &n.ident,
        UseTree::Rename(r) => &r.ident,
        _ => return use_src.to_string(),
    };
    let ident_str = root_ident.to_string();
    let action = match ident_str.as_str() {
        "super" => Action::WrapSuper,
        "self" => Action::ReplaceSelfWithSuper,
        root if local_module_roots.contains(root) => Action::WrapSuper,
        _ => return use_src.to_string(),
    };
    let span = root_ident.span();
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
    fn rebase_local_module_root() {
        let roots = HashSet::from(["filters".to_string()]);
        assert_eq!(
            rebase_use_for_subfile_with_local_roots("use filters::{XtcFilters};", &roots),
            "use super::filters::{XtcFilters};"
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
    fn bucket_idents_include_derive_macro_names() {
        let idents = bucket_idents(
            "#[derive(Debug, Serialize, Deserialize)]\n#[serde(default)]\nstruct Session;\n",
        )
        .expect("source parses");
        assert!(idents.contains("Serialize"), "idents={idents:?}");
        assert!(idents.contains("Deserialize"), "idents={idents:?}");
        assert!(idents.contains("serde"), "idents={idents:?}");
    }

    #[test]
    fn bucket_idents_include_attribute_string_paths() {
        let idents = bucket_idents(
            "#[derive(Deserialize)]\nstruct Config {\n    #[serde(default = \"defaults::gain\")]\n    gain: f64,\n}\n",
        )
        .expect("source parses");
        assert!(idents.contains("defaults"), "idents={idents:?}");
        assert!(idents.contains("gain"), "idents={idents:?}");
    }

    #[test]
    fn bucket_idents_include_macro_tokens() {
        let idents = bucket_idents(
            "fn uses_match(preset: Preset) { let _ = matches!(preset.crossover, SimpleCrossoverChoice::Lr48); }\n",
        )
        .expect("source parses");
        assert!(idents.contains("matches"), "idents={idents:?}");
        assert!(
            idents.contains("SimpleCrossoverChoice"),
            "idents={idents:?}"
        );
    }

    #[test]
    fn cpal_extension_trait_imports_survive_method_syntax() {
        use crate::item::{ItemKind, ItemVis, ParsedItem};
        use std::collections::HashSet;

        let traits_source = "use cpal::traits::{DeviceTrait, HostTrait};".to_string();
        let traits_use = ParsedItem {
            id: 0,
            kind: ItemKind::Use,
            name: String::new(),
            vis: ItemVis::Private,
            is_cfg_test: false,
            line_start: 1,
            line_end: 1,
            source: traits_source,
            refs: Vec::new(),
        };
        let sample_use = ParsedItem {
            id: 1,
            kind: ItemKind::Use,
            name: String::new(),
            vis: ItemVis::Private,
            is_cfg_test: false,
            line_start: 2,
            line_end: 2,
            source: "use cpal::Sample;".to_string(),
            refs: Vec::new(),
        };
        let idents = HashSet::from([
            "default_output_device".to_string(),
            "from_sample".to_string(),
            "supported_output_configs".to_string(),
        ]);

        let rendered =
            render_uses_for(&[&traits_use, &sample_use], &idents, &HashSet::new(), false);

        assert!(rendered.contains("DeviceTrait"), "rendered={rendered}");
        assert!(rendered.contains("HostTrait"), "rendered={rendered}");
        assert!(rendered.contains("Sample"), "rendered={rendered}");
    }

    #[test]
    fn serde_serialize_struct_trait_import_survives_method_syntax() {
        use crate::item::{ItemKind, ItemVis, ParsedItem};
        use std::collections::HashSet;

        let serde_use = ParsedItem {
            id: 0,
            kind: ItemKind::Use,
            name: String::new(),
            vis: ItemVis::Private,
            is_cfg_test: false,
            line_start: 1,
            line_end: 1,
            source: "use serde::{ser::SerializeStruct, Serialize, Serializer};".to_string(),
            refs: Vec::new(),
        };
        let idents = HashSet::from([
            "Serialize".to_string(),
            "Serializer".to_string(),
            "serialize_struct".to_string(),
            "serialize_field".to_string(),
            "end".to_string(),
        ]);

        let rendered = render_uses_for(&[&serde_use], &idents, &HashSet::new(), false);

        assert!(rendered.contains("SerializeStruct"), "rendered={rendered}");
        assert!(rendered.contains("Serialize"), "rendered={rendered}");
        assert!(rendered.contains("Serializer"), "rendered={rendered}");
    }

    #[test]
    fn common_extension_trait_imports_survive_method_syntax() {
        use crate::item::{ItemKind, ItemVis, ParsedItem};
        use std::collections::HashSet;

        let imports = [
            ParsedItem {
                id: 0,
                kind: ItemKind::Use,
                name: String::new(),
                vis: ItemVis::Private,
                is_cfg_test: false,
                line_start: 1,
                line_end: 1,
                source: "use rustfft::num_traits::Zero;".to_string(),
                refs: Vec::new(),
            },
            ParsedItem {
                id: 1,
                kind: ItemKind::Use,
                name: String::new(),
                vis: ItemVis::Private,
                is_cfg_test: false,
                line_start: 2,
                line_end: 2,
                source: "use byteorder::{BigEndian, WriteBytesExt};".to_string(),
                refs: Vec::new(),
            },
            ParsedItem {
                id: 2,
                kind: ItemKind::Use,
                name: String::new(),
                vis: ItemVis::Private,
                is_cfg_test: false,
                line_start: 3,
                line_end: 3,
                source: "use rand::{Rng, SeedableRng};".to_string(),
                refs: Vec::new(),
            },
            ParsedItem {
                id: 3,
                kind: ItemKind::Use,
                name: String::new(),
                vis: ItemVis::Private,
                is_cfg_test: false,
                line_start: 4,
                line_end: 4,
                source: "use base64::{Engine as _, engine::general_purpose};".to_string(),
                refs: Vec::new(),
            },
            ParsedItem {
                id: 4,
                kind: ItemKind::Use,
                name: String::new(),
                vis: ItemVis::Private,
                is_cfg_test: false,
                line_start: 5,
                line_end: 5,
                source: "use unicode_segmentation::UnicodeSegmentation;".to_string(),
                refs: Vec::new(),
            },
            ParsedItem {
                id: 5,
                kind: ItemKind::Use,
                name: String::new(),
                vis: ItemVis::Private,
                is_cfg_test: false,
                line_start: 6,
                line_end: 6,
                source: "use rubato::{Fft, FixedSync, Resampler};".to_string(),
                refs: Vec::new(),
            },
            ParsedItem {
                id: 6,
                kind: ItemKind::Use,
                name: String::new(),
                vis: ItemVis::Private,
                is_cfg_test: false,
                line_start: 7,
                line_end: 7,
                source: "use crate::theme::ThemeExt;".to_string(),
                refs: Vec::new(),
            },
            ParsedItem {
                id: 7,
                kind: ItemKind::Use,
                name: String::new(),
                vis: ItemVis::Private,
                is_cfg_test: false,
                line_start: 8,
                line_end: 8,
                source: "use crate::accessibility::AccessibilityExt;".to_string(),
                refs: Vec::new(),
            },
        ];
        let idents = HashSet::from([
            "BigEndian".to_string(),
            "Complex64".to_string(),
            "Fft".to_string(),
            "FixedSync".to_string(),
            "StdRng".to_string(),
            "encode".to_string(),
            "from_rng".to_string(),
            "general_purpose".to_string(),
            "output_delay".to_string(),
            "output_frames_max".to_string(),
            "register_accessible".to_string(),
            "random_range".to_string(),
            "seed_from_u64".to_string(),
            "split_word_bound_indices".to_string(),
            "theme".to_string(),
            "write_f32".to_string(),
            "write_i32".to_string(),
            "zero".to_string(),
        ]);

        let rendered = render_uses_for(
            &imports.iter().collect::<Vec<_>>(),
            &idents,
            &HashSet::new(),
            false,
        );

        assert!(rendered.contains("Zero"), "rendered={rendered}");
        assert!(rendered.contains("WriteBytesExt"), "rendered={rendered}");
        assert!(rendered.contains("Rng"), "rendered={rendered}");
        assert!(rendered.contains("SeedableRng"), "rendered={rendered}");
        assert!(rendered.contains("Engine as _"), "rendered={rendered}");
        assert!(
            rendered.contains("UnicodeSegmentation"),
            "rendered={rendered}"
        );
        assert!(rendered.contains("Resampler"), "rendered={rendered}");
        assert!(rendered.contains("ThemeExt"), "rendered={rendered}");
        assert!(rendered.contains("AccessibilityExt"), "rendered={rendered}");
    }

    #[test]
    fn rebase_invalid_input_passes_through() {
        // If syn can't parse, return verbatim — better to ship the
        // original line than a corrupted one.
        assert_eq!(rebase_use_for_subfile("not a use stmt"), "not a use stmt");
    }
}
