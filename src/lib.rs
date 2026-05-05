pub mod carve;
pub mod cluster;
pub mod graph;
pub mod item;
pub mod llm;
pub mod plan;
pub mod tokensave;
pub mod write;

use anyhow::Result;
use std::path::Path;

pub struct SplitOptions {
    pub use_tokensave: bool,
    pub llm: Option<llm::LlmConfig>,
    pub write: Option<write::WriteOptions>,
}

pub fn run_split(path: &Path, opts: SplitOptions) -> Result<()> {
    let src = std::fs::read_to_string(path)?;
    let mut items = item::parse_file(&src)?;

    let evidence = if opts.use_tokensave {
        match tokensave::Tokensave::locate(path) {
            Some(root) => match tokensave::Tokensave::open(&root) {
                Ok(ts) => match ts.evidence_for_file(path, &items) {
                    Ok(ev) => {
                        let with_callees = ev.intra_file_callees.len();
                        let with_callers = ev.external_callers.len();
                        eprintln!(
                            "[tokensave] using {} ({} items with intra-file callees, {} with external callers)",
                            root.display(),
                            with_callees,
                            with_callers
                        );
                        Some(ev)
                    }
                    Err(e) => {
                        eprintln!("[tokensave] evidence query failed: {e}");
                        None
                    }
                },
                Err(e) => {
                    eprintln!("[tokensave] open failed: {e}");
                    None
                }
            },
            None => {
                eprintln!("[tokensave] no .tokensave/ found above {}", path.display());
                None
            }
        }
    } else {
        None
    };

    graph::annotate_refs(&mut items, evidence.as_ref());
    let mut plan = plan::build(&items);

    if let Some(cfg) = &opts.llm {
        match llm::advise(cfg, &plan, &items) {
            Ok(outcome) => {
                if !outcome.applied_renames.is_empty() || !outcome.applied_moves.is_empty() {
                    eprintln!(
                        "[llm] applied {} rename(s), {} move(s)",
                        outcome.applied_renames.len(),
                        outcome.applied_moves.len()
                    );
                    for (old, new) in &outcome.applied_renames {
                        eprintln!("[llm] rename {old} -> {new}");
                    }
                    for (id, from, to) in &outcome.applied_moves {
                        eprintln!("[llm] move id={id} {from} -> {to}");
                    }
                } else {
                    eprintln!("[llm] no changes proposed");
                }
                if !outcome.rejected.is_empty() {
                    eprintln!("[llm] rejected {} suggestion(s):", outcome.rejected.len());
                    for r in &outcome.rejected {
                        eprintln!("  - {r}");
                    }
                }
                plan = outcome.plan;
            }
            Err(e) => {
                eprintln!("[llm] advisor failed, keeping deterministic plan: {e}");
            }
        }
    }

    plan::print_dry_run(&plan, &items);

    if let Some(write_opts) = opts.write {
        let report = write::write_plan(path, &plan, &items, &write_opts)?;
        eprintln!();
        eprintln!("[write] backup -> {}", report.backup.display());
        eprintln!("[write] target -> {}/", report.target_dir.display());
        for f in &report.written_files {
            eprintln!("[write]   {}", f.display());
        }
        eprintln!("[write] facade -> {}", report.facade.display());
        eprintln!("[write] run `cargo check` next; private items used cross-bucket may need pub(super) promotion.");
    }

    Ok(())
}
