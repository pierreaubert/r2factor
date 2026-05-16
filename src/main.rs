use anyhow::Result;
use clap::{Parser, Subcommand};
use r2factor::SplitOptions;
use r2factor::llm::LlmConfig;
use r2factor::write::WriteOptions;
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "r2factor", about = "Refactor large Rust files into modules")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run r2factor as an MCP (Model Context Protocol) server over stdio.
    /// Lets an MCP-aware client (Claude Code, IDE extensions, etc.)
    /// discover and call `split_dry_run` and `split_write` as tools.
    Mcp,
    /// Consolidate `foo.rs + foo/` (or `foo/mod.rs + foo/*.rs`) back into
    /// a single `.rs` file. Inverse of `split`. Without --write, just
    /// prints the merged content to stdout.
    Consolidate {
        /// Either the facade file (`foo.rs` or `foo/mod.rs`) or the
        /// sub-directory containing the buckets.
        path: PathBuf,
        /// Actually replace the facade in place, backup to `.bak`, and
        /// delete the sub-dir.
        #[arg(long)]
        write: bool,
    },
    /// Propose how to split a single .rs file into a module of smaller files.
    Split {
        file: PathBuf,
        /// Skip tokensave even when a `.tokensave/` is found.
        #[arg(long)]
        no_tokensave: bool,
        /// Run an LLM placement signal over the deterministic plan.
        #[arg(long)]
        llm: bool,
        /// LLM endpoint (OpenAI-compatible). Defaults to local Ollama.
        #[arg(long, default_value = "http://localhost:11434/v1/chat/completions")]
        llm_endpoint: String,
        /// LLM model name.
        #[arg(long, default_value = "llama3.2:3b")]
        llm_model: String,
        /// Bearer token for hosted endpoints. Falls back to env
        /// `R2FACTOR_LLM_API_KEY`. Local endpoints (Ollama / llama.cpp /
        /// LM Studio) ignore this.
        #[arg(long, env = "R2FACTOR_LLM_API_KEY", hide_env_values = true)]
        llm_api_key: Option<String>,
        /// Actually write the split. Without this the run is dry-run only.
        #[arg(long)]
        write: bool,
        /// When writing, overwrite an existing target directory.
        #[arg(long, requires = "write")]
        force: bool,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Mcp => r2factor::mcp::serve(),
        Cmd::Consolidate { path, write } => {
            if write {
                let report = r2factor::consolidate::consolidate_write(
                    &path,
                    &r2factor::consolidate::ConsolidateOptions { write: true },
                )?;
                eprintln!("[consolidate] merged -> {}", report.merged_target.display());
                if let Some(b) = &report.backup {
                    eprintln!("[consolidate] backup -> {}", b.display());
                }
                eprintln!(
                    "[consolidate] removed {} sub-file(s)",
                    report.removed_files.len()
                );
                Ok(())
            } else {
                let merged = r2factor::consolidate::consolidate_dry_run(&path)?;
                println!("{merged}");
                Ok(())
            }
        }
        Cmd::Split {
            file,
            no_tokensave,
            llm,
            llm_endpoint,
            llm_model,
            llm_api_key,
            write,
            force,
        } => {
            let opts = SplitOptions {
                use_tokensave: !no_tokensave,
                llm: llm.then_some(LlmConfig {
                    endpoint: llm_endpoint,
                    model: llm_model,
                    timeout_secs: 120,
                    api_key: llm_api_key,
                }),
                write: write.then_some(WriteOptions { force }),
            };
            r2factor::run_split(&file, opts)
        }
    }
}
