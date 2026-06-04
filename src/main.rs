use anyhow::Result;
use clap::{Parser, Subcommand};
use r2factor::SplitOptions;
use r2factor::combine::CombineOptions;
use r2factor::llm::LlmConfig;
use r2factor::refactor::{RefactorMode, RefactorOptions};
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
    /// List `.bak` files created by r2factor operations.
    Backups {
        /// File or directory to inspect. Defaults to cwd.
        path: Option<PathBuf>,
        /// Output the backup list as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Check local r2factor readiness: Cargo root, TokenSave index health,
    /// known weak signals, and local path dependencies.
    Check {
        /// Path inside the project to inspect. Defaults to cwd.
        path: Option<PathBuf>,
        /// Output the health report as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Discover all `.rs` files under a directory, parse them, and print a
    /// global item index + reference graph. Non-destructive.
    Discover {
        /// Directory to scan. Defaults to cwd.
        path: Option<PathBuf>,
        /// Output the discovery as JSON.
        #[arg(long)]
        json: bool,
        /// Skip tokensave discovery even if available.
        #[arg(long)]
        no_tokensave: bool,
    },
    /// Validate an architecture spec file (JSON or YAML) and report errors
    /// and warnings.
    SpecValidate {
        /// Path to the spec file.
        spec: PathBuf,
        /// Output the validation report as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Build a global refactoring plan from an architecture spec and a
    /// codebase directory. Non-destructive — produces a dry-run report.
    Plan {
        /// Path to the architecture spec file.
        spec: PathBuf,
        /// Directory to discover. Defaults to cwd.
        #[arg(long)]
        path: Option<PathBuf>,
        /// Skip tokensave discovery even if available.
        #[arg(long)]
        no_tokensave: bool,
        /// Output the plan as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Safe whole-codebase refactor engine: plan, preview, write, or rollback.
    Refactor {
        #[command(subcommand)]
        cmd: RefactorCmd,
    },
    /// Run r2factor as an MCP (Model Context Protocol) server over stdio.
    /// Lets an MCP-aware client (Claude Code, IDE extensions, etc.)
    /// discover and call `split_dry_run` and `split_write` as tools.
    Mcp,
    /// Restore a single `.bak` file to its original sibling path.
    Restore {
        /// Backup file to restore, such as `foo.rs.bak`.
        backup: PathBuf,
        /// Overwrite the restore target if it already exists.
        #[arg(long)]
        force: bool,
        /// Output the restore report as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Combine two or more peer `.rs` files into a new parent module directory.
    /// Generates a facade with mod declarations and re-exports, rewrites
    /// paths, and updates the parent module declaration.
    Combine {
        /// First `.rs` file to combine.
        file1: PathBuf,
        /// Second `.rs` file to combine.
        file2: PathBuf,
        /// Additional peer `.rs` files to combine into the same parent module.
        extra_files: Vec<PathBuf>,
        /// Name for the new parent module directory.
        #[arg(long)]
        name: Option<String>,
        /// Actually perform the combine. Without this, runs in dry-run mode.
        #[arg(long)]
        write: bool,
        /// Overwrite an existing target directory.
        #[arg(long, requires = "write")]
        force: bool,
        /// Output dry-run plan as JSON.
        #[arg(long)]
        json: bool,
        /// Show consumer impact report (requires tokensave).
        #[arg(long)]
        preview_impacts: bool,
        /// Skip tokensave discovery even if available.
        #[arg(long)]
        no_tokensave: bool,
        /// Filter pattern for re-exports (regex).
        #[arg(long)]
        re_export_filter: Option<String>,
        /// Rewrite crate consumers from old module paths to the new combined path.
        #[arg(long, requires = "write")]
        rewrite_consumers: bool,
        /// Preview crate consumer path rewrites in dry-run output.
        #[arg(long)]
        preview_consumer_rewrites: bool,
    },
    /// Suggest peer files that are good candidates for `combine`.
    CombineSuggest {
        /// Directory (or a file inside the directory) to inspect. Defaults to cwd.
        path: Option<PathBuf>,
        /// Output suggestions as JSON.
        #[arg(long)]
        json: bool,
        /// Minimum score required for a suggestion.
        #[arg(long, default_value_t = 1)]
        min_score: usize,
    },
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
    /// Flatten a consolidated file's inline modules into one scope with
    /// mechanical bucket-prefixed renames. Without --write, prints the
    /// flattened content to stdout.
    Flatten {
        /// The consolidated `.rs` file containing inline `mod name { ... }`
        /// blocks.
        file: PathBuf,
        /// Actually replace the file in place, backing it up to `.bak`.
        #[arg(long)]
        write: bool,
    },
    /// Split a Rust file, or with --write split every oversized .rs file under a directory.
    Split {
        /// Rust source file, or a directory to scan when --write is set.
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
        /// Recursively split generated files above this many lines. For directory input,
        /// also selects source files over this threshold. Use 0 to disable.
        #[arg(long, requires = "write", default_value_t = 1000)]
        max_lines: usize,
    },
}

#[derive(Subcommand)]
enum RefactorCmd {
    /// Build a typed migration plan. Non-destructive.
    Plan {
        spec: PathBuf,
        #[arg(long)]
        path: Option<PathBuf>,
        #[arg(long)]
        no_tokensave: bool,
        #[arg(long)]
        no_rg: bool,
        #[arg(long)]
        llm: bool,
        #[arg(long, default_value = "http://localhost:11434/v1/chat/completions")]
        llm_endpoint: String,
        #[arg(long, default_value = "llama3.2:3b")]
        llm_model: String,
        #[arg(long, env = "R2FACTOR_LLM_API_KEY", hide_env_values = true)]
        llm_api_key: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Apply the migration plan to a temp copy and run checks.
    Preview {
        spec: PathBuf,
        #[arg(long)]
        path: Option<PathBuf>,
        #[arg(long)]
        no_tokensave: bool,
        #[arg(long)]
        no_rg: bool,
        /// Also run cargo test for affected packages after mandatory cargo check.
        #[arg(long)]
        run_tests: bool,
        #[arg(long)]
        json: bool,
    },
    /// Write only after preview/checks pass. Requires --confirm.
    Write {
        spec: PathBuf,
        #[arg(long)]
        path: Option<PathBuf>,
        #[arg(long)]
        no_tokensave: bool,
        #[arg(long)]
        no_rg: bool,
        #[arg(long)]
        confirm: bool,
        /// Also run cargo test for affected packages before and after writing.
        #[arg(long)]
        run_tests: bool,
        #[arg(long)]
        json: bool,
    },
    /// Restore files from a refactor write manifest.
    Rollback {
        manifest: PathBuf,
        #[arg(long)]
        json: bool,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Backups { path, json } => {
            let path = path.unwrap_or(std::env::current_dir()?);
            let entries = r2factor::backups::list_backups(&path)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&entries)?);
            } else {
                print!("{}", r2factor::backups::human_list(&entries));
            }
            Ok(())
        }
        Cmd::Check { path, json } => {
            let path = path.unwrap_or(std::env::current_dir()?);
            let report = r2factor::health::check(&path)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                print!("{}", r2factor::health::human_report(&report));
            }
            Ok(())
        }
        Cmd::Discover {
            path,
            json,
            no_tokensave,
        } => {
            let path = path.unwrap_or(std::env::current_dir()?);
            let disc = r2factor::discover::discover_dir(&path, !no_tokensave)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&disc)?);
            } else {
                print!("{}", r2factor::discover::human_report(&disc));
            }
            Ok(())
        }
        Cmd::SpecValidate { spec, json } => {
            let s = r2factor::spec::parse_spec(&spec)?;
            let report = r2factor::spec::validate(&s);
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "valid": report.is_valid(),
                        "errors": report.errors,
                        "warnings": report.warnings,
                    }))?
                );
            } else {
                if report.is_valid() {
                    println!("✓ spec is valid");
                } else {
                    println!("✗ spec is invalid");
                }
                for e in &report.errors {
                    println!("  error: {e}");
                }
                for w in &report.warnings {
                    println!("  warning: {w}");
                }
            }
            if report.is_valid() {
                Ok(())
            } else {
                std::process::exit(1);
            }
        }
        Cmd::Plan {
            spec,
            path,
            no_tokensave,
            json,
        } => {
            let s = r2factor::spec::parse_spec(&spec)?;
            let report = r2factor::spec::validate(&s);
            if !report.is_valid() {
                eprintln!("spec is invalid; aborting");
                for e in &report.errors {
                    eprintln!("  error: {e}");
                }
                std::process::exit(1);
            }
            let path = path.unwrap_or(std::env::current_dir()?);
            let disc = r2factor::discover::discover_dir(&path, !no_tokensave)?;
            let plan = r2factor::global_plan::build_plan(&disc, &s);
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "assignments": plan.assignments,
                        "violations": plan.violations,
                        "operations": plan.operations,
                    }))?
                );
            } else {
                print!("{}", r2factor::global_plan::human_report(&plan, &disc));
            }
            Ok(())
        }
        Cmd::Refactor { cmd } => match cmd {
            RefactorCmd::Plan {
                spec,
                path,
                no_tokensave,
                no_rg,
                llm,
                llm_endpoint,
                llm_model,
                llm_api_key,
                json,
            } => {
                let spec = r2factor::spec::parse_spec(&spec)?;
                let path = path.unwrap_or(std::env::current_dir()?);
                let opts = RefactorOptions {
                    use_tokensave: !no_tokensave,
                    require_tokensave_for_rewrites: true,
                    use_rg_candidates: !no_rg,
                    llm: llm.then_some(LlmConfig {
                        endpoint: llm_endpoint,
                        model: llm_model,
                        timeout_secs: 120,
                        api_key: llm_api_key,
                    }),
                    run_full_tests: false,
                    mode: RefactorMode::Plan,
                };
                let report = r2factor::refactor::refactor_plan(&path, &spec, &opts)?;
                if json {
                    println!("{}", serde_json::to_string_pretty(&report)?);
                } else {
                    println!("{}", serde_json::to_string_pretty(&report)?);
                }
                Ok(())
            }
            RefactorCmd::Preview {
                spec,
                path,
                no_tokensave,
                no_rg,
                run_tests,
                json,
            } => {
                let spec = r2factor::spec::parse_spec(&spec)?;
                let path = path.unwrap_or(std::env::current_dir()?);
                let opts = RefactorOptions {
                    use_tokensave: !no_tokensave,
                    require_tokensave_for_rewrites: true,
                    use_rg_candidates: !no_rg,
                    llm: None,
                    run_full_tests: run_tests,
                    mode: RefactorMode::Preview,
                };
                let report = r2factor::refactor::refactor_preview(&path, &spec, &opts)?;
                if json {
                    println!("{}", serde_json::to_string_pretty(&report)?);
                } else {
                    println!("{}", serde_json::to_string_pretty(&report)?);
                }
                Ok(())
            }
            RefactorCmd::Write {
                spec,
                path,
                no_tokensave,
                no_rg,
                confirm,
                run_tests,
                json,
            } => {
                if !confirm {
                    anyhow::bail!("refactor write requires --confirm");
                }
                let spec = r2factor::spec::parse_spec(&spec)?;
                let path = path.unwrap_or(std::env::current_dir()?);
                let opts = RefactorOptions {
                    use_tokensave: !no_tokensave,
                    require_tokensave_for_rewrites: true,
                    use_rg_candidates: !no_rg,
                    llm: None,
                    run_full_tests: run_tests,
                    mode: RefactorMode::Write,
                };
                let report = r2factor::refactor::refactor_write(&path, &spec, &opts)?;
                if json {
                    println!("{}", serde_json::to_string_pretty(&report)?);
                } else {
                    println!("{}", serde_json::to_string_pretty(&report)?);
                }
                Ok(())
            }
            RefactorCmd::Rollback { manifest, json } => {
                let report = r2factor::refactor::refactor_rollback_manifest(&manifest)?;
                if json {
                    println!("{}", serde_json::to_string_pretty(&report)?);
                } else {
                    println!("{}", serde_json::to_string_pretty(&report)?);
                }
                Ok(())
            }
        },
        Cmd::Mcp => r2factor::mcp::serve(),
        Cmd::Restore {
            backup,
            force,
            json,
        } => {
            let report = r2factor::backups::restore_backup(&backup, force)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                print!("{}", r2factor::backups::human_restore(&report));
            }
            Ok(())
        }
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
        Cmd::Flatten { file, write } => {
            if write {
                let report = r2factor::flatten::flatten_write(
                    &file,
                    &r2factor::flatten::FlattenOptions { write: true },
                )?;
                eprintln!("[flatten] flattened -> {}", report.target.display());
                if let Some(b) = &report.backup {
                    eprintln!("[flatten] backup -> {}", b.display());
                }
                eprintln!("[flatten] rewrites -> {}", report.rewrites);
                for warning in &report.warnings {
                    eprintln!("[flatten] warning: {warning}");
                }
                Ok(())
            } else {
                let flattened = r2factor::flatten::flatten_dry_run(&file)?;
                println!("{flattened}");
                Ok(())
            }
        }
        Cmd::Combine {
            file1,
            file2,
            extra_files,
            name,
            write,
            force,
            json,
            preview_impacts,
            no_tokensave,
            re_export_filter,
            rewrite_consumers,
            preview_consumer_rewrites,
        } => {
            let mut files = vec![file1, file2];
            files.extend(extra_files);
            let opts = CombineOptions {
                module_name: name,
                write,
                force,
                json,
                preview_impacts,
                use_tokensave: !no_tokensave,
                re_export_filter,
                rewrite_consumers,
                preview_consumer_rewrites,
            };
            if write {
                let report = r2factor::combine::combine_write_many(&files, &opts)?;
                eprintln!("[combine] facade -> {}", report.facade_path.display());
                for m in &report.moved_files {
                    eprintln!("[combine] moved {} -> {}", m.from.display(), m.to.display());
                }
                if let Some(p) = &report.parent_update {
                    eprintln!(
                        "[combine] parent -> {} (add `{}`, remove {:?})",
                        p.path.display(),
                        p.add,
                        p.remove
                    );
                }
                for b in &report.backups {
                    eprintln!("[combine] backup -> {}", b.display());
                }
                for r in &report.consumer_rewrites {
                    eprintln!(
                        "[combine] consumer -> {} ({} replacement(s))",
                        r.file.display(),
                        r.replacements
                    );
                    for hunk in &r.hunks {
                        eprintln!("[combine]   L{}: {} -> {}", hunk.line, hunk.old, hunk.new);
                    }
                }
                for skipped in &report.skipped_consumer_rewrites {
                    eprintln!(
                        "[combine] skipped consumer -> {}:{} `{}` ({})",
                        skipped.file.display(),
                        skipped.line,
                        skipped.old,
                        skipped.reason
                    );
                }
                Ok(())
            } else {
                let report = r2factor::combine::combine_dry_run_many(&files, &opts)?;
                println!("{report}");
                Ok(())
            }
        }
        Cmd::CombineSuggest {
            path,
            json,
            min_score,
        } => {
            let path = path.unwrap_or(std::env::current_dir()?);
            let report = r2factor::combine::suggest_groups(
                &path,
                &r2factor::combine::SuggestOptions { min_score },
            )?;
            if json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                if report.suggestions.is_empty() {
                    println!(
                        "No combine suggestions found in {}.",
                        report.directory.display()
                    );
                } else {
                    println!("Combine suggestions for {}:", report.directory.display());
                    for suggestion in &report.suggestions {
                        let files = suggestion
                            .files
                            .iter()
                            .map(|path| path.display().to_string())
                            .collect::<Vec<_>>()
                            .join(", ");
                        println!(
                            "- {} (score {}): {}",
                            suggestion.module_name, suggestion.score, files
                        );
                        for reason in &suggestion.reasons {
                            println!("  {reason}");
                        }
                    }
                }
                println!("[tokensave] {}", report.tokensave.message);
            }
            Ok(())
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
            max_lines,
        } => {
            let opts = SplitOptions {
                use_tokensave: !no_tokensave,
                llm: llm.then_some(LlmConfig {
                    endpoint: llm_endpoint,
                    model: llm_model,
                    timeout_secs: 120,
                    api_key: llm_api_key,
                }),
                write: write.then_some(WriteOptions {
                    force,
                    recursive_max_lines: Some(max_lines),
                }),
            };
            r2factor::run_split(&file, opts)
        }
    }
}
