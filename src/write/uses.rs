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
use proc_macro2::Ident;
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
    collect_tree(&parsed.tree, None, &mut out);
    out
}

pub fn use_roots(use_item: &ParsedItem) -> Vec<String> {
    debug_assert!(matches!(use_item.kind, ItemKind::Use));
    let parsed: syn::ItemUse = match syn::parse_str(&use_item.source) {
        Ok(p) => p,
        Err(_) => return Vec::new(),
    };
    root_of_tree(&parsed.tree).into_iter().collect()
}

fn root_of_tree(tree: &UseTree) -> Option<String> {
    match tree {
        UseTree::Path(p) => Some(p.ident.to_string()),
        UseTree::Name(n) => Some(n.ident.to_string()),
        UseTree::Rename(r) => Some(r.ident.to_string()),
        UseTree::Group(_) | UseTree::Glob(_) => None,
    }
}

fn collect_tree(tree: &UseTree, prefix: Option<&Ident>, out: &mut Vec<String>) {
    match tree {
        UseTree::Path(p) => collect_tree(&p.tree, Some(&p.ident), out),
        UseTree::Name(n) if n.ident == "self" => {
            if let Some(prefix) = prefix {
                out.push(prefix.to_string());
            } else {
                out.push(n.ident.to_string());
            }
        }
        UseTree::Name(n) => out.push(n.ident.to_string()),
        UseTree::Rename(r) => out.push(r.rename.to_string()),
        UseTree::Group(g) => {
            for t in &g.items {
                collect_tree(t, prefix, out);
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

pub fn path_roots_in_source(src: &str) -> HashSet<String> {
    let Ok(file) = syn::parse_file(src) else {
        return HashSet::new();
    };
    let mut collector = PathRootCollector {
        roots: HashSet::new(),
    };
    collector.visit_file(&file);
    collector.roots
}

struct IdentCollector {
    found: HashSet<String>,
}

struct PathRootCollector {
    roots: HashSet<String>,
}

impl<'ast> Visit<'ast> for PathRootCollector {
    fn visit_path(&mut self, path: &'ast syn::Path) {
        if path.segments.len() > 1
            && let Some(first) = path.segments.first()
        {
            let root = first.ident.to_string();
            if !matches!(root.as_str(), "crate" | "self" | "super" | "Self") {
                self.roots.insert(root);
            }
        }
        visit::visit_path(self, path);
    }
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
            TokenTree::Literal(lit) => scan_macro_string_literal(&lit, v),
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

fn scan_macro_string_literal(lit: &proc_macro2::Literal, v: &mut IdentCollector) {
    let Ok(lit) = syn::parse_str::<syn::LitStr>(&lit.to_string()) else {
        return;
    };
    collect_format_string_captures(&lit.value(), v);
}

fn collect_format_string_captures(value: &str, v: &mut IdentCollector) {
    let mut chars = value.char_indices().peekable();
    while let Some((_, ch)) = chars.next() {
        if ch != '{' {
            continue;
        }
        if matches!(chars.peek(), Some((_, '{'))) {
            chars.next();
            continue;
        }
        while matches!(chars.peek(), Some((_, ch)) if ch.is_whitespace()) {
            chars.next();
        }
        let Some((_, first)) = chars.peek().copied() else {
            continue;
        };
        if !is_ident_start(first) {
            continue;
        }
        let mut ident = String::new();
        ident.push(first);
        chars.next();
        while let Some((_, ch)) = chars.peek().copied() {
            if !is_ident_continue(ch) {
                break;
            }
            ident.push(ch);
            chars.next();
        }
        v.found.insert(ident);
    }
}

fn is_ident_start(ch: char) -> bool {
    ch == '_' || ch.is_ascii_alphabetic()
}

fn is_ident_continue(ch: char) -> bool {
    ch == '_' || ch.is_ascii_alphanumeric()
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
        .any(|ident| is_type_like_ident(ident) && !provided.contains(ident))
        || glob_prelude_hint_is_used(idents);

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
    parsed.tree = filter_tree(parsed.tree, idents, keep_globs, None)?;
    let mut rendered = parsed.to_token_stream().to_string();
    if !rendered.ends_with(';') {
        rendered.push(';');
    }
    Some(rendered)
}

fn filter_tree(
    tree: UseTree,
    idents: &HashSet<String>,
    keep_globs: bool,
    prefix: Option<Ident>,
) -> Option<UseTree> {
    match tree {
        UseTree::Path(mut p) => {
            let child_prefix = Some(p.ident.clone());
            p.tree = Box::new(filter_tree(*p.tree, idents, keep_globs, child_prefix)?);
            Some(UseTree::Path(p))
        }
        UseTree::Name(n) => {
            let binding = if n.ident == "self" {
                prefix
                    .as_ref()
                    .map(ToString::to_string)
                    .unwrap_or_else(|| n.ident.to_string())
            } else {
                n.ident.to_string()
            };
            binding_is_used(&binding, idents).then_some(UseTree::Name(n))
        }
        UseTree::Rename(r) => {
            let used = if r.rename == "_" {
                binding_is_used(&r.ident.to_string(), idents)
            } else {
                idents.contains(&r.rename.to_string())
                    || trait_method_hint_is_used(&r.ident.to_string(), idents)
            };
            used.then_some(UseTree::Rename(r))
        }
        UseTree::Group(mut g) => {
            g.items = g
                .items
                .into_iter()
                .filter_map(|tree| filter_tree(tree, idents, keep_globs, prefix.clone()))
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
    idents.contains(binding)
        || trait_method_hint_is_used(binding, idents)
        || macro_companion_hint_is_used(binding, idents)
}

fn macro_companion_hint_is_used(binding: &str, idents: &HashSet<String>) -> bool {
    match binding {
        "sel" | "sel_impl" => idents.contains("msg_send") || idents.contains("sel"),
        _ => false,
    }
}

fn glob_prelude_hint_is_used(idents: &HashSet<String>) -> bool {
    idents.contains("into_par_iter")
        || idents.contains("into_par_bridge")
        || idents.contains("par_iter")
        || idents.contains("par_iter_mut")
        || idents.contains("par_chunks")
        || idents.contains("par_chunks_mut")
        || idents.contains("par_sort")
        || idents.contains("par_sort_unstable")
}

fn trait_method_hint_is_used(binding: &str, idents: &HashSet<String>) -> bool {
    match binding {
        "Aead" => idents.contains("encrypt") || idents.contains("decrypt"),
        "AeadInPlace" => {
            idents.contains("encrypt_in_place")
                || idents.contains("decrypt_in_place")
                || idents.contains("encrypt_in_place_detached")
                || idents.contains("decrypt_in_place_detached")
        }
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
        "StreamTrait" => idents.contains("play") || idents.contains("pause"),
        "MediaSource" => idents.contains("is_seekable"),
        "CopyToUninit" => idents.contains("copy_to_uninit"),
        "Plugin" => {
            idents.contains("process")
                || idents.contains("process_f64")
                || idents.contains("reset")
                || idents.contains("info")
        }
        "Host" => {
            idents.contains("add_plugin")
                || idents.contains("get_plugin")
                || idents.contains("get_plugin_data")
                || idents.contains("remove_plugin")
                || idents.contains("process")
                || idents.contains("reset")
                || idents.contains("take_analyzer_contention_stats")
        }
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
        "DecodeAll" => idents.contains("decode_all"),
        "Encode" => idents.contains("encode") || idents.contains("using_encoded"),
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
        "ReadBytesExt" => {
            idents.contains("read_i8")
                || idents.contains("read_i16")
                || idents.contains("read_i24")
                || idents.contains("read_i32")
                || idents.contains("read_i64")
                || idents.contains("read_i128")
                || idents.contains("read_u8")
                || idents.contains("read_u16")
                || idents.contains("read_u24")
                || idents.contains("read_u32")
                || idents.contains("read_u64")
                || idents.contains("read_u128")
                || idents.contains("read_f32")
                || idents.contains("read_f64")
        }
        "Read" => {
            idents.contains("read")
                || idents.contains("read_exact")
                || idents.contains("read_to_end")
                || idents.contains("read_to_string")
        }
        "Write" => {
            idents.contains("write")
                || idents.contains("write_all")
                || idents.contains("write_fmt")
                || idents.contains("write_str")
                || idents.contains("writeln")
        }
        "AsyncReadExt" => {
            idents.contains("read")
                || idents.contains("read_exact")
                || idents.contains("read_to_end")
                || idents.contains("read_to_string")
        }
        "AsyncSeekExt" => idents.contains("seek"),
        "AsyncWriteExt" => {
            idents.contains("flush")
                || idents.contains("shutdown")
                || idents.contains("write")
                || idents.contains("write_all")
        }
        "Context" => idents.contains("context") || idents.contains("with_context"),
        "FromStr" => idents.contains("from_str"),
        "ValueEnum" => idents.contains("from_str") || idents.contains("value_variants"),
        "KeyInit" => idents.contains("new") || idents.contains("new_from_slice"),
        "Spanned" => idents.contains("span"),
        "Digest" => {
            idents.contains("finalize") || idents.contains("new") || idents.contains("update")
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
        "Itertools" => {
            idents.contains("find_or_first")
                || idents.contains("find_or_last")
                || idents.contains("collect_vec")
                || idents.contains("join")
        }
        "Scale" => idents.contains("invert"),
        "DesignExt" => idents.contains("design"),
        "AppContext" => idents.contains("new"),
        "PlayerAdapter" => {
            idents.contains("current_song")
                || idents.contains("next")
                || idents.contains("pause")
                || idents.contains("play")
                || idents.contains("playlist_info")
                || idents.contains("previous")
                || idents.contains("set_volume")
                || idents.contains("status")
                || idents.contains("stop")
        }
        "PluginEditingManager" => {
            idents.contains("sync_spectrum_visible")
                || idents.contains("add_plugin")
                || idents.contains("toggle_plugin")
                || idents.contains("move_plugin_up")
                || idents.contains("move_plugin_down")
                || idents.contains("select_next_plugin")
                || idents.contains("select_previous_plugin")
                || idents.contains("remove_plugin")
                || idents.contains("get_editing_plugin")
                || idents.contains("get_editing_plugin_mut")
                || idents.contains("select_next_param")
                || idents.contains("select_previous_param")
                || idents.contains("adjust_selected_param")
                || idents.contains("set_plugin_param")
                || idents.contains("set_plugin_param_string")
                || idents.contains("set_spectrum_tilt_correction")
                || idents.contains("set_spectrum_tilt_reference")
                || idents.contains("reset_plugin_param")
                || idents.contains("load_apo_file")
                || idents.contains("load_sofa_file")
                || idents.contains("add_eq_band")
                || idents.contains("remove_eq_band")
                || idents.contains("toggle_eq_band_mute")
                || idents.contains("toggle_eq_band_solo")
                || idents.contains("set_eq_per_channel_mode")
                || idents.contains("cycle_eq_filter_topology")
                || idents.contains("cycle_eq_filter_lambda")
                || idents.contains("add_eq_kautz_section")
                || idents.contains("pop_eq_kautz_section")
                || idents.contains("refresh_plugin_presets")
                || idents.contains("save_plugin_chain")
                || idents.contains("save_selected_preset")
                || idents.contains("load_plugin_chain")
                || idents.contains("load_selected_preset")
                || idents.contains("select_next_preset")
                || idents.contains("select_previous_preset")
                || idents.contains("toggle_chain_bypass")
                || idents.contains("toggle_chain_autogain")
                || idents.contains("toggle_plugin_solo")
                || idents.contains("apply_matrix_mono")
                || idents.contains("apply_matrix_ms")
        }
        "LevelMeterManager" => {
            idents.contains("update_level_meter_groups")
                || idents.contains("update_level_meter_peak_hold")
                || idents.contains("set_level_meter_mute")
                || idents.contains("set_level_meter_solo")
                || idents.contains("set_level_meter_dim")
        }
        "TuiEditablePlugin" => idents.contains("get_descriptors"),
        "ParamSectionStyle" => {
            idents.contains("param_section_style") || idents.contains("param_section_style_lg")
        }
        "ThemeExt" => idents.contains("theme"),
        "AccessibilityExt" => idents.contains("register_accessible"),
        "BoolExt" => idents.contains("to_objc"),
        "NSStringExt" => idents.contains("to_str"),
        "NSString" => idents.contains("UTF8String"),
        "NSURL" => idents.contains("absoluteString"),
        "NSArray" => idents.contains("count") || idents.contains("objectAtIndex"),
        "NSDictionary" => idents.contains("objectForKey_"),
        "NSFastEnumeration" => idents.contains("iter"),
        "NSControl" => idents.contains("setEnabled_") || idents.contains("isEnabled_"),
        "NSVisualEffectView" => idents.contains("setState_"),
        "PlatformKeyboardLayout" => idents.contains("id"),
        "OsStrExt" => idents.contains("from_bytes") || idents.contains("as_bytes"),
        "TCFType" => {
            idents.contains("as_CFTypeRef")
                || idents.contains("as_concrete_TypeRef")
                || idents.contains("wrap_under_create_rule")
                || idents.contains("wrap_under_get_rule")
        }
        "ResultExt" => idents.contains("log_err"),
        "Hash" => idents.contains("hash"),
        "Hasher" => {
            idents.contains("finish")
                || idents.contains("write")
                || idents.contains("write_u8")
                || idents.contains("write_u16")
                || idents.contains("write_u32")
                || idents.contains("write_u64")
                || idents.contains("write_usize")
        }
        "Seek" => idents.contains("seek"),
        "SliceRandom" => {
            idents.contains("shuffle")
                || idents.contains("choose")
                || idents.contains("choose_multiple")
                || idents.contains("choose_weighted")
                || idents.contains("partial_shuffle")
        }
        "SymbolicTraitAccessors" => idents.contains("is_monospace") || idents.contains("is_italic"),
        "TraitAccessors" => {
            idents.contains("normalized_slant")
                || idents.contains("normalized_weight")
                || idents.contains("normalized_width")
        }
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
    let Some(action) = use_root_action_for_ident(&ident_str, local_module_roots, true) else {
        return use_src.to_string();
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

/// Rebase every nested `use` item inside moved source. This catches function
/// or method-local imports such as `use filters::Thing;` which do not pass
/// through the generated file prelude.
pub fn rebase_use_roots_in_source(src: &str, local_module_roots: &HashSet<String>) -> String {
    let Ok(file) = syn::parse_file(src) else {
        return src.to_string();
    };
    let mut collector = UseRootEditCollector {
        src,
        local_module_roots,
        edits: Vec::new(),
    };
    collector.visit_file(&file);
    apply_use_root_edits(src, collector.edits)
}

/// Rebase bare module-root paths inside moved item bodies.
///
/// A path such as `parallel_eval::ParallelConfig` can resolve from the
/// original module, but after the item moves into a generated sibling module it
/// needs to point through the facade's parent: `super::parallel_eval::...`.
pub fn rebase_local_module_paths_in_source(
    src: &str,
    local_module_roots: &HashSet<String>,
) -> String {
    let Ok(file) = syn::parse_file(src) else {
        return src.to_string();
    };
    let mut collector = PathRootEditCollector {
        src,
        local_module_roots,
        edits: Vec::new(),
    };
    collector.visit_file(&file);
    apply_use_root_edits(src, collector.edits)
}

#[derive(Clone, Copy)]
enum Action {
    WrapSuper,
    ReplaceSelfWithSuper,
}

#[derive(Clone, Copy)]
struct UseRootEdit {
    start: usize,
    end: usize,
    action: Action,
}

struct UseRootEditCollector<'a> {
    src: &'a str,
    local_module_roots: &'a HashSet<String>,
    edits: Vec<UseRootEdit>,
}

impl<'ast> Visit<'ast> for UseRootEditCollector<'_> {
    fn visit_item_use(&mut self, item: &'ast syn::ItemUse) {
        if let Some((root, action)) = nested_use_root_action(&item.tree, self.local_module_roots) {
            let span = root.span();
            let start = span.start();
            let end = span.end();
            if let (Some(start), Some(end)) = (
                line_col_to_byte_offset(self.src, start.line, start.column),
                line_col_to_byte_offset(self.src, end.line, end.column),
            ) {
                self.edits.push(UseRootEdit { start, end, action });
            }
        }
        visit::visit_item_use(self, item);
    }
}

struct PathRootEditCollector<'a> {
    src: &'a str,
    local_module_roots: &'a HashSet<String>,
    edits: Vec<UseRootEdit>,
}

impl<'ast> Visit<'ast> for PathRootEditCollector<'_> {
    fn visit_item_use(&mut self, _item: &'ast syn::ItemUse) {
        // `use` trees are handled by `rebase_use_roots_in_source`; visiting
        // through them here would produce duplicate edits at the same span.
    }

    fn visit_path(&mut self, path: &'ast syn::Path) {
        if path.leading_colon.is_some() {
            return;
        }
        let Some(root) = path.segments.first().map(|segment| &segment.ident) else {
            return;
        };
        let ident = root.to_string();
        let action = match ident.as_str() {
            "self" if path.segments.len() > 1 => Some(Action::ReplaceSelfWithSuper),
            "super" | "crate" | "Self" => None,
            root if self.local_module_roots.contains(root) && path.segments.len() > 1 => {
                Some(Action::WrapSuper)
            }
            _ => None,
        };
        if let Some(action) = action {
            let span = root.span();
            let start = span.start();
            let end = span.end();
            if let (Some(start), Some(end)) = (
                line_col_to_byte_offset(self.src, start.line, start.column),
                line_col_to_byte_offset(self.src, end.line, end.column),
            ) {
                self.edits.push(UseRootEdit { start, end, action });
            }
        }
        visit::visit_path(self, path);
    }
}

fn nested_use_root_action<'a>(
    tree: &'a UseTree,
    local_module_roots: &HashSet<String>,
) -> Option<(&'a Ident, Action)> {
    use_root_action_inner(tree, local_module_roots, false)
}

fn use_root_action_inner<'a>(
    tree: &'a UseTree,
    local_module_roots: &HashSet<String>,
    rebase_super: bool,
) -> Option<(&'a Ident, Action)> {
    let root_ident = match tree {
        UseTree::Path(p) => &p.ident,
        UseTree::Name(n) => &n.ident,
        UseTree::Rename(r) => &r.ident,
        _ => return None,
    };
    let ident_str = root_ident.to_string();
    let action = use_root_action_for_ident(&ident_str, local_module_roots, rebase_super)?;
    Some((root_ident, action))
}

fn use_root_action_for_ident(
    ident: &str,
    local_module_roots: &HashSet<String>,
    rebase_super: bool,
) -> Option<Action> {
    match ident {
        "super" if rebase_super => Some(Action::WrapSuper),
        "self" => Some(Action::ReplaceSelfWithSuper),
        root if local_module_roots.contains(root) => Some(Action::WrapSuper),
        _ => None,
    }
}

fn apply_use_root_edits(src: &str, mut edits: Vec<UseRootEdit>) -> String {
    if edits.is_empty() {
        return src.to_string();
    }
    edits.sort_by_key(|edit| edit.start);
    edits.dedup_by_key(|edit| (edit.start, edit.end));
    let mut out = src.to_string();
    for edit in edits.into_iter().rev() {
        if edit.start > out.len() || edit.end > out.len() || edit.start > edit.end {
            continue;
        }
        match edit.action {
            Action::WrapSuper => {
                out.insert_str(edit.start, "super::");
            }
            Action::ReplaceSelfWithSuper => {
                if &out[edit.start..edit.end] == "self" {
                    out.replace_range(edit.start..edit.end, "super");
                }
            }
        }
    }
    out
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
    fn rebase_nested_local_use_root() {
        let roots = HashSet::from(["filters".to_string()]);
        let src = "impl Plugin {\n    fn build() {\n        use filters::XtcFilters;\n        let _ = XtcFilters::new();\n    }\n}\n";
        let rewritten = rebase_use_roots_in_source(src, &roots);
        assert!(
            rewritten.contains("use super::filters::XtcFilters;"),
            "rewritten:\n{rewritten}"
        );
    }

    #[test]
    fn rebase_bare_local_module_paths_in_moved_source() {
        let roots = HashSet::from(["parallel_eval".to_string(), "lshade".to_string()]);
        let src = "pub struct DEConfig {\n    pub parallel: parallel_eval::ParallelConfig,\n    pub lshade: lshade::LShadeConfig,\n}\n\nimpl Default for DEConfig {\n    fn default() -> Self {\n        Self {\n            parallel: parallel_eval::ParallelConfig::default(),\n            lshade: lshade::LShadeConfig::default(),\n        }\n    }\n}\n";
        let rewritten = rebase_local_module_paths_in_source(src, &roots);
        assert!(
            rewritten.contains("pub parallel: super::parallel_eval::ParallelConfig"),
            "rewritten:\n{rewritten}"
        );
        assert!(
            rewritten.contains("pub lshade: super::lshade::LShadeConfig"),
            "rewritten:\n{rewritten}"
        );
        assert!(
            rewritten.contains("parallel: super::parallel_eval::ParallelConfig::default()"),
            "rewritten:\n{rewritten}"
        );
        assert!(
            rewritten.contains("lshade: super::lshade::LShadeConfig::default()"),
            "rewritten:\n{rewritten}"
        );
    }

    #[test]
    fn rebase_local_module_paths_leaves_self_value_receiver_alone() {
        let roots = HashSet::from(["filters".to_string()]);
        let src =
            "impl Plugin {\n    fn value(&self) -> usize {\n        self.filters.len()\n    }\n}\n";
        let rewritten = rebase_local_module_paths_in_source(src, &roots);
        assert!(
            rewritten.contains("self.filters.len()"),
            "rewritten:\n{rewritten}"
        );
        assert!(!rewritten.contains("super.filters"));
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
    fn bucket_idents_include_format_string_captures() {
        let idents = bucket_idents(
            "fn greeting() -> String { let MPD_VERSION = \"0.23.5\"; format!(\"OK MPD {MPD_VERSION}\\n\") }\n",
        )
        .expect("source parses");

        assert!(idents.contains("format"), "idents={idents:?}");
        assert!(idents.contains("MPD_VERSION"), "idents={idents:?}");
    }

    #[test]
    fn cpal_extension_trait_imports_survive_method_syntax() {
        use crate::item::{ItemKind, ItemVis, ParsedItem};
        use std::collections::HashSet;

        let traits_source = "use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};".to_string();
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
            "pause".to_string(),
            "play".to_string(),
            "supported_output_configs".to_string(),
        ]);

        let rendered =
            render_uses_for(&[&traits_use, &sample_use], &idents, &HashSet::new(), false);

        assert!(rendered.contains("DeviceTrait"), "rendered={rendered}");
        assert!(rendered.contains("HostTrait"), "rendered={rendered}");
        assert!(rendered.contains("StreamTrait"), "rendered={rendered}");
        assert!(rendered.contains("Sample"), "rendered={rendered}");
    }

    #[test]
    fn plugin_host_and_copy_to_uninit_traits_survive_method_syntax() {
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
                source: "use rtrb::CopyToUninit;".to_string(),
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
                source: "use sotf_plugins::{Host, Plugin};".to_string(),
                refs: Vec::new(),
            },
        ];
        let use_refs: Vec<&ParsedItem> = imports.iter().collect();
        let idents = HashSet::from([
            "copy_to_uninit".to_string(),
            "get_plugin_data".to_string(),
            "process".to_string(),
            "reset".to_string(),
            "take_analyzer_contention_stats".to_string(),
        ]);

        let rendered = render_uses_for(&use_refs, &idents, &HashSet::new(), false);

        assert!(rendered.contains("CopyToUninit"), "rendered={rendered}");
        assert!(rendered.contains("Host"), "rendered={rendered}");
        assert!(rendered.contains("Plugin"), "rendered={rendered}");
    }

    #[test]
    fn fmt_context_and_from_str_traits_survive_method_syntax() {
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
                source: "use std::fmt::Write;".to_string(),
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
                source: "use anyhow::Context;".to_string(),
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
                source: "use std::str::FromStr;".to_string(),
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
                source: "use clap::ValueEnum;".to_string(),
                refs: Vec::new(),
            },
        ];
        let use_refs: Vec<&ParsedItem> = imports.iter().collect();
        let idents = HashSet::from([
            "from_str".to_string(),
            "with_context".to_string(),
            "writeln".to_string(),
        ]);

        let rendered = render_uses_for(&use_refs, &idents, &HashSet::new(), false);

        assert!(rendered.contains("Write"), "rendered={rendered}");
        assert!(rendered.contains("Context"), "rendered={rendered}");
        assert!(rendered.contains("FromStr"), "rendered={rendered}");
        assert!(rendered.contains("ValueEnum"), "rendered={rendered}");
    }

    #[test]
    fn renamed_extension_trait_survives_method_syntax() {
        use crate::item::{ItemKind, ItemVis, ParsedItem};
        use std::collections::HashSet;

        let item = ParsedItem {
            id: 0,
            kind: ItemKind::Use,
            name: String::new(),
            vis: ItemVis::Private,
            is_cfg_test: false,
            line_start: 1,
            line_end: 1,
            source: "use std::fmt::Write as FmtWrite;".to_string(),
            refs: Vec::new(),
        };
        let idents = HashSet::from(["writeln".to_string()]);

        let rendered = render_uses_for(&[&item], &idents, &HashSet::new(), false);

        assert!(rendered.contains("FmtWrite"), "rendered={rendered}");
    }

    #[test]
    fn std_io_write_trait_survives_write_all_method_syntax() {
        use crate::item::{ItemKind, ItemVis, ParsedItem};
        use std::collections::HashSet;

        let write_use = ParsedItem {
            id: 0,
            kind: ItemKind::Use,
            name: String::new(),
            vis: ItemVis::Private,
            is_cfg_test: false,
            line_start: 1,
            line_end: 1,
            source: "use std::io::Write;".to_string(),
            refs: Vec::new(),
        };
        let idents = HashSet::from(["write_all".to_string()]);

        let rendered = render_uses_for(&[&write_use], &idents, &HashSet::new(), false);

        assert!(rendered.contains("Write"), "rendered={rendered}");
    }

    #[test]
    fn rayon_prelude_glob_survives_parallel_method_syntax() {
        use crate::item::{ItemKind, ItemVis, ParsedItem};
        use std::collections::HashSet;

        let rayon_use = ParsedItem {
            id: 0,
            kind: ItemKind::Use,
            name: String::new(),
            vis: ItemVis::Private,
            is_cfg_test: false,
            line_start: 1,
            line_end: 1,
            source: "use rayon::prelude::*;".to_string(),
            refs: Vec::new(),
        };
        let idents = HashSet::from(["into_par_iter".to_string()]);

        let rendered = render_uses_for(&[&rayon_use], &idents, &HashSet::new(), false);

        assert!(
            rendered.contains("rayon::prelude::*"),
            "rendered={rendered}"
        );
    }

    #[test]
    fn syn_spanned_trait_survives_span_method_syntax() {
        use crate::item::{ItemKind, ItemVis, ParsedItem};
        use std::collections::HashSet;

        let spanned_use = ParsedItem {
            id: 0,
            kind: ItemKind::Use,
            name: String::new(),
            vis: ItemVis::Private,
            is_cfg_test: false,
            line_start: 1,
            line_end: 1,
            source: "use syn::spanned::Spanned;".to_string(),
            refs: Vec::new(),
        };
        let idents = HashSet::from(["span".to_string()]);

        let rendered = render_uses_for(&[&spanned_use], &idents, &HashSet::new(), false);

        assert!(rendered.contains("Spanned"), "rendered={rendered}");
    }

    #[test]
    fn aead_traits_survive_method_syntax() {
        use crate::item::{ItemKind, ItemVis, ParsedItem};
        use std::collections::HashSet;

        let aead_use = ParsedItem {
            id: 0,
            kind: ItemKind::Use,
            name: String::new(),
            vis: ItemVis::Private,
            is_cfg_test: false,
            line_start: 1,
            line_end: 1,
            source: "use chacha20poly1305::aead::{Aead, AeadInPlace, KeyInit};".to_string(),
            refs: Vec::new(),
        };
        let idents = HashSet::from([
            "decrypt".to_string(),
            "encrypt_in_place_detached".to_string(),
            "new_from_slice".to_string(),
        ]);

        let rendered = render_uses_for(&[&aead_use], &idents, &HashSet::new(), false);

        assert!(rendered.contains("Aead"), "rendered={rendered}");
        assert!(rendered.contains("AeadInPlace"), "rendered={rendered}");
        assert!(rendered.contains("KeyInit"), "rendered={rendered}");
    }

    #[test]
    fn byteorder_read_bytes_ext_survives_read_u24_method_syntax() {
        use crate::item::{ItemKind, ItemVis, ParsedItem};
        use std::collections::HashSet;

        let read_use = ParsedItem {
            id: 0,
            kind: ItemKind::Use,
            name: String::new(),
            vis: ItemVis::Private,
            is_cfg_test: false,
            line_start: 1,
            line_end: 1,
            source: "use byteorder::{BigEndian, ReadBytesExt};".to_string(),
            refs: Vec::new(),
        };
        let idents = HashSet::from(["BigEndian".to_string(), "read_u24".to_string()]);

        let rendered = render_uses_for(&[&read_use], &idents, &HashSet::new(), false);

        assert!(rendered.contains("ReadBytesExt"), "rendered={rendered}");
    }

    #[test]
    fn core_text_trait_accessors_survive_method_syntax() {
        use crate::item::{ItemKind, ItemVis, ParsedItem};
        use std::collections::HashSet;

        let core_text_use = ParsedItem {
            id: 0,
            kind: ItemKind::Use,
            name: String::new(),
            vis: ItemVis::Private,
            is_cfg_test: false,
            line_start: 1,
            line_end: 1,
            source: "use core_text::font_descriptor::{SymbolicTraitAccessors, TraitAccessors};"
                .to_string(),
            refs: Vec::new(),
        };
        let idents = HashSet::from([
            "is_italic".to_string(),
            "is_monospace".to_string(),
            "normalized_slant".to_string(),
            "normalized_weight".to_string(),
            "normalized_width".to_string(),
        ]);

        let rendered = render_uses_for(&[&core_text_use], &idents, &HashSet::new(), false);

        assert!(
            rendered.contains("SymbolicTraitAccessors"),
            "rendered={rendered}"
        );
        assert!(rendered.contains("TraitAccessors"), "rendered={rendered}");
    }

    #[test]
    fn underscore_renamed_extension_trait_survives_method_syntax() {
        use crate::item::{ItemKind, ItemVis, ParsedItem};
        use std::collections::HashSet;

        let item = ParsedItem {
            id: 0,
            kind: ItemKind::Use,
            name: String::new(),
            vis: ItemVis::Private,
            is_cfg_test: false,
            line_start: 1,
            line_end: 1,
            source: "use gpui::AppContext as _;".to_string(),
            refs: Vec::new(),
        };
        let idents = HashSet::from(["new".to_string()]);

        let rendered = render_uses_for(&[&item], &idents, &HashSet::new(), false);

        assert!(rendered.contains("AppContext as _"), "rendered={rendered}");
    }

    #[test]
    fn objc_macro_and_extension_traits_survive_method_syntax() {
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
                source: "use objc::{class, msg_send, sel, sel_impl};".to_string(),
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
                source: "use cocoa::foundation::{NSDictionary, NSFastEnumeration};".to_string(),
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
                source: "use crate::{BoolExt, NSStringExt};".to_string(),
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
                source: "use util::ResultExt;".to_string(),
                refs: Vec::new(),
            },
        ];
        let use_refs: Vec<&ParsedItem> = imports.iter().collect();
        let idents = HashSet::from([
            "class".to_string(),
            "iter".to_string(),
            "log_err".to_string(),
            "msg_send".to_string(),
            "objectForKey_".to_string(),
            "to_objc".to_string(),
            "to_str".to_string(),
        ]);

        let rendered = render_uses_for(&use_refs, &idents, &HashSet::new(), false);

        assert!(rendered.contains("msg_send"), "rendered={rendered}");
        assert!(rendered.contains("sel"), "rendered={rendered}");
        assert!(rendered.contains("sel_impl"), "rendered={rendered}");
        assert!(rendered.contains("NSDictionary"), "rendered={rendered}");
        assert!(
            rendered.contains("NSFastEnumeration"),
            "rendered={rendered}"
        );
        assert!(rendered.contains("BoolExt"), "rendered={rendered}");
        assert!(rendered.contains("NSStringExt"), "rendered={rendered}");
        assert!(rendered.contains("ResultExt"), "rendered={rendered}");
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
    fn hash_trait_imports_survive_method_syntax() {
        use crate::item::{ItemKind, ItemVis, ParsedItem};
        use std::collections::HashSet;

        let hash_use = ParsedItem {
            id: 0,
            kind: ItemKind::Use,
            name: String::new(),
            vis: ItemVis::Private,
            is_cfg_test: false,
            line_start: 1,
            line_end: 1,
            source: "use std::hash::{Hash, Hasher};".to_string(),
            refs: Vec::new(),
        };
        let idents = HashSet::from(["hash".to_string(), "finish".to_string()]);

        let rendered = render_uses_for(&[&hash_use], &idents, &HashSet::new(), false);

        assert!(rendered.contains("Hash"), "rendered={rendered}");
        assert!(rendered.contains("Hasher"), "rendered={rendered}");
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
            ParsedItem {
                id: 8,
                kind: ItemKind::Use,
                name: String::new(),
                vis: ItemVis::Private,
                is_cfg_test: false,
                line_start: 9,
                line_end: 9,
                source: "use gpui_design::DesignExt;".to_string(),
                refs: Vec::new(),
            },
            ParsedItem {
                id: 9,
                kind: ItemKind::Use,
                name: String::new(),
                vis: ItemVis::Private,
                is_cfg_test: false,
                line_start: 10,
                line_end: 10,
                source: "use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};".to_string(),
                refs: Vec::new(),
            },
            ParsedItem {
                id: 10,
                kind: ItemKind::Use,
                name: String::new(),
                vis: ItemVis::Private,
                is_cfg_test: false,
                line_start: 11,
                line_end: 11,
                source: "use sotf_mpd::PlayerAdapter;".to_string(),
                refs: Vec::new(),
            },
            ParsedItem {
                id: 11,
                kind: ItemKind::Use,
                name: String::new(),
                vis: ItemVis::Private,
                is_cfg_test: false,
                line_start: 12,
                line_end: 12,
                source: "use sha2::Digest;".to_string(),
                refs: Vec::new(),
            },
            ParsedItem {
                id: 12,
                kind: ItemKind::Use,
                name: String::new(),
                vis: ItemVis::Private,
                is_cfg_test: false,
                line_start: 13,
                line_end: 13,
                source: "use std::io::Read;".to_string(),
                refs: Vec::new(),
            },
            ParsedItem {
                id: 13,
                kind: ItemKind::Use,
                name: String::new(),
                vis: ItemVis::Private,
                is_cfg_test: false,
                line_start: 14,
                line_end: 14,
                source: "use core_foundation::base::{CFType, TCFType};".to_string(),
                refs: Vec::new(),
            },
            ParsedItem {
                id: 14,
                kind: ItemKind::Use,
                name: String::new(),
                vis: ItemVis::Private,
                is_cfg_test: false,
                line_start: 15,
                line_end: 15,
                source: "use cocoa::foundation::{NSArray, NSString, NSURL};".to_string(),
                refs: Vec::new(),
            },
            ParsedItem {
                id: 15,
                kind: ItemKind::Use,
                name: String::new(),
                vis: ItemVis::Private,
                is_cfg_test: false,
                line_start: 16,
                line_end: 16,
                source: "use std::os::unix::ffi::OsStrExt;".to_string(),
                refs: Vec::new(),
            },
            ParsedItem {
                id: 16,
                kind: ItemKind::Use,
                name: String::new(),
                vis: ItemVis::Private,
                is_cfg_test: false,
                line_start: 17,
                line_end: 17,
                source: "use itertools::Itertools;".to_string(),
                refs: Vec::new(),
            },
            ParsedItem {
                id: 17,
                kind: ItemKind::Use,
                name: String::new(),
                vis: ItemVis::Private,
                is_cfg_test: false,
                line_start: 18,
                line_end: 18,
                source: "use cocoa::appkit::{NSControl as _, NSVisualEffectView};".to_string(),
                refs: Vec::new(),
            },
            ParsedItem {
                id: 18,
                kind: ItemKind::Use,
                name: String::new(),
                vis: ItemVis::Private,
                is_cfg_test: false,
                line_start: 19,
                line_end: 19,
                source: "use gpui::PlatformKeyboardLayout;".to_string(),
                refs: Vec::new(),
            },
            ParsedItem {
                id: 19,
                kind: ItemKind::Use,
                name: String::new(),
                vis: ItemVis::Private,
                is_cfg_test: false,
                line_start: 20,
                line_end: 20,
                source: "use d3rs::scale::{LinearScale, LogScale, Scale};".to_string(),
                refs: Vec::new(),
            },
            ParsedItem {
                id: 20,
                kind: ItemKind::Use,
                name: String::new(),
                vis: ItemVis::Private,
                is_cfg_test: false,
                line_start: 21,
                line_end: 21,
                source: "use sotf_audio_player::ui_params::TuiEditablePlugin;".to_string(),
                refs: Vec::new(),
            },
            ParsedItem {
                id: 21,
                kind: ItemKind::Use,
                name: String::new(),
                vis: ItemVis::Private,
                is_cfg_test: false,
                line_start: 22,
                line_end: 22,
                source: "use codec::{DecodeAll, Encode};".to_string(),
                refs: Vec::new(),
            },
            ParsedItem {
                id: 22,
                kind: ItemKind::Use,
                name: String::new(),
                vis: ItemVis::Private,
                is_cfg_test: false,
                line_start: 23,
                line_end: 23,
                source: "use rand::seq::SliceRandom;".to_string(),
                refs: Vec::new(),
            },
        ];
        let idents = HashSet::from([
            "absoluteString".to_string(),
            "as_CFTypeRef".to_string(),
            "as_concrete_TypeRef".to_string(),
            "BigEndian".to_string(),
            "Complex64".to_string(),
            "Fft".to_string(),
            "FixedSync".to_string(),
            "UTF8String".to_string(),
            "find_or_first".to_string(),
            "from_bytes".to_string(),
            "current_song".to_string(),
            "design".to_string(),
            "finalize".to_string(),
            "id".to_string(),
            "invert".to_string(),
            "objectAtIndex".to_string(),
            "StdRng".to_string(),
            "decode_all".to_string(),
            "encode".to_string(),
            "from_rng".to_string(),
            "general_purpose".to_string(),
            "get_descriptors".to_string(),
            "output_delay".to_string(),
            "output_frames_max".to_string(),
            "read".to_string(),
            "read_exact".to_string(),
            "register_accessible".to_string(),
            "seek".to_string(),
            "random_range".to_string(),
            "seed_from_u64".to_string(),
            "setEnabled_".to_string(),
            "setState_".to_string(),
            "set_volume".to_string(),
            "shutdown".to_string(),
            "shuffle".to_string(),
            "split_word_bound_indices".to_string(),
            "theme".to_string(),
            "write_all".to_string(),
            "write_f32".to_string(),
            "write_i32".to_string(),
            "wrap_under_create_rule".to_string(),
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
        assert!(rendered.contains("DesignExt"), "rendered={rendered}");
        assert!(rendered.contains("AsyncReadExt"), "rendered={rendered}");
        assert!(rendered.contains("AsyncSeekExt"), "rendered={rendered}");
        assert!(rendered.contains("AsyncWriteExt"), "rendered={rendered}");
        assert!(rendered.contains("PlayerAdapter"), "rendered={rendered}");
        assert!(rendered.contains("Digest"), "rendered={rendered}");
        assert!(rendered.contains("Read"), "rendered={rendered}");
        assert!(rendered.contains("TCFType"), "rendered={rendered}");
        assert!(rendered.contains("NSArray"), "rendered={rendered}");
        assert!(rendered.contains("NSString"), "rendered={rendered}");
        assert!(rendered.contains("NSURL"), "rendered={rendered}");
        assert!(rendered.contains("OsStrExt"), "rendered={rendered}");
        assert!(rendered.contains("Itertools"), "rendered={rendered}");
        assert!(rendered.contains("NSControl"), "rendered={rendered}");
        assert!(
            rendered.contains("NSVisualEffectView"),
            "rendered={rendered}"
        );
        assert!(
            rendered.contains("PlatformKeyboardLayout"),
            "rendered={rendered}"
        );
        assert!(rendered.contains("Scale"), "rendered={rendered}");
        assert!(
            rendered.contains("TuiEditablePlugin"),
            "rendered={rendered}"
        );
        assert!(rendered.contains("ThemeExt"), "rendered={rendered}");
        assert!(rendered.contains("AccessibilityExt"), "rendered={rendered}");
        assert!(rendered.contains("DecodeAll"), "rendered={rendered}");
        assert!(rendered.contains("Encode"), "rendered={rendered}");
        assert!(rendered.contains("SliceRandom"), "rendered={rendered}");
    }

    #[test]
    fn local_extension_trait_imports_survive_method_syntax() {
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
                source: "use crate::components::plugins::editing::PluginEditingManager;"
                    .to_string(),
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
                source: "use crate::components::plugins::level_meters::LevelMeterManager;"
                    .to_string(),
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
                source: "use crate::components::plugins::common::ParamSectionStyle;".to_string(),
                refs: Vec::new(),
            },
        ];
        let idents = HashSet::from([
            "toggle_plugin".to_string(),
            "update_level_meter_groups".to_string(),
            "param_section_style_lg".to_string(),
        ]);

        let rendered = render_uses_for(
            &imports.iter().collect::<Vec<_>>(),
            &idents,
            &HashSet::new(),
            false,
        );

        assert!(
            rendered.contains("PluginEditingManager"),
            "rendered={rendered}"
        );
        assert!(
            rendered.contains("LevelMeterManager"),
            "rendered={rendered}"
        );
        assert!(
            rendered.contains("ParamSectionStyle"),
            "rendered={rendered}"
        );
    }

    #[test]
    fn rebase_invalid_input_passes_through() {
        // If syn can't parse, return verbatim — better to ship the
        // original line than a corrupted one.
        assert_eq!(rebase_use_for_subfile("not a use stmt"), "not a use stmt");
    }

    #[test]
    fn use_roots_reports_external_path_root() {
        use crate::item::{ItemKind, ItemVis, ParsedItem};

        let item = ParsedItem {
            id: 0,
            kind: ItemKind::Use,
            name: String::new(),
            vis: ItemVis::Private,
            is_cfg_test: false,
            line_start: 1,
            line_end: 1,
            source: "use coreaudio::audio_unit::macos_helpers;".to_string(),
            refs: Vec::new(),
        };

        assert_eq!(use_roots(&item), vec!["coreaudio".to_string()]);
    }

    #[test]
    fn grouped_self_import_counts_as_module_binding() {
        use crate::item::{ItemKind, ItemVis, ParsedItem};
        use std::collections::HashSet;

        let item = ParsedItem {
            id: 0,
            kind: ItemKind::Use,
            name: String::new(),
            vis: ItemVis::Private,
            is_cfg_test: false,
            line_start: 1,
            line_end: 1,
            source: "use super::multiseat::{self, MultiSeatMeasurements};".to_string(),
            refs: Vec::new(),
        };
        let idents = HashSet::from(["multiseat".to_string(), "MultiSeatMeasurements".to_string()]);

        let rendered = render_uses_for(&[&item], &idents, &HashSet::new(), true);

        assert!(rendered.contains("self"), "rendered={rendered}");
        assert!(
            rendered.contains("MultiSeatMeasurements"),
            "rendered={rendered}"
        );
    }

    #[test]
    fn path_roots_reports_fully_qualified_external_roots() {
        let roots = path_roots_in_source(
            "fn f() { coreaudio::audio_unit::macos_helpers::get_hogging_pid(1); }",
        );

        assert!(roots.contains("coreaudio"), "roots={roots:?}");
    }
}
