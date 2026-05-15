//! Builds the JSON `plan` payload the advisor sees, plus the system prompt
//! that describes its job. The prompt is a constant string here so it's
//! easy to A/B in isolation from the wire/apply machinery.

use crate::item::{ItemId, ItemKind, ParsedItem};
use crate::plan::Plan;
use serde::Serialize;
use std::collections::BTreeMap;

pub const SYSTEM_PROMPT: &str = "\
You are an active placement signal for a Rust refactoring tool that splits \
one large file into smaller files of a Rust module. You receive a JSON `plan`: \
an array of buckets, each with a `module` filename (without .rs) and an \
`items` list. Each item has a stable integer `id`, a `kind`, a `name`, a \
one-line `signature`, a `lines` count, and `refs` (names of other items it \
touches).

The deterministic pass already nailed the easy buckets (tests, macros, error, \
consts, types). Your job is the hard part: place the items currently in `misc` \
and rename mechanical bucket names into concept names.

Rules:
1. NEVER drop or duplicate an item id. Every input id must appear at most once \
across all your `moves` (or not at all if it stays put).
2. ACTIVELY place every item currently in `misc`: either move it to an \
existing bucket where it semantically belongs, or move multiple misc items \
together to a new bucket and rename `misc` to that concept name. Aim for an \
empty `misc` if reasonable.
3. Rename mechanical lowercase-typename buckets when a stronger concept fits \
across the items (e.g. several `parse_*` and `lex_*` buckets might collapse \
under a `frontend` rename + moves).
4. For non-misc buckets, only move items that are clearly misplaced — the \
deterministic pass is right by default. No more than 3 such corrections.
5. Reply with strict JSON, no prose, matching this schema exactly:
   { \"renames\": { \"<old>\": \"<new>\" }, \"moves\": [{\"id\": <int>, \"to\": \"<module>\"}] }
6. If nothing to change, respond: {\"renames\": {}, \"moves\": []}";

#[derive(Serialize)]
struct PromptItem<'a> {
    id: ItemId,
    kind: &'a str,
    name: &'a str,
    /// First non-attribute source line — e.g. `pub fn parse(s: &str) -> Result<Ast>`.
    signature: String,
    lines: usize,
    refs: &'a [String],
}

#[derive(Serialize)]
struct PromptBucket<'a> {
    module: &'a str,
    items: Vec<PromptItem<'a>>,
}

pub fn build_prompt(plan: &Plan, by_id: &BTreeMap<ItemId, &ParsedItem>) -> String {
    let buckets: Vec<PromptBucket> = plan
        .assignments
        .iter()
        .map(|(module, ids)| {
            let prompt_items = ids
                .iter()
                .map(|id| {
                    let it = by_id[id];
                    PromptItem {
                        id: it.id,
                        kind: kind_str(it),
                        name: &it.name,
                        signature: truncate(it.signature(), 160),
                        lines: it.line_end.saturating_sub(it.line_start) + 1,
                        refs: &it.refs,
                    }
                })
                .collect();
            PromptBucket {
                module,
                items: prompt_items,
            }
        })
        .collect();

    let plan_json = serde_json::to_string_pretty(&buckets).unwrap_or_default();
    format!("plan:\n{plan_json}")
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max).collect();
    out.push('…');
    out
}

fn kind_str(it: &ParsedItem) -> &'static str {
    match it.kind {
        ItemKind::Fn { is_test: true } => "test_fn",
        ItemKind::Fn { is_test: false } => "fn",
        ItemKind::Struct => "struct",
        ItemKind::Enum => "enum",
        ItemKind::Union => "union",
        ItemKind::Trait => "trait",
        ItemKind::TraitAlias => "trait_alias",
        ItemKind::Impl { .. } => "impl",
        ItemKind::Macro => "macro",
        ItemKind::Const => "const",
        ItemKind::Static => "static",
        ItemKind::TypeAlias => "type_alias",
        ItemKind::Use => "use",
        ItemKind::ExternCrate => "extern_crate",
        ItemKind::Mod => "mod",
        ItemKind::ForeignMod => "foreign_mod",
        ItemKind::Verbatim => "verbatim",
    }
}
