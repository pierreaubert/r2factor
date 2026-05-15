//! Advisor reply schema + a tolerant JSON extractor. Local models often wrap
//! their output in markdown fences or prepend a sentence of prose, so we
//! find the first balanced `{ ... }` block rather than `serde_json::from_str`
//! on the raw body.

use crate::item::ItemId;
use anyhow::{Result, anyhow};
use serde::Deserialize;
use std::collections::HashMap;

#[derive(Deserialize, Debug, Default)]
#[serde(default)]
pub struct AdvisorReply {
    pub renames: HashMap<String, String>,
    pub moves: Vec<AdvisorMove>,
}

#[derive(Deserialize, Debug)]
pub struct AdvisorMove {
    pub id: ItemId,
    pub to: String,
}

pub fn parse_reply(raw: &str) -> Result<AdvisorReply> {
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
