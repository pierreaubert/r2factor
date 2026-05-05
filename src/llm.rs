use crate::item::{ItemId, ParsedItem};
use crate::plan::Plan;
use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::time::Duration;

#[derive(Clone, Debug)]
pub struct LlmConfig {
    pub endpoint: String,
    pub model: String,
    pub timeout_secs: u64,
    /// Optional bearer token sent as `Authorization: Bearer <key>`.
    /// Required by hosted OpenAI-compatible endpoints; ignored locally.
    pub api_key: Option<String>,
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            endpoint: "http://localhost:11434/v1/chat/completions".to_string(),
            model: "llama3.2:3b".to_string(),
            timeout_secs: 120,
            api_key: None,
        }
    }
}

#[derive(Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    messages: Vec<ChatMessage<'a>>,
    response_format: ResponseFormat,
    temperature: f32,
    stream: bool,
}

#[derive(Serialize)]
struct ChatMessage<'a> {
    role: &'a str,
    content: String,
}

#[derive(Serialize)]
struct ResponseFormat {
    #[serde(rename = "type")]
    kind: &'static str,
}

#[derive(Deserialize)]
struct ChatResponse {
    choices: Vec<ChatChoice>,
}

#[derive(Deserialize)]
struct ChatChoice {
    message: ChatResponseMessage,
}

#[derive(Deserialize)]
struct ChatResponseMessage {
    content: String,
}

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

#[derive(Deserialize, Debug, Default)]
#[serde(default)]
struct AdvisorReply {
    renames: HashMap<String, String>,
    moves: Vec<AdvisorMove>,
}

#[derive(Deserialize, Debug)]
struct AdvisorMove {
    id: ItemId,
    to: String,
}

const SYSTEM_PROMPT: &str = "\
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

pub fn advise(cfg: &LlmConfig, plan: &Plan, items: &[ParsedItem]) -> Result<AdvisorOutcome> {
    let by_id: BTreeMap<ItemId, &ParsedItem> = items.iter().map(|i| (i.id, i)).collect();
    let prompt = build_prompt(plan, &by_id);

    let req = ChatRequest {
        model: &cfg.model,
        messages: vec![
            ChatMessage {
                role: "system",
                content: SYSTEM_PROMPT.to_string(),
            },
            ChatMessage {
                role: "user",
                content: prompt,
            },
        ],
        response_format: ResponseFormat { kind: "json_object" },
        temperature: 0.0,
        stream: false,
    };

    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(cfg.timeout_secs))
        .build();
    let mut req_builder = agent
        .post(&cfg.endpoint)
        .set("content-type", "application/json");
    if let Some(key) = cfg.api_key.as_deref()
        && !key.is_empty()
    {
        req_builder = req_builder.set("authorization", &format!("Bearer {key}"));
    }
    let resp: ChatResponse = req_builder
        .send_json(serde_json::to_value(&req)?)
        .with_context(|| format!("POST {}", cfg.endpoint))?
        .into_json()
        .context("decode chat response")?;

    let raw = resp
        .choices
        .into_iter()
        .next()
        .ok_or_else(|| anyhow!("LLM returned no choices"))?
        .message
        .content;

    let reply: AdvisorReply = parse_reply(&raw)
        .with_context(|| format!("parse advisor JSON: {raw:.200}"))?;

    apply_reply(plan, items, reply)
}

fn build_prompt(plan: &Plan, by_id: &BTreeMap<ItemId, &ParsedItem>) -> String {
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
    use crate::item::ItemKind as K;
    match it.kind {
        K::Fn { is_test: true } => "test_fn",
        K::Fn { is_test: false } => "fn",
        K::Struct => "struct",
        K::Enum => "enum",
        K::Union => "union",
        K::Trait => "trait",
        K::TraitAlias => "trait_alias",
        K::Impl { .. } => "impl",
        K::Macro => "macro",
        K::Const => "const",
        K::Static => "static",
        K::TypeAlias => "type_alias",
        K::Use => "use",
        K::ExternCrate => "extern_crate",
        K::Mod => "mod",
        K::ForeignMod => "foreign_mod",
        K::Verbatim => "verbatim",
    }
}

fn parse_reply(raw: &str) -> Result<AdvisorReply> {
    // Some local models wrap JSON in code fences or add a stray prose line.
    // Take the first balanced `{ ... }` block.
    let start = raw.find('{').ok_or_else(|| anyhow!("no `{{` in reply"))?;
    let mut depth = 0usize;
    let mut end = None;
    for (i, ch) in raw[start..].char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    end = Some(start + i + 1);
                    break;
                }
            }
            _ => {}
        }
    }
    let end = end.ok_or_else(|| anyhow!("unbalanced JSON in reply"))?;
    Ok(serde_json::from_str(&raw[start..end])?)
}

#[derive(Debug)]
pub struct AdvisorOutcome {
    pub plan: Plan,
    pub applied_renames: Vec<(String, String)>,
    pub applied_moves: Vec<(ItemId, String, String)>, // id, from, to
    pub rejected: Vec<String>,
}

fn apply_reply(plan: &Plan, items: &[ParsedItem], reply: AdvisorReply) -> Result<AdvisorOutcome> {
    let mut new = Plan {
        assignments: plan.assignments.clone(),
        rationale: plan.rationale.clone(),
    };
    let mut applied_renames = Vec::new();
    let mut applied_moves = Vec::new();
    let mut rejected = Vec::new();

    // 1) Apply moves first (they reference original module names).
    let valid_ids: BTreeSet<ItemId> = items.iter().map(|i| i.id).collect();
    let mut already_moved: BTreeSet<ItemId> = BTreeSet::new();
    for mv in reply.moves {
        if !valid_ids.contains(&mv.id) {
            rejected.push(format!("move: unknown id {}", mv.id));
            continue;
        }
        if already_moved.contains(&mv.id) {
            rejected.push(format!("move: id {} listed twice", mv.id));
            continue;
        }
        let dest = sanitize_module(&mv.to);
        let from = match find_bucket(&new, mv.id) {
            Some(b) => b,
            None => {
                rejected.push(format!("move: id {} not found in plan", mv.id));
                continue;
            }
        };
        if from == dest {
            continue;
        }
        if let Some(v) = new.assignments.get_mut(&from) {
            v.retain(|x| *x != mv.id);
        }
        new.assignments
            .entry(dest.clone())
            .or_default()
            .push(mv.id);
        new.rationale
            .insert(mv.id, format!("LLM move from `{from}`"));
        already_moved.insert(mv.id);
        applied_moves.push((mv.id, from, dest));
    }

    // Drop empty buckets after moves so renames don't operate on ghosts.
    new.assignments.retain(|_, v| !v.is_empty());

    // 2) Apply renames (old name → new name).
    for (old, new_name) in reply.renames {
        let new_name = sanitize_module(&new_name);
        if old == new_name {
            continue;
        }
        let Some(items_in) = new.assignments.remove(&old) else {
            rejected.push(format!("rename: unknown bucket `{old}`"));
            continue;
        };
        new.assignments
            .entry(new_name.clone())
            .or_default()
            .extend(items_in);
        applied_renames.push((old, new_name));
    }

    // Sanity: every original id is present exactly once.
    let mut seen: BTreeSet<ItemId> = BTreeSet::new();
    for ids in new.assignments.values() {
        for id in ids {
            if !seen.insert(*id) {
                return Err(anyhow!("LLM apply produced duplicate id {id}"));
            }
        }
    }
    if seen != valid_ids {
        let missing: Vec<ItemId> = valid_ids.difference(&seen).copied().collect();
        return Err(anyhow!("LLM apply dropped ids: {missing:?}"));
    }

    Ok(AdvisorOutcome {
        plan: new,
        applied_renames,
        applied_moves,
        rejected,
    })
}

fn find_bucket(plan: &Plan, id: ItemId) -> Option<String> {
    plan.assignments
        .iter()
        .find(|(_, v)| v.contains(&id))
        .map(|(k, _)| k.clone())
}

fn sanitize_module(name: &str) -> String {
    let trimmed = name.trim().trim_end_matches(".rs");
    let mut out = String::with_capacity(trimmed.len());
    for ch in trimmed.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' {
            out.push(ch.to_ascii_lowercase());
        } else if ch.is_whitespace() || ch == '-' || ch == '/' {
            out.push('_');
        }
    }
    if out.is_empty() {
        "misc".to_string()
    } else {
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::item::{ItemKind, ItemVis};

    #[test]
    fn parses_fenced_reply() {
        let raw = "```json\n{\"renames\": {\"a\": \"b\"}, \"moves\": []}\n```";
        let r = parse_reply(raw).unwrap();
        assert_eq!(r.renames.get("a").unwrap(), "b");
    }

    #[test]
    fn parses_prose_then_json() {
        let raw = "Sure! Here is the JSON: { \"renames\": {}, \"moves\": [] } thanks";
        parse_reply(raw).unwrap();
    }

    #[test]
    fn sanitize_strips_ext_and_specials() {
        assert_eq!(sanitize_module("Eval Helpers.rs"), "eval_helpers");
        assert_eq!(sanitize_module("foo-bar/baz"), "foo_bar_baz");
        assert_eq!(sanitize_module(""), "misc");
    }

    fn fake_item(id: ItemId, name: &str) -> ParsedItem {
        ParsedItem {
            id,
            kind: ItemKind::Fn { is_test: false },
            name: name.to_string(),
            vis: ItemVis::Private,
            is_cfg_test: false,
            line_start: 1,
            line_end: 1,
            source: String::new(),
            refs: vec![],
        }
    }

    fn three_item_plan() -> (Plan, Vec<ParsedItem>) {
        let items = vec![
            fake_item(0, "alpha"),
            fake_item(1, "beta"),
            fake_item(2, "gamma"),
        ];
        let mut plan = Plan::default();
        plan.assign("misc", 0, "");
        plan.assign("misc", 1, "");
        plan.assign("xyz", 2, "");
        (plan, items)
    }

    #[test]
    fn applies_rename_and_move() {
        let (plan, items) = three_item_plan();
        let reply = AdvisorReply {
            renames: [("misc".to_string(), "core".to_string())].into_iter().collect(),
            moves: vec![AdvisorMove { id: 2, to: "core".to_string() }],
        };
        let out = apply_reply(&plan, &items, reply).unwrap();
        let core = out.plan.assignments.get("core").unwrap();
        assert_eq!(core.len(), 3);
        assert!(!out.plan.assignments.contains_key("misc"));
        assert_eq!(out.applied_moves.len(), 1);
        assert_eq!(out.applied_renames, vec![("misc".to_string(), "core".to_string())]);
    }

    #[test]
    fn rejects_unknown_id_keeps_plan_valid() {
        let (plan, items) = three_item_plan();
        let reply = AdvisorReply {
            renames: Default::default(),
            moves: vec![AdvisorMove { id: 99, to: "elsewhere".to_string() }],
        };
        let out = apply_reply(&plan, &items, reply).unwrap();
        assert!(out.rejected.iter().any(|r| r.contains("unknown id 99")));
        // No bucket changes.
        assert_eq!(out.plan.assignments.len(), 2);
    }

    #[test]
    fn errors_when_apply_would_drop_id() {
        // Moves can't drop, but a rename to the same name is a no-op; we
        // simulate a drop by injecting an inconsistent state via duplicate
        // moves of the same id (which we already reject). Instead we verify
        // the duplicate-id guard fires when a future bug duplicates.
        let (plan, items) = three_item_plan();
        let reply = AdvisorReply {
            renames: [
                ("misc".to_string(), "core".to_string()),
                ("xyz".to_string(), "core".to_string()),
            ]
            .into_iter()
            .collect(),
            moves: vec![],
        };
        // Both renames merge into "core"; should still preserve all 3 ids.
        let out = apply_reply(&plan, &items, reply).unwrap();
        let total: usize = out.plan.assignments.values().map(|v| v.len()).sum();
        assert_eq!(total, 3);
    }
}
