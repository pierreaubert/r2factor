//! Apply an `AdvisorReply` to a `Plan` defensively: validate every id, run
//! moves before renames (moves reference the original module names),
//! collapse empty buckets, and refuse to commit if any item id was dropped
//! or duplicated. The advisor is best-effort — this module is the safety
//! net that keeps the deterministic plan valid if the LLM misbehaves.

use super::reply::AdvisorReply;
use crate::item::{ItemId, ParsedItem};
use crate::names::sanitize_module;
use crate::plan::Plan;
use anyhow::{Result, anyhow};
use std::collections::BTreeSet;

#[derive(Debug)]
pub struct AdvisorOutcome {
    pub plan: Plan,
    pub applied_renames: Vec<(String, String)>,
    pub applied_moves: Vec<(ItemId, String, String)>, // id, from, to
    pub rejected: Vec<String>,
}

pub fn apply_reply(plan: &Plan, items: &[ParsedItem], reply: AdvisorReply) -> Result<AdvisorOutcome> {
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

#[cfg(test)]
mod tests {
    use super::super::reply::AdvisorMove;
    use super::*;
    use crate::item::{ItemKind, ItemVis};

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
        assert_eq!(out.plan.assignments.len(), 2);
    }

    #[test]
    fn errors_when_apply_would_drop_id() {
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
        let out = apply_reply(&plan, &items, reply).unwrap();
        let total: usize = out.plan.assignments.values().map(Vec::len).sum();
        assert_eq!(total, 3);
    }
}
