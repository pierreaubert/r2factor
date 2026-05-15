use anyhow::{Context, Result};
use quote::ToTokens;
use syn::spanned::Spanned;
use syn::{Attribute, Item, Type, Visibility};

pub type ItemId = usize;

#[derive(Debug, Clone)]
pub enum ItemKind {
    Fn { is_test: bool },
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ItemVis {
    Public,
    Crate,
    Restricted,
    Private,
}

#[derive(Debug, Clone)]
pub struct ParsedItem {
    pub id: ItemId,
    pub kind: ItemKind,
    pub name: String,
    pub vis: ItemVis,
    pub is_cfg_test: bool,
    pub line_start: usize,
    pub line_end: usize,
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
    let line_start = attrs
        .iter()
        .map(|a| a.span().start().line)
        .min()
        .unwrap_or_else(|| item.span().start().line);
    let line_end = item.span().end().line;
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
            let trait_path = i
                .trait_
                .as_ref()
                .map(|(_, path, _)| path_to_string(path));
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
        Item::ForeignMod(_)
        | Item::Impl(_)
        | Item::Macro(_)
        | Item::Verbatim(_) => None,
        other => panic!("unhandled syn::Item variant: {other:?}"),
    };
    match v {
        None => ItemVis::Private,
        Some(Visibility::Public(_)) => ItemVis::Public,
        Some(Visibility::Restricted(r)) => {
            let path = path_to_string(&r.path);
            if path == "crate" {
                ItemVis::Crate
            } else {
                ItemVis::Restricted
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
