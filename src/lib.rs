pub mod backups;
pub mod carve;
pub mod cluster;
pub mod combine;
pub mod consolidate;
pub mod discover;
pub mod flatten;
pub mod global_plan;
pub mod graph;
pub mod health;
pub mod item;
pub mod llm;
pub mod mcp;
pub mod names;
pub mod pipeline;
pub mod plan;
pub mod promote;
pub mod refactor;
pub mod refine;
pub mod rewrite;
pub mod spec;
pub mod symbol_graph;
pub mod tokensave;
pub mod verify;
pub mod workspace_model;
pub mod write;

pub use pipeline::run_split;

#[derive(Clone)]
pub struct SplitOptions {
    pub use_tokensave: bool,
    pub llm: Option<llm::LlmConfig>,
    pub write: Option<write::WriteOptions>,
}
