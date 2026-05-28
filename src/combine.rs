//! Combine two peer `.rs` files into a new parent module.
//!
//! Public API:
//!   * `combine_dry_run` — return the proposed plan without writing files.
//!   * `combine_write` — execute the combine with backups.

use anyhow::{Context, Result};
use quote::ToTokens;
use std::fs;
use std::path::{Path, PathBuf};

mod facade;
mod impact;
mod parent;
mod plan;
mod report;
mod rewrite;
mod suggest;
mod write;

pub use suggest::{CombineGroupSuggestion, CombineSuggestReport, SuggestOptions, suggest_groups};

#[derive(Debug, Clone)]
pub struct CombineOptions {
    pub module_name: Option<String>,
    pub write: bool,
    pub force: bool,
    pub json: bool,
    pub preview_impacts: bool,
    pub use_tokensave: bool,
    pub re_export_filter: Option<String>,
    pub rewrite_consumers: bool,
    pub preview_consumer_rewrites: bool,
}

#[derive(Debug, serde::Serialize)]
pub struct CombineDryRunReport {
    pub module_name: String,
    pub facade_path: std::path::PathBuf,
    pub facade_content: String,
    pub rewrites: Vec<RewritePreview>,
    pub moved_files: Vec<MovedFileReport>,
    pub parent_update: Option<ParentUpdateReport>,
    pub impact: Option<impact::ImpactReport>,
    pub consumer_rewrites: Vec<ConsumerRewritePreview>,
    pub skipped_consumer_rewrites: Vec<impact::SkippedConsumerRewrite>,
    pub planned_backups: Vec<PathBuf>,
    pub manifest: write::OperationManifest,
}

#[derive(Debug, serde::Serialize)]
pub struct RewritePreview {
    pub from: std::path::PathBuf,
    pub to: std::path::PathBuf,
    pub content: String,
}

#[derive(Debug, serde::Serialize)]
pub struct MovedFileReport {
    pub from: std::path::PathBuf,
    pub to: std::path::PathBuf,
}

#[derive(Debug, serde::Serialize)]
pub struct ParentUpdateReport {
    pub path: std::path::PathBuf,
    pub add: String,
    pub remove: Vec<String>,
}

#[derive(Debug, serde::Serialize)]
pub struct ConsumerRewritePreview {
    pub file: std::path::PathBuf,
    pub replacements: usize,
    pub hunks: Vec<impact::ConsumerRewriteHunk>,
    pub new_source: String,
}

struct ModuleWork {
    path: PathBuf,
    stem: String,
    ast: syn::File,
    rewritten_src: String,
}

/// Dry-run: parse inputs, generate facade, rewrite paths, return report.
pub fn combine_dry_run(file1: &Path, file2: &Path, opts: &CombineOptions) -> Result<String> {
    combine_dry_run_many(&[file1.to_path_buf(), file2.to_path_buf()], opts)
}

pub fn combine_dry_run_many(files: &[PathBuf], opts: &CombineOptions) -> Result<String> {
    let plan = plan::build_plan_many(files, opts.module_name.as_deref())?;
    let modules = prepare_modules(&plan)?;

    let filter = opts.re_export_filter.as_deref();
    let facade_inputs: Vec<(&str, &syn::File)> = modules
        .iter()
        .map(|module| (module.stem.as_str(), &module.ast))
        .collect();
    let facade_ast = facade::generate_facade_many(&plan.module_name, &facade_inputs, filter)?;
    let facade_src = facade_ast.to_token_stream().to_string();

    // Impact report (optional)
    let impact = if opts.preview_impacts {
        impact::generate_impact_report_many(&plan.files, &plan.module_name, opts.use_tokensave)?
    } else {
        None
    };
    let consumer_rewrite_plan = if opts.preview_consumer_rewrites {
        Some(impact::plan_consumer_rewrites_many(
            &plan.files,
            plan.parent_module.as_deref(),
            &plan.target_dir,
            &plan.module_name,
        )?)
    } else {
        None
    };
    let consumer_rewrites = if let Some(plan) = &consumer_rewrite_plan {
        plan.rewrites
            .iter()
            .cloned()
            .map(|rewrite| ConsumerRewritePreview {
                file: rewrite.file,
                replacements: rewrite.replacements,
                hunks: rewrite.hunks,
                new_source: rewrite.new_source,
            })
            .collect()
    } else {
        Vec::new()
    };
    let skipped_consumer_rewrites = if let Some(plan) = consumer_rewrite_plan {
        plan.skipped
    } else {
        Vec::new()
    };

    let consumer_rewrites = consumer_rewrites
        .into_iter()
        .collect::<Vec<ConsumerRewritePreview>>();

    let (manifest, planned_backups) =
        dry_run_operation_preview(&plan, &consumer_rewrites, plan.parent_module.is_some())?;

    if opts.json {
        let report = CombineDryRunReport {
            module_name: plan.module_name.clone(),
            facade_path: plan.facade_path.clone(),
            facade_content: facade_src.clone(),
            rewrites: rewrite_previews(&plan, &modules),
            moved_files: moved_file_reports(&plan),
            parent_update: plan.parent_module.as_ref().map(|p| ParentUpdateReport {
                path: p.clone(),
                add: format!("mod {};", plan.module_name),
                remove: modules
                    .iter()
                    .map(|module| format!("mod {};", module.stem))
                    .collect(),
            }),
            impact,
            consumer_rewrites,
            skipped_consumer_rewrites,
            planned_backups,
            manifest,
        };
        Ok(serde_json::to_string_pretty(&report)?)
    } else {
        let mut report = report::human_report(&plan, &facade_src);
        for module in &modules {
            report.push_str(&format!(
                "\n== rewritten {} ==\n{}\n",
                plan.target_dir
                    .join(module.path.file_name().unwrap())
                    .display(),
                module.rewritten_src
            ));
        }
        if let Some(impact_report) = impact {
            report.push_str(&format!("\n[impact] {}\n", impact_report.message));
            for c in &impact_report.consumers {
                report.push_str(&format!(
                    "[impact] {}:{} `{}` -> `{}` ({})\n",
                    c.file.display(),
                    c.line,
                    c.old,
                    c.new,
                    c.symbol
                ));
            }
        }
        if !consumer_rewrites.is_empty() {
            report.push_str("\n[consumer-rewrites] planned updates:\n");
            for rewrite in &consumer_rewrites {
                report.push_str(&format!(
                    "[consumer-rewrites] {} ({} replacement(s))\n",
                    rewrite.file.display(),
                    rewrite.replacements
                ));
                for hunk in &rewrite.hunks {
                    report.push_str(&format!(
                        "  L{}: {}\n      -> {}\n",
                        hunk.line, hunk.old, hunk.new
                    ));
                }
            }
        }
        if !skipped_consumer_rewrites.is_empty() {
            report.push_str("\n[consumer-rewrites] skipped candidates:\n");
            for skipped in &skipped_consumer_rewrites {
                report.push_str(&format!(
                    "[consumer-rewrites] {}:{} `{}` ({})\n",
                    skipped.file.display(),
                    skipped.line,
                    skipped.old,
                    skipped.reason
                ));
            }
        }
        report.push_str(&format!(
            "\n[manifest] would write {} file(s), remove {} file(s), backup {} file(s)\n",
            manifest.written_files.len(),
            manifest.removed_files.len(),
            planned_backups.len()
        ));
        Ok(report)
    }
}

/// Write: execute the combine operation with backups.
pub fn combine_write(
    file1: &Path,
    file2: &Path,
    opts: &CombineOptions,
) -> Result<write::CombineWriteReport> {
    combine_write_many(&[file1.to_path_buf(), file2.to_path_buf()], opts)
}

pub fn combine_write_many(
    files: &[PathBuf],
    opts: &CombineOptions,
) -> Result<write::CombineWriteReport> {
    let plan = plan::build_plan_many(files, opts.module_name.as_deref())?;
    let modules = prepare_modules(&plan)?;

    let filter = opts.re_export_filter.as_deref();
    let facade_inputs: Vec<(&str, &syn::File)> = modules
        .iter()
        .map(|module| (module.stem.as_str(), &module.ast))
        .collect();
    let facade_ast = facade::generate_facade_many(&plan.module_name, &facade_inputs, filter)?;
    let facade_src = facade_ast.to_token_stream().to_string();

    // Parent module update
    let parent_src = if let Some(parent_path) = &plan.parent_module {
        let stems = modules
            .iter()
            .map(|module| module.stem.as_str())
            .collect::<Vec<_>>();
        let updated = parent::update_parent_module(parent_path, &plan.module_name, &stems)?;
        Some(updated)
    } else {
        None
    };

    let write_opts = write::WriteOptions { force: opts.force };

    let consumer_rewrite_plan = if opts.rewrite_consumers {
        impact::plan_consumer_rewrites_many(
            &plan.files,
            plan.parent_module.as_deref(),
            &plan.target_dir,
            &plan.module_name,
        )?
    } else {
        impact::ConsumerRewritePlan {
            rewrites: Vec::new(),
            skipped: Vec::new(),
        }
    };

    let file_srcs = modules
        .iter()
        .map(|module| module.rewritten_src.clone())
        .collect::<Vec<_>>();
    let report = write::execute_write(
        &plan,
        &facade_src,
        &file_srcs,
        parent_src.as_deref(),
        &consumer_rewrite_plan,
        &write_opts,
    )?;

    Ok(report)
}

fn prepare_modules(plan: &plan::CombinePlan) -> Result<Vec<ModuleWork>> {
    let stems: Vec<String> = plan
        .files
        .iter()
        .map(|path| {
            path.file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or_default()
                .to_string()
        })
        .collect();

    plan.files
        .iter()
        .zip(stems.iter())
        .map(|(path, stem)| {
            let src =
                fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
            let ast = syn::parse_file(&src).with_context(|| format!("parse {}", path.display()))?;
            let mut rewritten_ast = ast.clone();
            let other_stems: Vec<&str> = stems
                .iter()
                .filter(|candidate| *candidate != stem)
                .map(String::as_str)
                .collect();
            rewrite::rewrite_paths_many(&mut rewritten_ast, &other_stems, &plan.module_name);
            let rewritten_src = rewrite::render_ast(&rewritten_ast);
            Ok(ModuleWork {
                path: path.clone(),
                stem: stem.clone(),
                ast,
                rewritten_src,
            })
        })
        .collect()
}

fn rewrite_previews(plan: &plan::CombinePlan, modules: &[ModuleWork]) -> Vec<RewritePreview> {
    modules
        .iter()
        .map(|module| RewritePreview {
            from: module.path.clone(),
            to: plan.target_dir.join(module.path.file_name().unwrap()),
            content: module.rewritten_src.clone(),
        })
        .collect()
}

fn moved_file_reports(plan: &plan::CombinePlan) -> Vec<MovedFileReport> {
    plan.files
        .iter()
        .map(|path| MovedFileReport {
            from: path.clone(),
            to: plan.target_dir.join(path.file_name().unwrap()),
        })
        .collect()
}

fn dry_run_operation_preview(
    plan: &plan::CombinePlan,
    consumer_rewrites: &[ConsumerRewritePreview],
    updates_parent: bool,
) -> Result<(write::OperationManifest, Vec<PathBuf>)> {
    let file_dsts: Vec<PathBuf> = plan
        .files
        .iter()
        .map(|file| {
            file.file_name()
                .map(|name| plan.target_dir.join(name))
                .context("input file has no filename")
        })
        .collect::<Result<_>>()?;
    let mut planned_targets = Vec::with_capacity(file_dsts.len() + 1);
    planned_targets.push(plan.facade_path.clone());
    planned_targets.extend(file_dsts.iter().cloned());

    let mut manifest = write::OperationManifest::default();
    if plan.target_dir.exists() {
        for entry in fs::read_dir(&plan.target_dir)
            .with_context(|| format!("read_dir {}", plan.target_dir.display()))?
        {
            let entry = entry?;
            let path = entry.path();
            if path.is_file() && !planned_targets.iter().any(|p| same_path(p, &path)) {
                manifest.preserved_files.push(path);
            }
        }
    } else {
        manifest.created_dirs.push(plan.target_dir.clone());
    }

    if updates_parent && let Some(parent_path) = &plan.parent_module {
        manifest.written_files.push(parent_path.clone());
        manifest.updated_files.push(parent_path.clone());
    }
    manifest.written_files.push(plan.facade_path.clone());
    manifest.written_files.extend(file_dsts.iter().cloned());
    manifest.removed_files.extend(plan.files.iter().cloned());
    for rewrite in consumer_rewrites {
        manifest.written_files.push(rewrite.file.clone());
        manifest.updated_files.push(rewrite.file.clone());
    }

    let mut planned_backups = Vec::new();
    for path in &planned_targets {
        if path.exists() {
            planned_backups.push(parent::make_backup_path(path)?);
        }
    }
    if updates_parent && let Some(parent_path) = &plan.parent_module {
        planned_backups.push(parent::make_backup_path(parent_path)?);
    }
    for file in &plan.files {
        planned_backups.push(parent::make_backup_path(file)?);
    }
    for rewrite in consumer_rewrites {
        planned_backups.push(parent::make_backup_path(&rewrite.file)?);
    }
    planned_backups.sort();
    planned_backups.dedup();

    Ok((manifest, planned_backups))
}

fn same_path(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}
