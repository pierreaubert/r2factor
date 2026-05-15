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
