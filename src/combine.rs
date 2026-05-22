//! Combine two peer `.rs` files into a new parent module.
//!
//! Public API:
//!   * `combine_dry_run` — return the proposed plan without writing files.
//!   * `combine_write` — execute the combine with backups.

use anyhow::{Context, Result};
use quote::ToTokens;
use std::fs;
use std::path::Path;

mod facade;
mod impact;
mod parent;
mod plan;
mod report;
mod rewrite;
mod write;

#[derive(Debug, Clone)]
pub struct CombineOptions {
    pub module_name: Option<String>,
    pub write: bool,
    pub force: bool,
    pub json: bool,
    pub preview_impacts: bool,
    pub use_tokensave: bool,
    pub re_export_filter: Option<String>,
}

#[derive(Debug, serde::Serialize)]
pub struct CombineDryRunReport {
    pub module_name: String,
    pub facade_path: std::path::PathBuf,
    pub facade_content: String,
    pub moved_files: Vec<MovedFileReport>,
    pub parent_update: Option<ParentUpdateReport>,
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

/// Dry-run: parse inputs, generate facade, rewrite paths, return report.
pub fn combine_dry_run(file1: &Path, file2: &Path, opts: &CombineOptions) -> Result<String> {
    let plan = plan::build_plan(file1, file2, opts.module_name.as_deref())?;

    let src1 = fs::read_to_string(&plan.file1)
        .with_context(|| format!("read {}", plan.file1.display()))?;
    let src2 = fs::read_to_string(&plan.file2)
        .with_context(|| format!("read {}", plan.file2.display()))?;

    let ast1 = syn::parse_file(&src1)
        .with_context(|| format!("parse {}", plan.file1.display()))?;
    let ast2 = syn::parse_file(&src2)
        .with_context(|| format!("parse {}", plan.file2.display()))?;

    let stem1 = plan.file1.file_stem().unwrap().to_str().unwrap();
    let stem2 = plan.file2.file_stem().unwrap().to_str().unwrap();

    let filter = opts.re_export_filter.as_deref();
    let facade_ast = facade::generate_facade(&plan.module_name, stem1, stem2, &ast1, &ast2, filter)?;
    let facade_src = facade_ast.to_token_stream().to_string();

    // Rewrite paths in both files
    let mut rewritten_ast1 = ast1.clone();
    let mut rewritten_ast2 = ast2.clone();
    rewrite::rewrite_paths(&mut rewritten_ast1, stem2, &plan.module_name);
    rewrite::rewrite_paths(&mut rewritten_ast2, stem1, &plan.module_name);
    let _rewritten_src1 = rewrite::render_ast(&rewritten_ast1);
    let _rewritten_src2 = rewrite::render_ast(&rewritten_ast2);

    // Impact report (optional)
    let impact = if opts.preview_impacts {
        impact::generate_impact_report(file1, file2, &plan.module_name, opts.use_tokensave)?
    } else {
        None
    };

    if opts.json {
        let report = CombineDryRunReport {
            module_name: plan.module_name.clone(),
            facade_path: plan.facade_path.clone(),
            facade_content: facade_src.clone(),
            moved_files: vec![
                MovedFileReport {
                    from: plan.file1.clone(),
                    to: plan.target_dir.join(plan.file1.file_name().unwrap()),
                },
                MovedFileReport {
                    from: plan.file2.clone(),
                    to: plan.target_dir.join(plan.file2.file_name().unwrap()),
                },
            ],
            parent_update: plan.parent_module.as_ref().map(|p| {
                ParentUpdateReport {
                    path: p.clone(),
                    add: format!("mod {};", plan.module_name),
                    remove: vec![format!("mod {};", stem1), format!("mod {};", stem2)],
                }
            }),
        };
        let mut json = serde_json::to_string_pretty(&report)?;
        if let Some(impact_msg) = impact {
            json.push_str(&format!("\n\n// impact preview: {}\n", impact_msg));
        }
        Ok(json)
    } else {
        let mut report = report::human_report(&plan, &facade_src);
        report.push_str(&format!("\n[rewrite] {} -> path-adjusted\n", plan.file1.display()));
        report.push_str(&format!("[rewrite] {} -> path-adjusted\n", plan.file2.display()));
        if let Some(impact_msg) = impact {
            report.push_str(&format!("\n[impact] {}\n", impact_msg));
        }
        Ok(report)
    }
}

/// Write: execute the combine operation with backups.
pub fn combine_write(file1: &Path, file2: &Path, opts: &CombineOptions) -> Result<write::CombineWriteReport> {
    let plan = plan::build_plan(file1, file2, opts.module_name.as_deref())?;

    let src1 = fs::read_to_string(&plan.file1)
        .with_context(|| format!("read {}", plan.file1.display()))?;
    let src2 = fs::read_to_string(&plan.file2)
        .with_context(|| format!("read {}", plan.file2.display()))?;

    let ast1 = syn::parse_file(&src1)
        .with_context(|| format!("parse {}", plan.file1.display()))?;
    let ast2 = syn::parse_file(&src2)
        .with_context(|| format!("parse {}", plan.file2.display()))?;

    let stem1 = plan.file1.file_stem().unwrap().to_str().unwrap();
    let stem2 = plan.file2.file_stem().unwrap().to_str().unwrap();

    let filter = opts.re_export_filter.as_deref();
    let facade_ast = facade::generate_facade(&plan.module_name, stem1, stem2, &ast1, &ast2, filter)?;
    let facade_src = facade_ast.to_token_stream().to_string();

    // Rewrite paths
    let mut rewritten_ast1 = ast1.clone();
    let mut rewritten_ast2 = ast2.clone();
    rewrite::rewrite_paths(&mut rewritten_ast1, stem2, &plan.module_name);
    rewrite::rewrite_paths(&mut rewritten_ast2, stem1, &plan.module_name);
    let rewritten_src1 = rewrite::render_ast(&rewritten_ast1);
    let rewritten_src2 = rewrite::render_ast(&rewritten_ast2);

    // Parent module update
    let parent_src = if let Some(parent_path) = &plan.parent_module {
        let updated = parent::update_parent_module(parent_path, &plan.module_name, &[stem1, stem2])?;
        Some(updated)
    } else {
        None
    };

    let write_opts = write::WriteOptions {
        force: opts.force,
    };

    let report = write::execute_write(&plan, &facade_src, &rewritten_src1, &rewritten_src2, parent_src.as_deref(), &write_opts)?;

    Ok(report)
}
