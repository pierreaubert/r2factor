//! End-to-end orchestration: parse → annotate → plan → optional LLM advice
//! → dry-run print → optional write. Kept out of `lib.rs` so the entry
//! function stays focused on options and the module tree.

use crate::SplitOptions;
use crate::graph;
use crate::item::{self, ParsedItem};
use crate::llm::{self, AdvisorOutcome, LlmConfig};
use crate::plan::{self, Plan};
use crate::tokensave::{CrossFileEvidence, Tokensave};
use crate::write::{self, WriteReport};
use anyhow::Result;
use std::path::{Path, PathBuf};

const DEFAULT_RECURSIVE_MAX_LINES: usize = 1000;

pub fn run_split(path: &Path, opts: SplitOptions) -> Result<()> {
    run_split_inner(path, &opts, 0)
}

fn run_split_inner(path: &Path, opts: &SplitOptions, depth: usize) -> Result<()> {
    let src = std::fs::read_to_string(path)?;
    // Refuse early — feeding an r2factor facade back through the pipeline
    // produces a degenerate plan that, if written, would delete the
    // previously-split sub-files. Detection rule lives in `write::`.
    if write::is_r2factor_facade(&src) {
        anyhow::bail!(
            "refusing to operate on {}: it is an r2factor facade. Run on the original source or restore from the `.bak` backup.",
            path.display()
        );
    }
    let mut items = item::parse_file(&src)?;

    let evidence = if opts.use_tokensave {
        load_evidence(path, &items)
    } else {
        None
    };

    graph::annotate_refs(&mut items, evidence.as_ref());
    let mut plan = plan::build(&items);

    if let Some(cfg) = &opts.llm {
        plan = run_llm(cfg, plan, &items);
    }

    plan::print_dry_run(&plan, &items);
    plan::report_cohesion(&plan, &items);

    if let Some(write_opts) = opts.write {
        let report = write::write_plan(path, &plan, &items, &write_opts)?;
        report_write(&report);
        recursively_split_written(&report, opts, depth)?;
    }

    Ok(())
}

fn recursively_split_written(
    report: &WriteReport,
    opts: &SplitOptions,
    depth: usize,
) -> Result<()> {
    let Some(write_opts) = opts.write else {
        return Ok(());
    };
    let max_lines = write_opts
        .recursive_max_lines
        .unwrap_or(DEFAULT_RECURSIVE_MAX_LINES);
    if max_lines == 0 {
        return Ok(());
    }
    for path in oversized_written_files(&report.written_files, max_lines)? {
        eprintln!(
            "[write] recursive split depth={} -> {} (>{max_lines} lines)",
            depth + 1,
            path.display()
        );
        run_split_inner(&path, opts, depth + 1)?;
    }
    Ok(())
}

fn oversized_written_files(paths: &[PathBuf], max_lines: usize) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    for path in paths {
        let src = std::fs::read_to_string(path)?;
        if write::is_r2factor_facade(&src) {
            continue;
        }
        if src.lines().count() > max_lines {
            out.push(path.clone());
        }
    }
    Ok(out)
}

fn load_evidence(path: &Path, items: &[ParsedItem]) -> Option<CrossFileEvidence> {
    let root = match Tokensave::locate(path) {
        Some(root) => root,
        None => {
            eprintln!("[tokensave] no .tokensave/ found above {}", path.display());
            return None;
        }
    };
    let ts = match Tokensave::open(&root) {
        Ok(ts) => ts,
        Err(e) => {
            eprintln!("[tokensave] open failed: {e}");
            return None;
        }
    };
    match ts.evidence_for_file(path, items) {
        Ok(ev) => {
            eprintln!(
                "[tokensave] using {} ({} items with intra-file callees)",
                root.display(),
                ev.intra_file_callees.len(),
            );
            Some(ev)
        }
        Err(e) => {
            eprintln!("[tokensave] evidence query failed: {e}");
            None
        }
    }
}

fn run_llm(cfg: &LlmConfig, plan: Plan, items: &[ParsedItem]) -> Plan {
    match llm::advise(cfg, &plan, items) {
        Ok(outcome) => {
            report_llm(&outcome);
            outcome.plan
        }
        Err(e) => {
            eprintln!("[llm] advisor failed, keeping deterministic plan: {e}");
            plan
        }
    }
}

fn report_llm(outcome: &AdvisorOutcome) {
    if outcome.applied_renames.is_empty() && outcome.applied_moves.is_empty() {
        eprintln!("[llm] no changes proposed");
    } else {
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
    }
    if !outcome.rejected.is_empty() {
        eprintln!("[llm] rejected {} suggestion(s):", outcome.rejected.len());
        for r in &outcome.rejected {
            eprintln!("  - {r}");
        }
    }
}

fn report_write(report: &WriteReport) {
    eprintln!();
    eprintln!("[write] backup -> {}", report.backup.display());
    match &report.target_dir {
        Some(dir) => eprintln!("[write] target -> {}/", dir.display()),
        None => eprintln!("[write] target -> (no sub-files; everything inlined in facade)"),
    }
    for f in &report.written_files {
        eprintln!("[write]   {}", f.display());
    }
    eprintln!("[write] facade -> {}", report.facade.display());
    eprintln!(
        "[write] run `cargo check` next; private items used cross-bucket may need pub(super) promotion."
    );
}
