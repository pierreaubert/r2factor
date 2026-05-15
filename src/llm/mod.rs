//! LLM advisor pass. Public entry point is [`advise`], which takes a
//! deterministic [`Plan`] and asks the configured chat endpoint for
//! placement corrections, returning an [`AdvisorOutcome`] that either
//! supersedes or falls back to the input plan.

mod apply;
mod config;
mod prompt;
mod reply;
mod wire;

use crate::item::{ItemId, ParsedItem};
use crate::plan::Plan;
use anyhow::{Context, Result};
use std::collections::BTreeMap;

pub use apply::AdvisorOutcome;
pub use config::LlmConfig;

use apply::apply_reply;
use prompt::{SYSTEM_PROMPT, build_prompt};
use reply::parse_reply;
use wire::{ChatMessage, ChatRequest, ResponseFormat, send_chat};

pub fn advise(cfg: &LlmConfig, plan: &Plan, items: &[ParsedItem]) -> Result<AdvisorOutcome> {
    let by_id: BTreeMap<ItemId, &ParsedItem> = items.iter().map(|i| (i.id, i)).collect();
    let user_prompt = build_prompt(plan, &by_id);

    let req = ChatRequest {
        model: &cfg.model,
        messages: vec![
            ChatMessage {
                role: "system",
                content: SYSTEM_PROMPT.to_string(),
            },
            ChatMessage {
                role: "user",
                content: user_prompt,
            },
        ],
        response_format: ResponseFormat { kind: "json_object" },
        temperature: 0.0,
        stream: false,
    };

    let raw = send_chat(cfg, &req)?;
    let reply = parse_reply(&raw)
        .with_context(|| format!("parse advisor JSON: {raw:.200}"))?;
    apply_reply(plan, items, reply)
}
