use anyhow::{Context, Result};
use quote::ToTokens;
use serde::Serialize;
use syn::spanned::Spanned;
use syn::{Attribute, Item, Type, Visibility};

pub type ItemId = usize;

#[derive(Debug, Clone, Serialize)]
pub enum ItemKind {
    Fn {
        is_test: bool,
    },
    Struct,
    Enum,
    Union,
    Trait,
    TraitAlias,
    Impl {
        self_ty: String,
        trait_path: Option<String>,
    },
    Macro,
    Const,
    Static,
    TypeAlias,
    Use,
    ExternCrate,
    Mod,
    ForeignMod,
    Verbatim,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum ItemVis {
    Public,
    /// `pub(crate)`
    Crate,
    /// `pub(super)` — kept distinct from generic `Restricted` because the
    /// splitter needs to rebase it when an item moves into a sub-bucket
    /// (the module hierarchy gains a level, so `super` now points one
    /// level less far up than the author intended).
    Super,
    /// Any other `pub(in path)` form we don't specialize.
    Restricted,
    Private,
}

#[derive(Debug, Clone, Serialize)]
pub struct ParsedItem {
    pub id: ItemId,
    pub kind: ItemKind,
    pub name: String,
    pub vis: ItemVis,
    pub is_cfg_test: bool,
    pub line_start: usize,
    pub line_end: usize,
    #[serde(skip_serializing)]
    pub source: String,
    pub refs: Vec<String>,
}

impl ParsedItem {
    pub fn is_data_kind(&self) -> bool {
        matches!(
            self.kind,
            ItemKind::Struct | ItemKind::Enum | ItemKind::Union | ItemKind::TypeAlias
        )
    }

    /// First non-attr/non-doc line of the source — useful as a one-line
    /// signature for an LLM prompt or for human reading.
    pub fn signature(&self) -> &str {
        for line in self.source.lines() {
            let t = line.trim_start();
            if t.is_empty()
                || t.starts_with('#')
                || t.starts_with("///")
                || t.starts_with("//!")
                || t.starts_with("//")
            {
                continue;
            }
            return t;
        }
        ""
    }

    pub fn is_external_mod_decl(&self) -> bool {
        if !matches!(self.kind, ItemKind::Mod) {
            return false;
        }
        matches!(
            syn::parse_str::<syn::Item>(&self.source),
            Ok(syn::Item::Mod(module)) if module.content.is_none()
        )
    }

    /// Whether this item carries `#[docify::export]` (or `#[docify::export(...)]`).
    /// Docify-exported items must stay in the facade because other files embed
    /// them via `docify::embed!("./src/lib.rs", item_name)` which resolves at
    /// compile time against the original file path.
    pub fn is_docify_export(&self) -> bool {
        let Ok(item) = syn::parse_str::<syn::Item>(&self.source) else {
            return false;
        };
        item_attrs(&item).iter().any(is_docify_export_attr)
    }
}

pub fn parse_file(src: &str) -> Result<Vec<ParsedItem>> {
    let file = syn::parse_file(src).context("failed to parse Rust source")?;
    let lines: Vec<&str> = src.lines().collect();
    let mut out = Vec::with_capacity(file.items.len());
    for (i, item) in file.items.iter().enumerate() {
        out.push(make_parsed(i, item, &lines));
    }
    Ok(out)
}

fn make_parsed(id: ItemId, item: &Item, lines: &[&str]) -> ParsedItem {
    let attrs = item_attrs(item);
    let mut line_start = attrs
        .iter()
        .map(|a| a.span().start().line)
        .min()
        .unwrap_or_else(|| item.span().start().line);
    let line_end = item.span().end().line;
    if let Item::Mod(module) = item
        && module.content.is_some()
    {
        line_start =
            repair_inline_mod_start(lines, line_start, line_end, &module.ident.to_string());
    }
    let source = slice_lines(lines, line_start, line_end);
    let is_cfg_test = attrs.iter().any(is_cfg_test_attr);

    let (kind, name) = item_kind_and_name(item);
    let vis = item_vis(item);

    ParsedItem {
        id,
        kind,
        name,
        vis,
        is_cfg_test,
        line_start,
        line_end,
        source,
        refs: Vec::new(),
    }
}

fn slice_lines(lines: &[&str], start: usize, end: usize) -> String {
    if start == 0 || end == 0 || start > lines.len() {
        return String::new();
    }
    let s = start - 1;
    let e = end.min(lines.len());
    lines[s..e].join("\n")
}

fn repair_inline_mod_start(lines: &[&str], start: usize, end: usize, name: &str) -> usize {
    let source = slice_lines(lines, start, end);
    if parses_as_inline_mod(&source, name) {
        return start;
    }

    let start_idx = start.saturating_sub(1);
    let search_start = start_idx.saturating_sub(50);
    for idx in (search_start..start_idx).rev() {
        let line = lines.get(idx).map_or("", |line| line.trim_start());
        if !line.contains("mod") || !line.contains(name) || !line.contains('{') {
            continue;
        }
        let candidate_start = idx + 1;
        let candidate = slice_lines(lines, candidate_start, end);
        if parses_as_inline_mod(&candidate, name) {
            return candidate_start;
        }
    }

    start
}

fn parses_as_inline_mod(source: &str, name: &str) -> bool {
    matches!(
        syn::parse_file(source),
        Ok(file)
            if matches!(
                file.items.as_slice(),
                [Item::Mod(module)]
                    if module.ident == name && module.content.is_some()
            )
    )
}

fn item_attrs(item: &Item) -> &[Attribute] {
    match item {
        Item::Const(i) => &i.attrs,
        Item::Enum(i) => &i.attrs,
        Item::ExternCrate(i) => &i.attrs,
        Item::Fn(i) => &i.attrs,
        Item::ForeignMod(i) => &i.attrs,
        Item::Impl(i) => &i.attrs,
        Item::Macro(i) => &i.attrs,
        Item::Mod(i) => &i.attrs,
        Item::Static(i) => &i.attrs,
        Item::Struct(i) => &i.attrs,
        Item::Trait(i) => &i.attrs,
        Item::TraitAlias(i) => &i.attrs,
        Item::Type(i) => &i.attrs,
        Item::Union(i) => &i.attrs,
        Item::Use(i) => &i.attrs,
        Item::Verbatim(_) => &[],
        other => panic!("unhandled syn::Item variant: {other:?}"),
    }
}

fn item_kind_and_name(item: &Item) -> (ItemKind, String) {
    match item {
        Item::Const(i) => (ItemKind::Const, i.ident.to_string()),
        Item::Enum(i) => (ItemKind::Enum, i.ident.to_string()),
        Item::ExternCrate(i) => (ItemKind::ExternCrate, i.ident.to_string()),
        Item::Fn(i) => {
            let is_test = i.attrs.iter().any(is_test_attr);
            (ItemKind::Fn { is_test }, i.sig.ident.to_string())
        }
        Item::ForeignMod(_) => (ItemKind::ForeignMod, String::new()),
        Item::Impl(i) => {
            let self_ty = type_to_anchor(&i.self_ty);
            let trait_path = i.trait_.as_ref().map(|(_, path, _)| path_to_string(path));
            let name = match &trait_path {
                Some(t) => format!("impl {t} for {self_ty}"),
                None => format!("impl {self_ty}"),
            };
            (
                ItemKind::Impl {
                    self_ty,
                    trait_path,
                },
                name,
            )
        }
        Item::Macro(i) => {
            let name = i
                .ident
                .as_ref()
                .map(|id| id.to_string())
                .unwrap_or_default();
            (ItemKind::Macro, name)
        }
        Item::Mod(i) => (ItemKind::Mod, i.ident.to_string()),
        Item::Static(i) => (ItemKind::Static, i.ident.to_string()),
        Item::Struct(i) => (ItemKind::Struct, i.ident.to_string()),
        Item::Trait(i) => (ItemKind::Trait, i.ident.to_string()),
        Item::TraitAlias(i) => (ItemKind::TraitAlias, i.ident.to_string()),
        Item::Type(i) => (ItemKind::TypeAlias, i.ident.to_string()),
        Item::Union(i) => (ItemKind::Union, i.ident.to_string()),
        Item::Use(_) => (ItemKind::Use, String::new()),
        Item::Verbatim(_) => (ItemKind::Verbatim, String::new()),
        other => panic!("unhandled syn::Item variant: {other:?}"),
    }
}

fn item_vis(item: &Item) -> ItemVis {
    let v = match item {
        Item::Const(i) => Some(&i.vis),
        Item::Enum(i) => Some(&i.vis),
        Item::ExternCrate(i) => Some(&i.vis),
        Item::Fn(i) => Some(&i.vis),
        Item::Static(i) => Some(&i.vis),
        Item::Struct(i) => Some(&i.vis),
        Item::Trait(i) => Some(&i.vis),
        Item::TraitAlias(i) => Some(&i.vis),
        Item::Type(i) => Some(&i.vis),
        Item::Union(i) => Some(&i.vis),
        Item::Use(i) => Some(&i.vis),
        Item::Mod(i) => Some(&i.vis),
        Item::ForeignMod(_) | Item::Impl(_) | Item::Macro(_) | Item::Verbatim(_) => None,
        other => panic!("unhandled syn::Item variant: {other:?}"),
    };
    match v {
        None => ItemVis::Private,
        Some(Visibility::Public(_)) => ItemVis::Public,
        Some(Visibility::Restricted(r)) => {
            let path = path_to_string(&r.path);
            match path.as_str() {
                "crate" => ItemVis::Crate,
                "super" => ItemVis::Super,
                _ => ItemVis::Restricted,
            }
        }
        Some(Visibility::Inherited) => ItemVis::Private,
    }
}

fn is_cfg_test_attr(attr: &Attribute) -> bool {
    if !attr.path().is_ident("cfg") {
        return false;
    }
    let mut hit = false;
    let _ = attr.parse_nested_meta(|meta| {
        if meta.path.is_ident("test") {
            hit = true;
        }
        Ok(())
    });
    hit
}

fn is_test_attr(attr: &Attribute) -> bool {
    attr.path().is_ident("test")
}

fn is_docify_export_attr(attr: &Attribute) -> bool {
    let segs: Vec<_> = attr.path().segments.iter().collect();
    segs.len() == 2
        && segs[0].ident == "docify"
        && segs[1].ident == "export"
}

fn type_to_anchor(ty: &Type) -> String {
    if let Type::Path(p) = ty
        && let Some(last) = p.path.segments.last()
    {
        return last.ident.to_string();
    }
    ty.to_token_stream().to_string()
}

fn path_to_string(p: &syn::Path) -> String {
    p.segments
        .iter()
        .map(|s| s.ident.to_string())
        .collect::<Vec<_>>()
        .join("::")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inline_module_with_inner_attr_keeps_mod_wrapper() {
        let src = r#"pub fn before() {}

mod security {
    #![allow(non_upper_case_globals)]
    use super::*;

    pub const errSecSuccess: i32 = 0;
}
"#;

        let items = parse_file(src).expect("parse");
        let module = items
            .iter()
            .find(|item| matches!(item.kind, ItemKind::Mod) && item.name == "security")
            .expect("security module");
        assert!(
            module.source.starts_with("mod security {"),
            "source should include the inline module wrapper, got:\n{}",
            module.source
        );
        assert!(
            module.source.contains("#![allow(non_upper_case_globals)]"),
            "source should preserve inner attrs, got:\n{}",
            module.source
        );
        assert!(
            module.source.ends_with('}'),
            "source should include the closing brace, got:\n{}",
            module.source
        );
    }
}
