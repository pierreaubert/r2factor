//! Decide which private items need `pub(super)` so cross-bucket references
//! still resolve after the split. Without this lift, a private fn that was
//! visible to its callers (same scope) becomes invisible the moment those
//! callers move to a sibling sub-file.
//!
//! We don't lift to `pub` because the facade's `pub use mod::*;` would
//! then expose those items to the outside world. `pub(super)` keeps them
//! visible across sub-modules but invisible to the parent module's users.

use crate::item::{ItemId, ItemVis, ParsedItem};
use crate::plan::Plan;
use quote::ToTokens;
use std::collections::{BTreeMap, BTreeSet};

/// Extract verbatim `#[cfg(...)]` attribute source from an item's source
/// text. Used to gate cross-imports on the same cfg as the target item —
/// without this, a `use super::misc::linux_only;` against a function
/// defined `#[cfg(target_os = "linux")]` fails to resolve on every other
/// platform.
///
/// Returns each cfg attr rendered as its source representation (e.g.
/// `#[cfg(target_os = "linux")]`). Non-cfg attributes (`#[inline]`,
/// `#[derive(...)]`, doc comments) are ignored.
pub fn cfg_attrs_of(item_source: &str) -> Vec<String> {
    let Ok(file) = syn::parse_file(item_source) else {
        return Vec::new();
    };
    let Some(item) = file.items.first() else {
        return Vec::new();
    };
    let attrs = item_outer_attrs(item);
    attrs
        .iter()
        .filter(|a| a.path().is_ident("cfg"))
        .map(|a| a.to_token_stream().to_string())
        .collect()
}

fn item_outer_attrs(item: &syn::Item) -> &[syn::Attribute] {
    use syn::Item;
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
        _ => &[],
    }
}

/// "Facade buckets" don't materialize as a sub-file — `mod_root` items get
/// inlined into the facade's preamble, and the bucket whose name equals the
/// file stem holds the facade's primary items. Children of the facade
/// inherit access to facade-scope items by language rule, so anything that
/// lives there never needs a visibility lift.
fn is_facade_bucket(name: &str, stem: &str) -> bool {
    name == "mod_root" || name == stem
}

/// Precomputed lookups shared by [`compute_promotions`],
/// [`compute_cross_imports`], and [`compute_facade_imports`]. Built once
/// per `write_plan` invocation so each helper doesn't re-scan `items` and
/// `plan.assignments` from scratch. Fields are `pub(crate)` rather than
/// `pub` to keep the indices an implementation detail — callers outside
/// the crate should go through the helper functions.
pub struct RefContext<'a> {
    pub(crate) name_to_id: BTreeMap<&'a str, ItemId>,
    pub(crate) by_id: BTreeMap<ItemId, &'a ParsedItem>,
    pub(crate) bucket_of: BTreeMap<ItemId, &'a String>,
}

impl<'a> RefContext<'a> {
    pub fn new(plan: &'a Plan, items: &'a [ParsedItem]) -> Self {
        let name_to_id = items
            .iter()
            .filter(|i| !i.name.is_empty())
            .map(|i| (i.name.as_str(), i.id))
            .collect();
        let by_id = items.iter().map(|i| (i.id, i)).collect();
        let bucket_of = plan
            .assignments
            .iter()
            .flat_map(|(b, ids)| ids.iter().map(move |id| (*id, b)))
            .collect();
        Self {
            name_to_id,
            by_id,
            bucket_of,
        }
    }
}

/// IDs of items that should be re-emitted with `pub(super)` instead of their
/// original private visibility. We promote a target when:
///   * it lives in a real sub-bucket (facade-residing items already inherit
///     access to their children's scope so they don't need lifting),
///   * its current visibility is [`ItemVis::Private`],
///   * and it's referenced from a different bucket.
pub fn compute_promotions(
    ctx: &RefContext<'_>,
    items: &[ParsedItem],
    stem: &str,
) -> BTreeSet<ItemId> {
    let mut out: BTreeSet<ItemId> = BTreeSet::new();
    for it in items {
        let Some(my_bucket) = ctx.bucket_of.get(&it.id) else {
            continue;
        };
        for r in &it.refs {
            let Some(target_id) = ctx.name_to_id.get(r.as_str()) else {
                continue;
            };
            let Some(target_bucket) = ctx.bucket_of.get(target_id) else {
                continue;
            };
            if my_bucket == target_bucket {
                continue;
            }
            if is_facade_bucket(target_bucket, stem) {
                continue;
            }
            let target = ctx.by_id[target_id];
            if target.vis == ItemVis::Private && needs_keyword_rewrite(target) {
                out.insert(*target_id);
            }
        }
    }
    // cfg-variant siblings: `#[cfg(unix)] fn foo` and `#[cfg(not(unix))] fn foo`
    // are two ParsedItems sharing one name. `ctx.name_to_id` is a BTreeMap so
    // only one survives — meaning `compute_promotions` may have lifted only
    // one variant. Rustc picks whichever cfg-active variant exists at build
    // time, so a single-variant lift fails roughly half the time. Expand the
    // set so every variant of every promoted name is lifted in lockstep.
    let promoted_names: BTreeSet<&str> = out
        .iter()
        .filter_map(|id| ctx.by_id.get(id).map(|it| it.name.as_str()))
        .filter(|n| !n.is_empty())
        .collect();
    if !promoted_names.is_empty() {
        for it in items {
            if it.name.is_empty() || !promoted_names.contains(it.name.as_str()) {
                continue;
            }
            // Same eligibility gate as above so we don't accidentally
            // promote an item we wouldn't normally rewrite.
            if it.vis == ItemVis::Private && needs_keyword_rewrite(it) {
                out.insert(it.id);
            }
        }
    }
    out
}

/// For each sub-bucket A, the set of item names A needs to import from
/// sibling buckets or from the facade. This keeps generated sub-files from
/// relying on a blanket `use super::*;`, which in turn avoids dragging the
/// facade's entire public surface into every bucket.
/// A single cross-bucket `use` line to emit: source bucket, item name, and
/// any `#[cfg(...)]` attributes the use must carry so it stays in lockstep
/// with the target item's gating.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct CrossImport {
    /// `None` means the target item lives directly in the facade, so the
    /// import path is `super::<name>`. `Some(bucket)` means
    /// `super::<bucket>::<name>`.
    pub source_bucket: Option<String>,
    pub name: String,
    pub cfg_attrs: Vec<String>,
}

pub fn compute_cross_imports(
    ctx: &RefContext<'_>,
    items: &[ParsedItem],
    _promote: &BTreeSet<ItemId>,
    stem: &str,
) -> BTreeMap<String, BTreeSet<CrossImport>> {
    let mut out: BTreeMap<String, BTreeSet<CrossImport>> = BTreeMap::new();
    for it in items {
        let Some(my_bucket) = ctx.bucket_of.get(&it.id) else {
            continue;
        };
        if is_facade_bucket(my_bucket, stem) {
            continue;
        }
        for r in &it.refs {
            let Some(target_id) = ctx.name_to_id.get(r.as_str()) else {
                continue;
            };
            let Some(target_bucket) = ctx.bucket_of.get(target_id) else {
                continue;
            };
            if my_bucket == target_bucket {
                continue;
            }
            if *target_bucket == "mod_root" {
                continue;
            }
            let target = ctx.by_id[target_id];
            out.entry((*my_bucket).clone())
                .or_default()
                .insert(CrossImport {
                    source_bucket: (!is_facade_bucket(target_bucket, stem))
                        .then(|| (*target_bucket).clone()),
                    name: target.name.clone(),
                    cfg_attrs: cfg_attrs_of(&target.source),
                });
        }
    }
    out
}

/// IDs of inherent-impl blocks living in a sub-bucket. Each one gets its
/// associated items (`fn`, `const`, `type`) rewritten with `pub(super)` so
/// cross-bucket calls like `Type::method()` resolve.
///
/// We lift *all* inherent impls in sub-buckets, not just impls of promoted
/// types: a `pub struct` with a private `fn new` already trips E0624 once
/// callers move to a sibling bucket, even though the type itself is
/// already public. `pub(super)` is scoped to the facade module either way,
/// so the over-exposure is contained — same trade-off we already accepted
/// for top-level promotion.
///
/// Skipped:
///   * trait impls — trait methods inherit visibility from the trait
///     itself, and `pub(super)` on a trait method is a compile error.
///   * impls in facade buckets (mod_root / stem) — items at the facade's
///     scope are already visible to children, and lifting there would
///     leak methods one level above the facade.
pub fn compute_impl_lifts(
    ctx: &RefContext<'_>,
    items: &[ParsedItem],
    stem: &str,
) -> BTreeSet<ItemId> {
    use crate::item::ItemKind;
    items
        .iter()
        .filter_map(|it| {
            let bucket = ctx.bucket_of.get(&it.id)?;
            if is_facade_bucket(bucket, stem) {
                return None;
            }
            match &it.kind {
                ItemKind::Impl {
                    trait_path: None, ..
                } => Some(it.id),
                _ => None,
            }
        })
        .collect()
}

/// IDs of structs/unions in sub-buckets that aren't already covered by
/// [`compute_promotions`]. A `pub struct Foo { v: u32 }` keeps its `v`
/// field private after the split, but if a sibling sub-bucket reads
/// `foo.v` we get E0616 even though the type is reachable. Lift inherited
/// field visibility on every struct/union that lands in a sub-bucket — the
/// scope is `pub(super)`, so external API doesn't widen.
pub fn compute_field_lifts(
    ctx: &RefContext<'_>,
    items: &[ParsedItem],
    promote: &BTreeSet<ItemId>,
    stem: &str,
) -> BTreeSet<ItemId> {
    use crate::item::ItemKind;
    items
        .iter()
        .filter_map(|it| {
            let bucket = ctx.bucket_of.get(&it.id)?;
            if is_facade_bucket(bucket, stem) {
                return None;
            }
            // Items in `promote` get field-lifting via `add_pub_super`
            // already; avoid double-rewrites.
            if promote.contains(&it.id) {
                return None;
            }
            match it.kind {
                ItemKind::Struct | ItemKind::Union => Some(it.id),
                _ => None,
            }
        })
        .collect()
}

/// Field-lift only — same byte-splice mechanics as [`add_pub_super`] but
/// without the leading-keyword splice. Used for structs/unions that
/// aren't in the promote set (their *type* visibility is already enough,
/// but their fields need to widen).
pub fn add_pub_super_to_fields(source: &str) -> String {
    let Ok(file) = syn::parse_file(source) else {
        return source.to_string();
    };
    let Some(item) = file.items.first() else {
        return source.to_string();
    };
    let mut points: Vec<usize> = Vec::new();
    collect_field_insertion_points(item, source, &mut points);
    if points.is_empty() {
        return source.to_string();
    }
    points.sort_unstable();
    points.dedup();
    let mut out = source.to_string();
    for &p in points.iter().rev() {
        if p > out.len() {
            return source.to_string();
        }
        out.insert_str(p, "pub(super) ");
    }
    out
}

/// Walk every code path in `source` and add another `super::` in front of
/// any path whose first segment is `super`. This handles the body-code
/// counterpart of the use-prelude rebase: the original file's
/// `super::foo()` (calling a sibling at the file's parent level) needs
/// to become `super::super::foo()` from inside a sub-file because the
/// sub-file is one module-level deeper.
///
/// Skipped:
///   * paths inside `Visibility::Restricted` (the `super` in `pub(super)` /
///     `pub(in super::super)` is *visibility scope*, not a code reference,
///     and is already handled by [`rebase_pub_super_in_subfile`]),
///   * paths inside `use` items (those are handled by
///     [`crate::write::uses::rebase_use_for_subfile`]).
///
/// We splice in reverse byte order so earlier offsets don't shift as we
/// extend.
pub fn rebase_body_super_for_subfile(source: &str) -> String {
    let Ok(file) = syn::parse_file(source) else {
        return source.to_string();
    };
    let mut v = BodyPathRebaser::default();
    syn::visit::visit_file(&mut v, &file);
    if v.points.is_empty() {
        return source.to_string();
    }
    let mut offsets: Vec<usize> = v
        .points
        .iter()
        .filter_map(|(l, c)| line_col_to_byte_offset(source, *l, *c))
        .collect();
    offsets.sort_unstable();
    offsets.dedup();
    let mut out = source.to_string();
    for &p in offsets.iter().rev() {
        if p > out.len() {
            return source.to_string();
        }
        out.insert_str(p, "super::");
    }
    out
}

#[derive(Default)]
struct BodyPathRebaser {
    points: Vec<(usize, usize)>,
}

impl<'ast> syn::visit::Visit<'ast> for BodyPathRebaser {
    /// Don't descend into visibility annotations — their inner paths
    /// (`pub(in super::super)` etc.) are scope qualifiers, not code refs.
    fn visit_visibility(&mut self, _: &'ast syn::Visibility) {}

    fn visit_item_use(&mut self, item: &'ast syn::ItemUse) {
        // A `use super::X;` inside a function body sits at fn scope, not
        // module scope, but its path resolves the same way as a
        // module-level use. From a sub-file the original `super` now
        // points at the facade, so we need to prepend another `super::`
        // for the path to reach what the user wrote (the facade's
        // parent). The use-prelude rebase only handles MODULE-level uses
        // because it operates on the prelude string; body-level uses
        // need this hook.
        record_leading_super(&item.tree, &mut self.points);
    }

    fn visit_path(&mut self, p: &'ast syn::Path) {
        if let Some(first) = p.segments.first()
            && first.ident == "super"
        {
            let start = first.ident.span().start();
            self.points.push((start.line, start.column));
        }
        // Continue descent so generics / sub-paths inside this one are
        // also processed (e.g. `Foo<super::Bar>`).
        syn::visit::visit_path(self, p);
    }
}

/// Walk a `UseTree` looking for a leading `super` segment. Records the
/// position of that `super` so the surrounding splice loop can insert
/// another `super::` in front of it. Groups recurse — `use { super::a,
/// super::b };` is rare but legal.
fn record_leading_super(tree: &syn::UseTree, points: &mut Vec<(usize, usize)>) {
    use syn::UseTree;
    match tree {
        UseTree::Path(p) if p.ident == "super" => {
            let start = p.ident.span().start();
            points.push((start.line, start.column));
        }
        UseTree::Group(g) => {
            for inner in &g.items {
                record_leading_super(inner, points);
            }
        }
        _ => {}
    }
}

/// Sub-bucket items whose original visibility was `pub(super)` need a
/// visibility *widen* (not lift) when emitted into a sub-bucket: from the
/// sub-bucket's perspective the facade is `super`, but the original
/// `pub(super)` was relative to the original file's super (= facade's
/// parent). Rewriting to `pub(in super::super)` restores the original
/// effective scope exactly.
///
/// Returns the rewritten source, or the input unchanged if syn can't parse
/// it or the item doesn't actually carry `pub(super)`.
pub fn rebase_pub_super_in_subfile(source: &str) -> String {
    let Ok(file) = syn::parse_file(source) else {
        return source.to_string();
    };
    let Some(item) = file.items.first() else {
        return source.to_string();
    };
    let vis = item_vis(item);
    let Some(vis) = vis else {
        return source.to_string();
    };
    let syn::Visibility::Restricted(r) = vis else {
        return source.to_string();
    };
    // We only specialize the bare `pub(super)` form. `pub(in super)` parses
    // as Restricted with the same single-segment path; that's fine — same
    // rewrite. Any longer path we leave alone (could be `pub(in crate::x)`
    // which is already absolute, or `pub(in super::y)` which would need a
    // different rebase rule).
    let path_segments: Vec<String> = r
        .path
        .segments
        .iter()
        .map(|s| s.ident.to_string())
        .collect();
    if path_segments != ["super"] {
        return source.to_string();
    }
    // Find the byte span of the `super` token inside the `pub(...)` and
    // splice in an extra `super::` before it.
    let super_ident_span = r
        .path
        .segments
        .first()
        .expect("checked length above")
        .ident
        .span();
    let start = super_ident_span.start();
    let Some(pos) = line_col_to_byte_offset(source, start.line, start.column) else {
        return source.to_string();
    };
    // `pub(super)` -> `pub(in super::super)`. We need to add `in ` before
    // the existing `super` AND another `super::` immediately after. Doing
    // both in one splice keeps the byte offset unambiguous.
    let mut out = String::with_capacity(source.len() + "in super::".len());
    out.push_str(&source[..pos]);
    out.push_str("in super::");
    out.push_str(&source[pos..]);
    out
}

/// Pulled out so [`rebase_pub_super_in_subfile`] can read the visibility of
/// any item kind without re-implementing the giant match.
fn item_vis(item: &syn::Item) -> Option<&syn::Visibility> {
    use syn::Item;
    match item {
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
        _ => None,
    }
}

/// Rewrite an inherent-impl block so its inherited-vis associated items
/// carry `pub(super)`. Used by the renderer for any impl in
/// [`compute_impl_lifts`]. Trait impls are passed through unchanged
/// because the language doesn't allow per-method visibility there.
pub fn lift_impl_methods(source: &str) -> String {
    let Ok(parsed) = syn::parse_str::<syn::Item>(source) else {
        return source.to_string();
    };
    let syn::Item::Impl(item_impl) = parsed else {
        return source.to_string();
    };
    if item_impl.trait_.is_some() {
        return source.to_string();
    }

    let mut points: Vec<usize> = Vec::new();
    for impl_item in &item_impl.items {
        let Some(span) = impl_item_keyword_span(impl_item) else {
            continue;
        };
        let start = span.start();
        if let Some(pos) = line_col_to_byte_offset(source, start.line, start.column) {
            points.push(pos);
        }
    }
    if points.is_empty() {
        return source.to_string();
    }
    points.sort_unstable();
    points.dedup();
    let mut out = source.to_string();
    for &p in points.iter().rev() {
        if p > out.len() {
            return source.to_string();
        }
        out.insert_str(p, "pub(super) ");
    }
    out
}

/// Where to splice `pub(super) ` for an inherent-impl associated item with
/// inherited visibility. Returns `None` for items that already carry an
/// explicit visibility, or for kinds we don't rewrite (macros, verbatim).
fn impl_item_keyword_span(item: &syn::ImplItem) -> Option<proc_macro2::Span> {
    use syn::ImplItem;
    match item {
        ImplItem::Fn(f) if matches!(f.vis, syn::Visibility::Inherited) => {
            Some(fn_keyword_span(&f.sig))
        }
        ImplItem::Const(c) if matches!(c.vis, syn::Visibility::Inherited) => {
            Some(c.const_token.span)
        }
        ImplItem::Type(t) if matches!(t.vis, syn::Visibility::Inherited) => Some(t.type_token.span),
        _ => None,
    }
}

/// Imports the facade itself needs: promoted items in sub-buckets referenced
/// from the facade's primary items or `mod_root`. The facade renders these
/// as `use <source_bucket>::<item_name>;` (no `super::` — the facade IS the
/// parent of every sub-bucket).
pub fn compute_facade_imports(
    ctx: &RefContext<'_>,
    items: &[ParsedItem],
    promote: &BTreeSet<ItemId>,
    stem: &str,
) -> BTreeSet<CrossImport> {
    let mut out: BTreeSet<CrossImport> = BTreeSet::new();
    for it in items {
        let Some(my_bucket) = ctx.bucket_of.get(&it.id) else {
            continue;
        };
        if !is_facade_bucket(my_bucket, stem) {
            continue;
        }
        for r in &it.refs {
            let Some(target_id) = ctx.name_to_id.get(r.as_str()) else {
                continue;
            };
            if !promote.contains(target_id) {
                continue;
            }
            let Some(target_bucket) = ctx.bucket_of.get(target_id) else {
                continue;
            };
            if is_facade_bucket(target_bucket, stem) {
                continue;
            }
            let target = ctx.by_id[target_id];
            out.insert(CrossImport {
                source_bucket: Some((*target_bucket).clone()),
                name: target.name.clone(),
                cfg_attrs: cfg_attrs_of(&target.source),
            });
        }
    }
    out
}

/// Items whose source starts with a visibility-friendly keyword we know how
/// to prefix. Impls/macros/use/extern blocks don't take visibility, so we
/// skip them — even if a private "impl" target is referenced cross-bucket,
/// the issue is whoever wrote the impl, not visibility.
fn needs_keyword_rewrite(it: &ParsedItem) -> bool {
    use crate::item::ItemKind as K;
    matches!(
        it.kind,
        K::Fn { .. }
            | K::Struct
            | K::Enum
            | K::Union
            | K::Trait
            | K::TraitAlias
            | K::Const
            | K::Static
            | K::TypeAlias
            | K::Mod
    )
}

/// Rewrite the first item-keyword token in `source` so the item carries a
/// `pub(super)` visibility. Attributes, doc comments, and indentation above
/// the item are preserved verbatim.
///
/// Driven by `syn`'s token spans rather than line prefixes so that
/// multi-line attributes — e.g.
///
/// ```ignore
/// #[allow(
///     dead_code,
/// )]
/// fn helper() {}
/// ```
///
/// — don't get mis-parsed (a previous line-prefix implementation would have
/// treated `    dead_code,` as the item-keyword line and produced
/// `pub(super) dead_code,`, which is not valid Rust).
///
/// Falls back to a conservative line-based insertion only if syn parsing
/// fails, which shouldn't happen since promoted items came through
/// `parse_file` originally.
pub fn add_pub_super(source: &str) -> String {
    if let Some(out) = add_pub_super_via_syn(source) {
        return out;
    }
    add_pub_super_line_based(source)
}

fn add_pub_super_via_syn(source: &str) -> Option<String> {
    let file = syn::parse_file(source).ok()?;
    let item = file.items.first()?;

    // Collect every position where `pub(super) ` needs to be inserted: the
    // item-keyword itself plus, for struct/union items, each inherited-vis
    // field. Without lifting fields a private struct that gets promoted
    // works for *type* references cross-bucket but fails the moment a
    // sibling reads `instance.field` — Rust enforces field visibility
    // per-module independently of the type's visibility.
    let mut points: Vec<usize> = Vec::new();

    let kw_span = item_keyword_span(item)?;
    let kw_start = kw_span.start();
    let kw_pos = line_col_to_byte_offset(source, kw_start.line, kw_start.column)?;
    points.push(kw_pos);

    collect_field_insertion_points(item, source, &mut points);

    // Splice in reverse so earlier byte offsets stay valid as we extend.
    points.sort_unstable();
    points.dedup();
    let mut out = source.to_string();
    for &p in points.iter().rev() {
        if p > out.len() {
            return None;
        }
        out.insert_str(p, "pub(super) ");
    }
    Some(out)
}

/// For struct/union items, find each inherited-visibility field and record
/// the byte offset where a `pub(super) ` prefix should go. Enums are
/// intentionally skipped — struct-style variant fields inherit visibility
/// from the variant, which inherits from the enum itself, so once the
/// outer `enum` is `pub(super)` everything inside follows.
fn collect_field_insertion_points(item: &syn::Item, source: &str, points: &mut Vec<usize>) {
    use syn::Item;
    match item {
        Item::Struct(s) => collect_from_fields(&s.fields, source, points),
        Item::Union(u) => collect_from_named(&u.fields, source, points),
        _ => {}
    }
}

fn collect_from_fields(fields: &syn::Fields, source: &str, points: &mut Vec<usize>) {
    match fields {
        syn::Fields::Named(named) => collect_from_named(named, source, points),
        syn::Fields::Unnamed(unnamed) => collect_from_unnamed(unnamed, source, points),
        syn::Fields::Unit => {}
    }
}

fn collect_from_named(named: &syn::FieldsNamed, source: &str, points: &mut Vec<usize>) {
    for field in &named.named {
        if !matches!(field.vis, syn::Visibility::Inherited) {
            continue;
        }
        let Some(ident) = &field.ident else {
            continue;
        };
        let span = ident.span();
        if let Some(pos) = line_col_to_byte_offset(source, span.start().line, span.start().column) {
            points.push(pos);
        }
    }
}

fn collect_from_unnamed(unnamed: &syn::FieldsUnnamed, source: &str, points: &mut Vec<usize>) {
    use syn::spanned::Spanned;
    for field in &unnamed.unnamed {
        if !matches!(field.vis, syn::Visibility::Inherited) {
            continue;
        }
        // For tuple fields the syntax is `vis? ty` (no ident) — splice
        // before the type.
        let span = field.ty.span();
        if let Some(pos) = line_col_to_byte_offset(source, span.start().line, span.start().column) {
            points.push(pos);
        }
    }
}

/// Span of the first non-visibility, non-attribute token of an item — the
/// position where a `pub(super) ` prefix needs to land. For fn/trait/mod we
/// have to skip past `const`/`async`/`unsafe`/`auto`/`extern` modifiers,
/// which all sit between the visibility slot and the kind keyword.
fn item_keyword_span(item: &syn::Item) -> Option<proc_macro2::Span> {
    use syn::Item;
    match item {
        Item::Fn(i) => Some(fn_keyword_span(&i.sig)),
        Item::Struct(i) => Some(i.struct_token.span),
        Item::Enum(i) => Some(i.enum_token.span),
        Item::Union(i) => Some(i.union_token.span),
        Item::Trait(i) => Some(trait_keyword_span(i)),
        Item::TraitAlias(i) => Some(i.trait_token.span),
        Item::Const(i) => Some(i.const_token.span),
        Item::Static(i) => Some(i.static_token.span),
        Item::Type(i) => Some(i.type_token.span),
        Item::Mod(i) => Some(mod_keyword_span(i)),
        _ => None,
    }
}

fn fn_keyword_span(sig: &syn::Signature) -> proc_macro2::Span {
    if let Some(c) = &sig.constness {
        return c.span;
    }
    if let Some(a) = &sig.asyncness {
        return a.span;
    }
    if let Some(u) = &sig.unsafety {
        return u.span;
    }
    if let Some(abi) = &sig.abi {
        return abi.extern_token.span;
    }
    sig.fn_token.span
}

fn trait_keyword_span(t: &syn::ItemTrait) -> proc_macro2::Span {
    if let Some(u) = &t.unsafety {
        return u.span;
    }
    if let Some(at) = &t.auto_token {
        return at.span;
    }
    t.trait_token.span
}

fn mod_keyword_span(m: &syn::ItemMod) -> proc_macro2::Span {
    if let Some(u) = &m.unsafety {
        return u.span;
    }
    m.mod_token.span
}

/// proc-macro2 reports 1-indexed line numbers and 0-indexed byte columns when
/// the `span-locations` feature is enabled (which Cargo.toml does). Walk the
/// source line-by-line, return the absolute byte offset that matches.
///
/// IMPORTANT: `column` is a **byte** offset within the line, not a char
/// count. Don't replace the arithmetic below with `chars().take(column)` —
/// for any non-ASCII content on the same line that would compute a
/// different position than proc-macro2 reports.
pub(crate) fn line_col_to_byte_offset(src: &str, line: usize, column: usize) -> Option<usize> {
    if line == 0 {
        return None;
    }
    let target_line = line - 1;
    let mut byte = 0usize;
    for (idx, l) in src.split('\n').enumerate() {
        if idx == target_line {
            if column > l.len() {
                return None;
            }
            return Some(byte + column);
        }
        byte += l.len() + 1; // +1 for the '\n' consumed by split
    }
    None
}

/// Fallback used only when `syn::parse_file` rejects the source (shouldn't
/// happen on items r2factor itself emits). Skips attribute / doc / comment /
/// blank lines and inserts `pub(super) ` before the first remaining line.
/// Not robust to multi-line attributes — that's why it's the fallback.
fn add_pub_super_line_based(source: &str) -> String {
    let mut out = String::new();
    let mut promoted = false;
    let lines: Vec<&str> = source.lines().collect();
    for (i, line) in lines.iter().enumerate() {
        let last = i + 1 == lines.len();
        if promoted || is_attr_or_blank(line) {
            out.push_str(line);
            if !last {
                out.push('\n');
            }
            continue;
        }
        let indent_end = line.len() - line.trim_start().len();
        out.push_str(&line[..indent_end]);
        out.push_str("pub(super) ");
        out.push_str(line[indent_end..].trim_start());
        if !last {
            out.push('\n');
        }
        promoted = true;
    }
    out
}

fn is_attr_or_blank(line: &str) -> bool {
    let t = line.trim_start();
    t.is_empty()
        || t.starts_with('#')
        || t.starts_with("///")
        || t.starts_with("//!")
        || t.starts_with("//")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::item::{ItemKind, ItemVis};

    // Build a minimal ParsedItem with controllable visibility, kind, and
    // refs. `line_start = id + 1` keeps line-order deterministic.
    fn fake(id: ItemId, name: &str, vis: ItemVis, kind: ItemKind, refs: &[&str]) -> ParsedItem {
        ParsedItem {
            id,
            kind,
            name: name.to_string(),
            vis,
            is_cfg_test: false,
            line_start: id + 1,
            line_end: id + 1,
            source: String::new(),
            refs: refs.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn cross_import_carries_cfg_attrs() {
        // A cfg-only target (only one variant, no sibling): the use line
        // MUST be cfg-gated, otherwise on non-matching platforms the use
        // tries to resolve a symbol that doesn't exist (E0432).
        let mut linux_only = fake(
            0,
            "linux_thing",
            ItemVis::Private,
            ItemKind::Fn { is_test: false },
            &[],
        );
        linux_only.source = "#[cfg(target_os = \"linux\")]\nfn linux_thing() {}".to_string();
        let consumer = fake(
            1,
            "uses_it",
            ItemVis::Public,
            ItemKind::Fn { is_test: false },
            &["linux_thing"],
        );
        let items = vec![linux_only, consumer];
        let mut plan = Plan::default();
        plan.assign("misc", 0, "");
        plan.assign("caller", 1, "");

        let ctx = RefContext::new(&plan, &items);
        let promote = compute_promotions(&ctx, &items, "file");
        let cross = compute_cross_imports(&ctx, &items, &promote, "file");
        let caller = cross.get("caller").expect("caller needs imports");
        let import = caller
            .iter()
            .find(|c| c.name == "linux_thing")
            .expect("linux_thing import present");
        assert!(
            import
                .cfg_attrs
                .iter()
                .any(|s| s.contains("target_os") && s.contains("linux")),
            "use line must carry the cfg(target_os = \"linux\") attr; got {:?}",
            import.cfg_attrs
        );
    }

    #[test]
    fn promotion_expands_to_cfg_variant_siblings() {
        // Two ParsedItems with the same name (cfg variants). Without the
        // sibling expansion, only the last one wins in `name_to_id` and
        // the other stays private — rustc picks the cfg-active variant
        // at build time and randomly hits a private one.
        let items = vec![
            fake(
                0,
                "unix_id",
                ItemVis::Private,
                ItemKind::Fn { is_test: false },
                &[],
            ),
            fake(
                1,
                "unix_id",
                ItemVis::Private,
                ItemKind::Fn { is_test: false },
                &[],
            ),
            fake(
                2,
                "consumer",
                ItemVis::Public,
                ItemKind::Fn { is_test: false },
                &["unix_id"],
            ),
        ];
        let mut plan = Plan::default();
        plan.assign("unix", 0, "");
        plan.assign("unix", 1, "");
        plan.assign("prim", 2, "");

        let ctx = RefContext::new(&plan, &items);
        let promote = compute_promotions(&ctx, &items, "file");
        assert!(promote.contains(&0), "cfg variant 0 must be promoted");
        assert!(promote.contains(&1), "cfg variant 1 must be promoted");
    }

    #[test]
    fn cross_bucket_private_ref_triggers_promotion_and_import() {
        // File stem is `sample`. `parser` (sub-bucket) calls `helper`
        // (private fn in `eval` sub-bucket). Expect: helper is in the
        // promote set, parser's import set contains ("eval", "helper").
        let items = vec![
            fake(
                0,
                "helper",
                ItemVis::Private,
                ItemKind::Fn { is_test: false },
                &[],
            ),
            fake(
                1,
                "parse",
                ItemVis::Public,
                ItemKind::Fn { is_test: false },
                &["helper"],
            ),
        ];
        let mut plan = Plan::default();
        plan.assign("eval", 0, "");
        plan.assign("parser", 1, "");

        let ctx = RefContext::new(&plan, &items);
        let promote = compute_promotions(&ctx, &items, "sample");
        assert!(promote.contains(&0), "helper should be promoted");

        let cross = compute_cross_imports(&ctx, &items, &promote, "sample");
        let parser_imports = cross.get("parser").expect("parser needs imports");
        assert!(
            parser_imports
                .iter()
                .any(|c| c.source_bucket.as_deref() == Some("eval")
                    && c.name == "helper"
                    && c.cfg_attrs.is_empty()),
            "parser should import super::eval::helper with no cfg attrs"
        );
    }

    #[test]
    fn facade_residing_target_is_not_promoted() {
        // `Helper` lives in mod_root (facade). Children inherit access, so
        // even though `parser` references it cross-bucket, we don't promote.
        let items = vec![
            fake(0, "Helper", ItemVis::Private, ItemKind::Struct, &[]),
            fake(
                1,
                "parse",
                ItemVis::Public,
                ItemKind::Fn { is_test: false },
                &["Helper"],
            ),
        ];
        let mut plan = Plan::default();
        plan.assign("mod_root", 0, "");
        plan.assign("parser", 1, "");

        let ctx = RefContext::new(&plan, &items);
        let promote = compute_promotions(&ctx, &items, "sample");
        assert!(
            promote.is_empty(),
            "facade-residing items shouldn't be promoted"
        );
    }

    #[test]
    fn facade_primary_referencing_sub_bucket_promoted_item_gets_import() {
        // Stem == "sample"; the `sample`-named bucket holds the file's
        // primary type `Sample`. Sample's source touches a private helper
        // in the `eval` sub-bucket. Expect: helper promoted, and
        // compute_facade_imports surfaces ("eval", "helper").
        let items = vec![
            fake(
                0,
                "helper",
                ItemVis::Private,
                ItemKind::Fn { is_test: false },
                &[],
            ),
            fake(1, "Sample", ItemVis::Public, ItemKind::Struct, &["helper"]),
        ];
        let mut plan = Plan::default();
        plan.assign("eval", 0, "");
        plan.assign("sample", 1, "");

        let ctx = RefContext::new(&plan, &items);
        let promote = compute_promotions(&ctx, &items, "sample");
        assert!(promote.contains(&0));

        let facade_imports = compute_facade_imports(&ctx, &items, &promote, "sample");
        assert!(
            facade_imports
                .iter()
                .any(|c| c.source_bucket.as_deref() == Some("eval") && c.name == "helper"),
            "facade should import eval::helper"
        );

        // And the sub-side cross-imports should NOT list anything for the
        // facade bucket — it has no sub-file.
        let cross = compute_cross_imports(&ctx, &items, &promote, "sample");
        assert!(!cross.contains_key("sample"));
        assert!(!cross.contains_key("mod_root"));
    }

    #[test]
    fn promotes_after_attrs_and_docs() {
        let src = "/// doc\n#[inline]\nfn helper() {}";
        let out = add_pub_super(src);
        assert_eq!(out, "/// doc\n#[inline]\npub(super) fn helper() {}");
    }

    #[test]
    fn promotes_preserves_indent() {
        let src = "    fn helper() {}";
        let out = add_pub_super(src);
        assert_eq!(out, "    pub(super) fn helper() {}");
    }

    #[test]
    fn promotes_only_first_keyword_line() {
        let src = "fn helper() {\n    fn nested() {}\n}";
        let out = add_pub_super(src);
        assert_eq!(out, "pub(super) fn helper() {\n    fn nested() {}\n}");
    }

    // The visibility must precede item modifiers like `unsafe`, `async`,
    // `const`, `extern`. These tests pin the ordering — Rust's grammar
    // requires `ItemVisibility ItemModifiers ItemKind`, so prepending
    // `pub(super) ` to a line that starts with a modifier is correct.

    #[test]
    fn promotes_unsafe_fn() {
        let src = "unsafe fn raw_helper() {}";
        assert_eq!(add_pub_super(src), "pub(super) unsafe fn raw_helper() {}");
    }

    #[test]
    fn promotes_async_fn() {
        let src = "async fn fetch() {}";
        assert_eq!(add_pub_super(src), "pub(super) async fn fetch() {}");
    }

    #[test]
    fn promotes_const_fn() {
        let src = "const fn doubled(x: u32) -> u32 { x * 2 }";
        assert_eq!(
            add_pub_super(src),
            "pub(super) const fn doubled(x: u32) -> u32 { x * 2 }"
        );
    }

    #[test]
    fn promotes_extern_fn() {
        let src = "extern \"C\" fn cb() {}";
        assert_eq!(add_pub_super(src), "pub(super) extern \"C\" fn cb() {}");
    }

    #[test]
    fn promotes_unsafe_trait() {
        let src = "unsafe trait Marker {}";
        assert_eq!(add_pub_super(src), "pub(super) unsafe trait Marker {}");
    }

    #[test]
    fn promotes_after_inner_line_comment() {
        // A regular `// ...` line between attrs and the keyword should be
        // preserved verbatim; we still find the keyword on the line below.
        let src = "#[inline]\n// note: hot path\nfn helper() {}";
        assert_eq!(
            add_pub_super(src),
            "#[inline]\n// note: hot path\npub(super) fn helper() {}"
        );
    }

    #[test]
    fn promotes_struct_lifts_named_field_visibility() {
        // Without field-vis lifting a sibling bucket can `use Config` but
        // can't read `cfg.v` — Rust enforces field visibility independently
        // of type visibility. The e2e compile-check caught this; lock it in
        // here too.
        let src = "struct Config { v: u32 }";
        let out = add_pub_super(src);
        assert_eq!(out, "pub(super) struct Config { pub(super) v: u32 }");
        syn::parse_file(&out).expect("lifted struct must parse");
    }

    #[test]
    fn promotes_struct_only_lifts_inherited_fields() {
        // Don't double-prefix a field that's already pub or pub(crate).
        let src = "struct Mixed { pub a: u32, b: u32, pub(crate) c: u32 }";
        let out = add_pub_super(src);
        assert_eq!(
            out,
            "pub(super) struct Mixed { pub a: u32, pub(super) b: u32, pub(crate) c: u32 }"
        );
        syn::parse_file(&out).expect("mixed struct must parse");
    }

    #[test]
    fn promotes_struct_lifts_all_fields_across_multiple_lines() {
        let src = "struct Wide {\n    a: u32,\n    b: String,\n}";
        let out = add_pub_super(src);
        assert_eq!(
            out,
            "pub(super) struct Wide {\n    pub(super) a: u32,\n    pub(super) b: String,\n}"
        );
        syn::parse_file(&out).expect("multi-line struct must parse");
    }

    #[test]
    fn promotes_tuple_struct_lifts_field_types() {
        let src = "struct Tagged(u32, String);";
        let out = add_pub_super(src);
        assert_eq!(
            out,
            "pub(super) struct Tagged(pub(super) u32, pub(super) String);"
        );
        syn::parse_file(&out).expect("tuple struct must parse");
    }

    #[test]
    fn promotes_unit_struct_has_no_field_inserts() {
        let src = "struct Marker;";
        let out = add_pub_super(src);
        assert_eq!(out, "pub(super) struct Marker;");
    }

    #[test]
    fn promotes_enum_does_not_lift_variant_fields() {
        // Enum variants inherit visibility from the enum itself, so a
        // `pub(super) enum E` already makes inner fields reachable; we
        // must NOT add per-field annotations (which is invalid syntax on
        // an enum variant struct-field).
        let src = "enum E { A { x: u32 }, B(u32) }";
        let out = add_pub_super(src);
        assert_eq!(out, "pub(super) enum E { A { x: u32 }, B(u32) }");
        syn::parse_file(&out).expect("enum must parse");
    }

    #[test]
    fn promotes_union_lifts_named_fields() {
        let src = "union U { a: u32, b: f32 }";
        let out = add_pub_super(src);
        assert_eq!(
            out,
            "pub(super) union U { pub(super) a: u32, pub(super) b: f32 }"
        );
        syn::parse_file(&out).expect("union must parse");
    }

    #[test]
    fn rebase_body_super_call_expr() {
        let src = "fn f() { super::foo(); }";
        assert_eq!(
            rebase_body_super_for_subfile(src),
            "fn f() { super::super::foo(); }"
        );
    }

    #[test]
    fn rebase_body_super_path_in_type() {
        let src = "fn f() -> super::T { super::make() }";
        assert_eq!(
            rebase_body_super_for_subfile(src),
            "fn f() -> super::super::T { super::super::make() }"
        );
    }

    #[test]
    fn rebase_body_super_in_generic_arg() {
        let src = "fn f() -> Vec<super::T> { Vec::new() }";
        assert_eq!(
            rebase_body_super_for_subfile(src),
            "fn f() -> Vec<super::super::T> { Vec::new() }"
        );
    }

    #[test]
    fn rebase_body_super_in_body_level_use() {
        // Real failure from the sonium2 sweep: `use super::X;` inside an
        // inline test fn. The use's tree is `super::X` and from a sub-file
        // that no longer resolves to the original parent — it resolves to
        // the facade. The rebase has to find the leading `super` in the
        // UseTree (not via visit_path) and insert another `super::`.
        let src = "fn outer() { use super::time_stepping::Coeffs; let _ = Coeffs; }";
        assert_eq!(
            rebase_body_super_for_subfile(src),
            "fn outer() { use super::super::time_stepping::Coeffs; let _ = Coeffs; }"
        );
    }

    #[test]
    fn rebase_body_super_skips_visibility() {
        // The `super` inside `pub(super)` is a visibility scope, not a
        // code reference. The body-rebaser must leave it alone — the vis
        // is widened separately by `rebase_pub_super_in_subfile`.
        let src = "pub(super) fn f() { super::foo(); }";
        assert_eq!(
            rebase_body_super_for_subfile(src),
            "pub(super) fn f() { super::super::foo(); }"
        );
    }

    #[test]
    fn rebase_body_super_use_items_in_same_chunk() {
        // The body-rebaser now visits ItemUse nodes too (to catch
        // body-level uses). When given a file with a module-level use
        // alongside a fn, both get rebased — which is fine because the
        // pipeline never feeds top-level uses *and* fn items as one
        // chunk: in render_sub_file the bucket emits each non-use item
        // separately, and the use prelude is generated upstream. This
        // test pins the function's behavior on a synthetic combined
        // input so future refactors of the visitor don't silently break
        // the body-level case.
        let src = "use super::X;\nfn f() { super::y(); }";
        assert_eq!(
            rebase_body_super_for_subfile(src),
            "use super::super::X;\nfn f() { super::super::y(); }"
        );
    }

    #[test]
    fn rebase_body_super_no_op_when_no_super() {
        let src = "fn f() { let x = crate::foo::bar(); }";
        assert_eq!(rebase_body_super_for_subfile(src), src);
    }

    #[test]
    fn rebase_body_super_multiple_paths_same_line() {
        // Reverse-order splice safety: two `super::` on the same line
        // must both get rewritten without corrupting offsets.
        let src = "fn f() { super::a(); super::b(); }";
        assert_eq!(
            rebase_body_super_for_subfile(src),
            "fn f() { super::super::a(); super::super::b(); }"
        );
    }

    #[test]
    fn rebase_pub_super_widens_to_in_super_super() {
        let src = "pub(super) fn helper() {}";
        assert_eq!(
            rebase_pub_super_in_subfile(src),
            "pub(in super::super) fn helper() {}"
        );
        syn::parse_file(&rebase_pub_super_in_subfile(src)).expect("must parse");
    }

    #[test]
    fn rebase_pub_super_struct_with_fields() {
        let src = "pub(super) struct S { v: u32 }";
        assert_eq!(
            rebase_pub_super_in_subfile(src),
            "pub(in super::super) struct S { v: u32 }"
        );
    }

    #[test]
    fn rebase_pub_super_no_op_for_pub() {
        assert_eq!(
            rebase_pub_super_in_subfile("pub fn x() {}"),
            "pub fn x() {}"
        );
    }

    #[test]
    fn rebase_pub_super_no_op_for_pub_crate() {
        assert_eq!(
            rebase_pub_super_in_subfile("pub(crate) fn x() {}"),
            "pub(crate) fn x() {}"
        );
    }

    #[test]
    fn rebase_pub_super_no_op_for_private() {
        assert_eq!(rebase_pub_super_in_subfile("fn x() {}"), "fn x() {}");
    }

    #[test]
    fn lift_impl_methods_inherent() {
        let src = "impl Foo {\n    fn helper() {}\n    pub fn already_pub() {}\n}";
        let out = lift_impl_methods(src);
        assert_eq!(
            out,
            "impl Foo {\n    pub(super) fn helper() {}\n    pub fn already_pub() {}\n}"
        );
        syn::parse_file(&out).expect("lifted impl must parse");
    }

    #[test]
    fn lift_impl_methods_const_and_type() {
        let src = "impl Foo {\n    const C: u32 = 0;\n    type T = u32;\n    fn f() {}\n}";
        let out = lift_impl_methods(src);
        assert_eq!(
            out,
            "impl Foo {\n    pub(super) const C: u32 = 0;\n    pub(super) type T = u32;\n    pub(super) fn f() {}\n}"
        );
        syn::parse_file(&out).expect("lifted impl items must parse");
    }

    #[test]
    fn lift_impl_methods_trait_impl_left_alone() {
        // `pub(super) fn` on a trait method is a compile error — visibility
        // is determined by the trait. So trait impls must pass through
        // even if the type was promoted.
        let src = "impl Iterator for Foo {\n    type Item = u32;\n    fn next(&mut self) -> Option<u32> { None }\n}";
        let out = lift_impl_methods(src);
        assert_eq!(out, src, "trait impls must pass through unchanged");
    }

    #[test]
    fn lift_impl_methods_unsafe_fn() {
        let src = "impl Foo {\n    unsafe fn raw() {}\n}";
        let out = lift_impl_methods(src);
        assert_eq!(out, "impl Foo {\n    pub(super) unsafe fn raw() {}\n}");
        syn::parse_file(&out).expect("unsafe impl-fn must parse");
    }

    #[test]
    fn lift_impl_methods_no_op_when_no_private_items() {
        let src = "impl Foo {\n    pub fn a() {}\n    pub(crate) fn b() {}\n}";
        let out = lift_impl_methods(src);
        assert_eq!(out, src);
    }

    #[test]
    fn promotes_through_multi_line_attribute() {
        // Regression: the previous line-prefix implementation would walk
        // past `#[allow(`, fail to recognize `    dead_code,` as still part
        // of the attr, and emit `pub(super) dead_code,` (invalid Rust).
        let src = "#[allow(\n    dead_code,\n)]\nfn helper() {}";
        let out = add_pub_super(src);
        assert_eq!(
            out,
            "#[allow(\n    dead_code,\n)]\npub(super) fn helper() {}"
        );
        syn::parse_file(&out).expect("multi-line-attr output must parse");
    }

    #[test]
    fn promotes_through_multi_line_derive() {
        // Tuple-field lift is on, so the inner `u32` gets `pub(super)` too —
        // the test's job is to prove the multi-line `#[derive(...)]` is
        // skipped correctly when locating the `struct` keyword. Both
        // splices (item keyword + tuple field) must happen in the right
        // order despite the multi-line attribute.
        let src = "#[derive(\n    Debug,\n    Clone,\n)]\nstruct Tagged(u32);";
        let out = add_pub_super(src);
        assert_eq!(
            out,
            "#[derive(\n    Debug,\n    Clone,\n)]\npub(super) struct Tagged(pub(super) u32);"
        );
        syn::parse_file(&out).expect("multi-line-derive output must parse");
    }

    #[test]
    fn promoted_output_parses_as_rust() {
        // Cheap sanity: every promoted form should still parse via syn so
        // a future refactor of `add_pub_super` can't accidentally produce
        // invalid Rust without a test failing.
        for src in [
            "fn helper() {}",
            "unsafe fn raw() {}",
            "async fn fetch() {}",
            "const fn doubled(x: u32) -> u32 { x * 2 }",
            "extern \"C\" fn cb() {}",
            "unsafe trait Marker {}",
            "struct Tagged(u32);",
            "enum State { On, Off }",
        ] {
            let promoted = add_pub_super(src);
            syn::parse_file(&promoted)
                .unwrap_or_else(|e| panic!("promoted output failed to parse: {promoted:?}: {e}"));
        }
    }
}
