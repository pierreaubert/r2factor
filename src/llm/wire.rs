//! OpenAI-compatible chat wire format + a one-shot HTTP send. Kept separate
//! so prompt construction and reply parsing don't drag in `ureq` or the
//! request/response shapes.

use super::config::LlmConfig;
use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};
use std::time::Duration;

#[derive(Serialize)]
pub struct ChatRequest<'a> {
    pub model: &'a str,
    pub messages: Vec<ChatMessage<'a>>,
    pub response_format: ResponseFormat,
    pub temperature: f32,
    pub stream: bool,
}

#[derive(Serialize)]
pub struct ChatMessage<'a> {
    pub role: &'a str,
    pub content: String,
}

#[derive(Serialize)]
pub struct ResponseFormat {
    #[serde(rename = "type")]
    pub kind: &'static str,
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

/// POST the chat request to `cfg.endpoint` and return the first choice's
/// raw `content` string. The caller is responsible for extracting / parsing
/// JSON from the content — local models often wrap it in prose or fences.
pub fn send_chat(cfg: &LlmConfig, req: &ChatRequest<'_>) -> Result<String> {
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
        .send_json(serde_json::to_value(req)?)
        .with_context(|| format!("POST {}", cfg.endpoint))?
        .into_json()
        .context("decode chat response")?;
    Ok(resp
        .choices
        .into_iter()
        .next()
        .ok_or_else(|| anyhow!("LLM returned no choices"))?
        .message
        .content)
}
