pub mod carve;
pub mod cluster;
pub mod graph;
pub mod item;
pub mod llm;
pub mod names;
pub mod pipeline;
pub mod plan;
pub mod promote;
pub mod refine;
pub mod tokensave;
pub mod write;

pub use pipeline::run_split;

pub struct SplitOptions {
    pub use_tokensave: bool,
    pub llm: Option<llm::LlmConfig>,
    pub write: Option<write::WriteOptions>,
}
