//! End-to-end orchestration: parse → annotate → plan → optional LLM advice
//! → dry-run print → optional write. Kept out of `lib.rs` so the entry
//! function stays focused on options and the module tree.

use crate::SplitOptions;
use crate::graph;
use crate::item::{self, ParsedItem};
use crate::llm::{self, AdvisorOutcome, LlmConfig};
use crate::plan::{self, Plan};
use crate::tokensave::{CrossFileEvidence, Tokensave};
use crate::verify;
use crate::write::{self, WriteReport};
use anyhow::Result;
use serde::Deserialize;
use std::collections::BTreeSet;
use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};

const DEFAULT_RECURSIVE_MAX_LINES: usize = 1000;

pub fn run_split(path: &Path, opts: SplitOptions) -> Result<()> {
    if path.is_dir() {
        return run_split_dir(path, &opts, true);
    }
    if let Some(tree) = run_split_inner(path, &opts, 0, true)? {
        verify_file_split_if_package(path, &tree, true)?;
    }
    Ok(())
}

#[derive(Debug, serde::Serialize)]
pub struct SplitWriteTree {
    pub report: WriteReport,
    pub children: Vec<SplitWriteTree>,
}

pub fn split_write_tree(path: &Path, opts: SplitOptions) -> Result<SplitWriteTree> {
    let tree = split_write_tree_inner(path, opts, false)?;
    verify_file_split_if_package(path, &tree, false)?;
    Ok(tree)
}

pub fn split_write_tree_with_output(path: &Path, opts: SplitOptions) -> Result<SplitWriteTree> {
    split_write_tree_inner(path, opts, true)
}

fn split_write_tree_inner(path: &Path, opts: SplitOptions, emit: bool) -> Result<SplitWriteTree> {
    if path.is_dir() {
        anyhow::bail!(
            "split_write_tree requires a file path, got directory {}",
            path.display()
        );
    }
    run_split_inner(path, &opts, 0, emit)?
        .ok_or_else(|| anyhow::anyhow!("split_write_tree requires write options"))
}

fn run_split_dir(root: &Path, opts: &SplitOptions, emit: bool) -> Result<()> {
    let Some(write_opts) = opts.write else {
        anyhow::bail!(
            "directory split requires --write so each selected file can be split in place"
        );
    };
    let Some(max_lines) = write_opts.recursive_max_lines else {
        anyhow::bail!("directory split requires a max-lines threshold");
    };
    if max_lines == 0 {
        if emit {
            eprintln!(
                "[write] directory split disabled for {} because max-lines is 0",
                root.display()
            );
        }
        return Ok(());
    }

    let files = oversized_rust_files_under(root, max_lines)?;
    if emit {
        eprintln!(
            "[write] directory split {} file(s) over {max_lines} lines under {}",
            files.len(),
            root.display()
        );
    }
    let mut trees = Vec::new();
    for path in &files {
        if emit {
            eprintln!("[write] split {}", path.display());
        }
        if let Some(tree) = run_split_inner(path, opts, 0, emit)? {
            trees.push(tree);
        }
    }
    if emit {
        verify_directory_split(root, &files, &trees)?;
    }
    Ok(())
}

fn verify_file_split_if_package(file: &Path, tree: &SplitWriteTree, emit: bool) -> Result<()> {
    let Some(manifest) = verify::locate_package_manifest(file) else {
        if emit {
            eprintln!(
                "[check] skipped: no Cargo.toml with [package] found above {}",
                file.display()
            );
        }
        return Ok(());
    };
    verify::verify_split_with_manifest_and_rollback(file, &manifest, tree)?;
    Ok(())
}

fn run_split_inner(
    path: &Path,
    opts: &SplitOptions,
    depth: usize,
    emit: bool,
) -> Result<Option<SplitWriteTree>> {
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

    if emit {
        plan::print_dry_run(&plan, &items);
        plan::report_cohesion(&plan, &items);
    }

    if let Some(write_opts) = opts.write {
        let report = write::write_plan(path, &plan, &items, &write_opts)?;
        if emit {
            report_write(&report);
        }
        let children = recursively_split_written(&report, opts, depth, emit)?;
        return Ok(Some(SplitWriteTree { report, children }));
    }

    Ok(None)
}

fn recursively_split_written(
    report: &WriteReport,
    opts: &SplitOptions,
    depth: usize,
    emit: bool,
) -> Result<Vec<SplitWriteTree>> {
    let Some(write_opts) = opts.write else {
        return Ok(Vec::new());
    };
    let max_lines = write_opts
        .recursive_max_lines
        .unwrap_or(DEFAULT_RECURSIVE_MAX_LINES);
    if max_lines == 0 {
        return Ok(Vec::new());
    }
    let mut children = Vec::new();
    for path in oversized_written_files(&report.written_files, max_lines)? {
        if emit {
            eprintln!(
                "[write] recursive split depth={} -> {} (>{max_lines} lines)",
                depth + 1,
                path.display()
            );
        }
        if let Some(child) = run_split_inner(&path, opts, depth + 1, emit)? {
            children.push(child);
        }
    }
    Ok(children)
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

fn oversized_rust_files_under(root: &Path, max_lines: usize) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    let include_targets = included_rust_files_under(root)?;
    while let Some(dir) = stack.pop() {
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(e) => {
                anyhow::bail!("read_dir {}: {e}", dir.display());
            }
        };
        for entry in entries {
            let entry = entry?;
            let path = entry.path();
            let file_type = entry.file_type()?;
            if file_type.is_dir() {
                let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
                if matches!(
                    name,
                    ".git" | ".tokensave" | "target" | "node_modules" | "dist" | "build"
                ) {
                    continue;
                }
                stack.push(path);
            } else if file_type.is_file()
                && path.extension().and_then(|s| s.to_str()) == Some("rs")
                && is_oversized_split_candidate(&path, max_lines, &include_targets)?
            {
                out.push(path);
            }
        }
    }
    out.sort_by(|a, b| path_depth(a).cmp(&path_depth(b)).then_with(|| a.cmp(b)));
    Ok(out)
}

fn path_depth(path: &Path) -> usize {
    path.components().count()
}

fn is_oversized_split_candidate(
    path: &Path,
    max_lines: usize,
    include_targets: &BTreeSet<PathBuf>,
) -> Result<bool> {
    let src = std::fs::read_to_string(path)?;
    if write::is_r2factor_facade(&src) {
        return Ok(false);
    }
    if src.lines().count() <= max_lines {
        return Ok(false);
    }
    if include_targets.contains(&normalize_path(path)) {
        eprintln!(
            "[write] skip {}: referenced by include! from another Rust source",
            path.display()
        );
        return Ok(false);
    }
    if let Err(err) = item::parse_file(&src) {
        eprintln!(
            "[write] skip {}: cannot split non-standalone Rust source ({err})",
            path.display()
        );
        return Ok(false);
    }
    Ok(true)
}

fn included_rust_files_under(root: &Path) -> Result<BTreeSet<PathBuf>> {
    let mut targets = BTreeSet::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = std::fs::read_dir(&dir)?;
        for entry in entries {
            let entry = entry?;
            let path = entry.path();
            let file_type = entry.file_type()?;
            if file_type.is_dir() {
                let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
                if matches!(
                    name,
                    ".git" | ".tokensave" | "target" | "node_modules" | "dist" | "build"
                ) {
                    continue;
                }
                stack.push(path);
            } else if file_type.is_file() && path.extension().and_then(|s| s.to_str()) == Some("rs")
            {
                let src = std::fs::read_to_string(&path)?;
                let Some(parent) = path.parent() else {
                    continue;
                };
                for include_path in include_macro_paths(&src) {
                    if !include_path.ends_with(".rs") {
                        continue;
                    }
                    targets.insert(normalize_path(&parent.join(include_path)));
                }
            }
        }
    }
    Ok(targets)
}

fn include_macro_paths(src: &str) -> Vec<String> {
    let marker = "include!(\"";
    let mut out = Vec::new();
    let mut rest = src;
    while let Some(pos) = rest.find(marker) {
        let path_start = pos + marker.len();
        let after_marker = &rest[path_start..];
        let Some(path_end) = string_literal_end(after_marker) else {
            break;
        };
        out.push(after_marker[..path_end].to_string());
        rest = &after_marker[path_end..];
    }
    out
}

fn string_literal_end(s: &str) -> Option<usize> {
    let mut escaped = false;
    for (idx, ch) in s.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match ch {
            '\\' => escaped = true,
            '"' => return Some(idx),
            _ => {}
        }
    }
    None
}

fn normalize_path(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

#[derive(Debug, Deserialize)]
struct CargoMetadata {
    packages: Vec<CargoPackage>,
}

#[derive(Debug, Deserialize)]
struct CargoPackage {
    name: String,
    manifest_path: PathBuf,
}

fn verify_directory_split(
    root: &Path,
    changed_files: &[PathBuf],
    trees: &[SplitWriteTree],
) -> Result<()> {
    if changed_files.is_empty() {
        eprintln!("[check] skipped: no files were split");
        return Ok(());
    }
    let Some(manifest) = find_manifest(root) else {
        eprintln!(
            "[check] skipped: no Cargo.toml found above {}",
            root.display()
        );
        return Ok(());
    };
    let workspace_root = manifest
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| root.to_path_buf());
    let packages = affected_packages(&workspace_root, changed_files)?;
    if packages.is_empty() {
        let mut failures = Vec::new();
        for args in [vec!["check"], vec!["test"], vec!["doc", "--no-deps"]] {
            if let Err(failure) = run_cargo_report(&workspace_root, &manifest, &args) {
                failures.push(*failure);
                break;
            }
        }
        if !failures.is_empty() {
            let reports = verify::fail_with_rollback_reports(root, trees, failures)?;
            anyhow::bail!(
                "[verify] cargo verification failed; wrote {} bug report(s): {}",
                reports.bug_reports.len(),
                display_paths(&reports.bug_reports)
            );
        }
        return Ok(());
    }
    let mut failures = Vec::new();
    for package in &packages {
        for args in [
            vec!["check", "-p", package.as_str()],
            vec!["test", "-p", package.as_str()],
            vec!["doc", "--no-deps", "-p", package.as_str()],
        ] {
            if let Err(failure) = run_cargo_report(&workspace_root, &manifest, &args) {
                failures.push(*failure);
                break;
            }
        }
    }
    if !failures.is_empty() {
        let reports = verify::fail_with_rollback_reports(root, trees, failures)?;
        anyhow::bail!(
            "[verify] cargo verification failed for {} package(s); wrote {} bug report(s): {}",
            reports.bug_reports.len(),
            reports.bug_reports.len(),
            display_paths(&reports.bug_reports)
        );
    }
    Ok(())
}

fn display_paths(paths: &[PathBuf]) -> String {
    paths
        .iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

fn affected_packages(root: &Path, changed_files: &[PathBuf]) -> Result<BTreeSet<String>> {
    let output = std::process::Command::new(cargo_program())
        .arg("metadata")
        .arg("--format-version")
        .arg("1")
        .arg("--no-deps")
        .current_dir(root)
        .output();
    let output = match output {
        Ok(output) => output,
        Err(e) => {
            anyhow::bail!("[check] cargo metadata failed to start: {e}");
        }
    };
    if !output.status.success() {
        anyhow::bail!(
            "[check] cargo metadata failed:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let metadata: CargoMetadata = serde_json::from_slice(&output.stdout)?;
    let changed: Vec<PathBuf> = changed_files
        .iter()
        .map(|path| path.canonicalize().unwrap_or_else(|_| path.clone()))
        .collect();
    let mut packages = BTreeSet::new();
    for package in metadata.packages {
        let manifest_dir = package
            .manifest_path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| root.to_path_buf());
        let manifest_dir = manifest_dir
            .canonicalize()
            .unwrap_or_else(|_| manifest_dir.clone());
        if changed.iter().any(|path| path.starts_with(&manifest_dir)) {
            packages.insert(package.name);
        }
    }
    Ok(packages)
}

fn run_cargo_report(
    root: &Path,
    manifest: &Path,
    args: &[&str],
) -> std::result::Result<(), Box<verify::CargoVerifyFailure>> {
    let command = format!("cargo {}", args.join(" "));
    let phase = args.first().copied().unwrap_or("check");
    eprintln!("[{phase}] {command}");
    let argv = cargo_argv_strings(args);
    let output = std::process::Command::new(cargo_program())
        .args(args)
        .current_dir(root)
        .output();
    let output = match output {
        Ok(output) => output,
        Err(e) => {
            return Err(Box::new(verify::CargoVerifyFailure::for_launch_error(
                Some(manifest.to_path_buf()),
                Vec::new(),
                argv,
                format!("[{phase}] {command} failed to start: {e}"),
            )));
        }
    };
    if output.status.success() {
        eprintln!("[{phase}] {command} ok");
        return Ok(());
    }
    eprintln!("[{phase}] {command} failed");
    if !output.stdout.is_empty() {
        eprintln!("--- {command} stdout ---");
        eprintln!("{}", String::from_utf8_lossy(&output.stdout));
    }
    if !output.stderr.is_empty() {
        eprintln!("--- {command} stderr ---");
        eprintln!("{}", String::from_utf8_lossy(&output.stderr));
    }
    Err(Box::new(verify::CargoVerifyFailure::for_command_output(
        Some(manifest.to_path_buf()),
        Vec::new(),
        argv,
        output,
    )))
}

fn cargo_argv_strings(args: &[&str]) -> Vec<String> {
    std::iter::once("cargo".to_string())
        .chain(args.iter().map(|arg| (*arg).to_string()))
        .collect()
}

fn cargo_program() -> OsString {
    if let Some(cargo) = std::env::var_os("CARGO") {
        return cargo;
    }
    if let Some(home) = std::env::var_os("HOME") {
        let candidate = PathBuf::from(home).join(".cargo/bin/cargo");
        if candidate.is_file() {
            return candidate.into_os_string();
        }
    }
    OsString::from("cargo")
}

fn find_manifest(start: &Path) -> Option<PathBuf> {
    let mut cur = if start.is_file() {
        start.parent()?.to_path_buf()
    } else {
        start.to_path_buf()
    };
    loop {
        let candidate = cur.join("Cargo.toml");
        if candidate.is_file() {
            return Some(candidate);
        }
        if !cur.pop() {
            return None;
        }
    }
}

fn load_evidence(path: &Path, items: &[ParsedItem]) -> Option<CrossFileEvidence> {
    let root = match Tokensave::locate(path) {
        Some(root) => root,
        None => {
            eprintln!("[tokensave] no .tokensave/ found above {}", path.display());
            return None;
        }
    };
    let ts = match Tokensave::open_safe(&root) {
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
}
