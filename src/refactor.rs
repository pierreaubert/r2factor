//! Safe whole-codebase refactor engine.
//!
//! The implementation intentionally starts conservative: it builds the
//! workspace/symbol/migration model, previews edits in a temp tree, and only
//! writes plans that pass verification.

use crate::discover::{Discovery, GlobalItem, GlobalItemId};
use crate::global_plan;
use crate::item::ItemVis;
use crate::llm::LlmConfig;
use crate::spec::{Spec, validate};
use crate::symbol_graph::{EvidenceConfidence, SymbolGraph, TokenSaveStatus, build_symbol_graph};
use crate::workspace_model::{FileId, WorkspaceModel, build_workspace_model};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use syn::spanned::Spanned;

#[derive(Debug, Clone)]
pub struct RefactorOptions {
    pub use_tokensave: bool,
    pub require_tokensave_for_rewrites: bool,
    pub use_rg_candidates: bool,
    pub llm: Option<LlmConfig>,
    pub run_full_tests: bool,
    pub mode: RefactorMode,
}

impl Default for RefactorOptions {
    fn default() -> Self {
        Self {
            use_tokensave: true,
            require_tokensave_for_rewrites: true,
            use_rg_candidates: true,
            llm: None,
            run_full_tests: false,
            mode: RefactorMode::Plan,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefactorMode {
    Plan,
    Preview,
    Write,
}

#[derive(Debug, Clone, Serialize)]
pub struct MigrationPlan {
    pub workspace: WorkspaceSummary,
    pub assignments: Vec<ItemAssignment>,
    pub diagnostics: Vec<PlanDiagnostic>,
    pub edits: Vec<PlanEdit>,
    pub safety: PlanSafety,
}

#[derive(Debug, Clone, Serialize)]
pub struct ItemAssignment {
    pub item: GlobalItemId,
    pub name: String,
    pub from_file: FileId,
    pub from_path: PathBuf,
    pub to_file: PathBuf,
    pub module_name: String,
    pub visibility: String,
    pub rationale: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct PlanDiagnostic {
    pub severity: DiagnosticSeverity,
    pub kind: String,
    pub message: String,
    pub path: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticSeverity {
    Info,
    Warning,
    Error,
}

#[derive(Debug, Clone, Serialize)]
pub struct PlanSafety {
    pub can_preview: bool,
    pub can_write: bool,
    pub blockers: Vec<String>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "edit", rename_all = "snake_case")]
pub enum PlanEdit {
    CreateFile {
        path: PathBuf,
        contents: String,
    },
    MoveItem {
        item: GlobalItemId,
        from: FileId,
        to: PathBuf,
        span: SourceSpan,
    },
    InsertModDecl {
        file: FileId,
        module: String,
        declaration: String,
    },
    RewritePath {
        file: FileId,
        span: SourceSpan,
        old: String,
        new: String,
        evidence: EvidenceConfidence,
    },
    AdjustVisibility {
        item: GlobalItemId,
        visibility: String,
    },
    AddUse {
        file: FileId,
        path: String,
    },
    AddReExport {
        file: FileId,
        path: String,
    },
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct SourceSpan {
    pub line_start: usize,
    pub line_end: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct PreviewReport {
    pub plan: MigrationPlan,
    pub temp_root: PathBuf,
    pub changed_files: Vec<PathBuf>,
    pub diff: String,
    pub checks: Vec<CheckResult>,
    pub passed: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct CheckResult {
    pub command: String,
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct WriteReport {
    pub manifest: WriteManifest,
    pub manifest_path: PathBuf,
    pub preview: PreviewReport,
    pub post_write_checks: Vec<CheckResult>,
    pub rolled_back: bool,
    pub passed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WriteManifest {
    pub root: PathBuf,
    pub backups: Vec<PathBuf>,
    pub created_files: Vec<PathBuf>,
    pub changed_files: Vec<PathBuf>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RollbackReport {
    pub restored: Vec<PathBuf>,
}

#[derive(Debug, Clone, Serialize)]
pub struct WorkspaceSummary {
    pub root: PathBuf,
    pub packages: usize,
    pub targets: usize,
    pub files: usize,
    pub modules: usize,
}

#[derive(Debug, Serialize)]
struct AdvisorPayload<'a> {
    items: Vec<AdvisorPayloadItem<'a>>,
    evidence: AdvisorEvidenceSummary,
    diagnostics: Vec<&'a str>,
}

#[derive(Debug, Serialize)]
struct AdvisorPayloadItem<'a> {
    id: GlobalItemId,
    name: &'a str,
    kind: String,
    signature: String,
    from_file: &'a Path,
    assigned_module: &'a str,
    evidence_counts: BTreeMap<&'static str, usize>,
}

#[derive(Debug, Serialize)]
struct AdvisorEvidenceSummary {
    tokensave: String,
    exact_or_semantic_edges: usize,
    scoped_candidates: usize,
    text_candidates: usize,
    ambiguous: usize,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct AdvisorReply {
    renames: BTreeMap<String, String>,
    moves: Vec<AdvisorMove>,
}

#[derive(Debug, Deserialize)]
struct AdvisorMove {
    id: GlobalItemId,
    to: String,
}

pub fn refactor_plan(root: &Path, spec: &Spec, opts: &RefactorOptions) -> Result<MigrationPlan> {
    let report = validate(spec);
    if !report.is_valid() {
        bail!("spec is invalid: {}", report.errors.join("; "));
    }

    let workspace = build_workspace_model(root)?;
    let discovery = crate::discover::discover_dir(root, opts.use_tokensave)?;
    let symbols = build_symbol_graph(
        &workspace,
        &discovery,
        opts.use_tokensave,
        opts.use_rg_candidates,
    );
    let mut global = global_plan::build_plan(&discovery, spec);

    let mut diagnostics = collect_diagnostics(&workspace, &symbols);
    for violation in &global.violations {
        diagnostics.push(PlanDiagnostic {
            severity: match violation.kind {
                global_plan::ViolationKind::NameCollision => DiagnosticSeverity::Error,
                global_plan::ViolationKind::BoundaryViolation => DiagnosticSeverity::Warning,
            },
            kind: format!("{:?}", violation.kind),
            message: violation.message.clone(),
            path: None,
        });
    }
    if let Some(llm) = opts.llm.as_ref() {
        if spec.llm.allow_advice {
            match apply_llm_advisor(llm, spec, &discovery, &symbols, &mut global.assignments) {
                Ok(mut advisor_diagnostics) => diagnostics.append(&mut advisor_diagnostics),
                Err(e) => diagnostics.push(PlanDiagnostic {
                    severity: DiagnosticSeverity::Warning,
                    kind: "llm_advisor_failed".to_string(),
                    message: format!("local LLM advisor failed; deterministic plan kept: {e}"),
                    path: None,
                }),
            }
        } else {
            diagnostics.push(PlanDiagnostic {
                severity: DiagnosticSeverity::Info,
                kind: "llm_advisor_disabled_by_spec".to_string(),
                message: "local LLM config was provided, but spec.llm.allow_advice is false"
                    .to_string(),
                path: None,
            });
        }
    }

    let assignments = global
        .assignments
        .iter()
        .map(|(id, assignment)| {
            let item = &discovery.items[*id];
            ItemAssignment {
                item: *id,
                name: item.name.clone(),
                from_file: item.file_id,
                from_path: discovery.files[item.file_id].path.clone(),
                to_file: assignment.target_file.clone(),
                module_name: assignment.module_name.clone(),
                visibility: assignment.visibility.clone(),
                rationale: assignment.rationale.clone(),
            }
        })
        .collect();

    let edits = derive_migration_edits(&workspace, &discovery, &global.assignments);
    for edit in &edits {
        if let PlanEdit::CreateFile { path, .. } = edit
            && path.exists()
        {
            diagnostics.push(PlanDiagnostic {
                severity: DiagnosticSeverity::Error,
                kind: "target_exists".to_string(),
                message: format!("target file already exists: {}", path.display()),
                path: Some(path.clone()),
            });
        }
    }
    let safety = safety_for_plan(opts, &symbols, &diagnostics, &edits);

    Ok(MigrationPlan {
        workspace: workspace_summary(&workspace),
        assignments,
        diagnostics,
        edits,
        safety,
    })
}

pub fn refactor_preview(root: &Path, spec: &Spec, opts: &RefactorOptions) -> Result<PreviewReport> {
    let mut opts = opts.clone();
    opts.mode = RefactorMode::Preview;
    let plan = refactor_plan(root, spec, &opts)?;
    if !plan.safety.can_preview {
        bail!(
            "plan cannot be previewed: {}",
            plan.safety.blockers.join("; ")
        );
    }

    let original_root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let temp = tempfile::tempdir().context("create preview tempdir")?;
    let temp_root = temp.path().join(
        original_root
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("workspace"),
    );
    copy_tree(&original_root, &temp_root)?;
    let changed_files = apply_plan_to_root(&plan, &original_root, &temp_root)?;
    let checks = run_preview_checks(&temp_root, &changed_files, true, opts.run_full_tests);
    let passed = checks.iter().all(|c| c.success);
    let diff = render_diff(&original_root, &temp_root, &changed_files);
    let temp_root = temp.keep().join(
        original_root
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("workspace"),
    );

    Ok(PreviewReport {
        plan,
        temp_root,
        changed_files,
        diff,
        checks,
        passed,
    })
}

pub fn refactor_write(root: &Path, spec: &Spec, opts: &RefactorOptions) -> Result<WriteReport> {
    let mut opts = opts.clone();
    opts.mode = RefactorMode::Write;
    let preview = refactor_preview(root, spec, &opts)?;
    if !preview.passed {
        bail!("preview checks failed; refusing to write");
    }
    if !preview.plan.safety.can_write {
        bail!(
            "plan is not write-safe: {}",
            preview.plan.safety.blockers.join("; ")
        );
    }

    let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let changed_paths = planned_changed_paths(&preview.plan);
    let mut backups = Vec::new();
    let mut created_files = Vec::new();
    for path in &changed_paths {
        if path.exists() {
            let backup = backup_path(path);
            fs::copy(path, &backup)
                .with_context(|| format!("backup {} -> {}", path.display(), backup.display()))?;
            backups.push(backup);
        } else {
            created_files.push(path.clone());
        }
    }

    let changed_files = apply_plan_to_root(&preview.plan, &root, &root)?;
    let manifest = WriteManifest {
        root: root.clone(),
        backups,
        created_files,
        changed_files,
    };
    let manifest_path = root.join(".r2factor-refactor-manifest.json");
    fs::write(&manifest_path, serde_json::to_string_pretty(&manifest)?)
        .with_context(|| format!("write {}", manifest_path.display()))?;
    let post_write_checks =
        run_preview_checks(&root, &manifest.changed_files, false, opts.run_full_tests);
    let passed = post_write_checks.iter().all(|c| c.success);
    let rolled_back = if passed {
        false
    } else {
        let _ = refactor_rollback(&manifest);
        true
    };
    Ok(WriteReport {
        manifest,
        manifest_path,
        preview,
        post_write_checks,
        rolled_back,
        passed,
    })
}

pub fn refactor_rollback(manifest: &WriteManifest) -> Result<RollbackReport> {
    let mut restored = Vec::new();
    for created in manifest.created_files.iter().rev() {
        if created.exists() {
            fs::remove_file(created).with_context(|| format!("remove {}", created.display()))?;
        }
    }
    for backup in &manifest.backups {
        let Some(name) = backup.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        let Some(original) = name.strip_suffix(".bak") else {
            continue;
        };
        let target = backup.with_file_name(original);
        fs::copy(backup, &target)
            .with_context(|| format!("restore {} -> {}", backup.display(), target.display()))?;
        restored.push(target);
    }
    Ok(RollbackReport { restored })
}

pub fn refactor_rollback_manifest(path: &Path) -> Result<RollbackReport> {
    let src = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let manifest: WriteManifest = serde_json::from_str(&src).context("parse write manifest")?;
    refactor_rollback(&manifest)
}

fn collect_diagnostics(workspace: &WorkspaceModel, symbols: &SymbolGraph) -> Vec<PlanDiagnostic> {
    let mut out = Vec::new();
    for diagnostic in &workspace.diagnostics {
        out.push(PlanDiagnostic {
            severity: DiagnosticSeverity::Warning,
            kind: diagnostic.kind.clone(),
            message: diagnostic.message.clone(),
            path: diagnostic.path.clone(),
        });
    }
    for diagnostic in &symbols.diagnostics {
        out.push(PlanDiagnostic {
            severity: DiagnosticSeverity::Warning,
            kind: diagnostic.kind.clone(),
            message: diagnostic.message.clone(),
            path: diagnostic.path.clone(),
        });
    }
    if let TokenSaveStatus::Unavailable { message } = &symbols.tokensave {
        out.push(PlanDiagnostic {
            severity: DiagnosticSeverity::Warning,
            kind: "tokensave_unavailable".to_string(),
            message: message.clone(),
            path: None,
        });
    }
    out
}

fn apply_llm_advisor(
    cfg: &LlmConfig,
    spec: &Spec,
    discovery: &Discovery,
    symbols: &SymbolGraph,
    assignments: &mut BTreeMap<GlobalItemId, global_plan::ModuleAssignment>,
) -> Result<Vec<PlanDiagnostic>> {
    let payload = advisor_payload(discovery, symbols, assignments);
    let user_payload = serde_json::to_string(&payload)?;
    let request = serde_json::json!({
        "model": cfg.model,
        "messages": [
            {
                "role": "system",
                "content": "\
    You advise a Rust refactor planner. You may only propose assignment changes. \
    Return strict JSON: {\"renames\":{\"old_module\":\"new_module\"},\"moves\":[{\"id\":0,\"to\":\"module\"}]}. \
    Do not emit code, paths, text edits, confidence changes, blockers, or test commands. Unknown ids are ignored."
            },
            {
                "role": "user",
                "content": user_payload
            }
        ],
        "response_format": {"type": "json_object"},
        "temperature": 0.0,
        "stream": false
    });
    let response = send_llm_request(cfg, request)?;
    let reply = parse_advisor_reply(&response)?;
    Ok(apply_advisor_reply(spec, discovery, assignments, reply))
}

fn advisor_payload<'a>(
    discovery: &'a Discovery,
    symbols: &'a SymbolGraph,
    assignments: &'a BTreeMap<GlobalItemId, global_plan::ModuleAssignment>,
) -> AdvisorPayload<'a> {
    let mut items = Vec::new();
    for item in &discovery.items {
        let Some(assignment) = assignments.get(&item.id) else {
            continue;
        };
        let parsed = discovery.files[item.file_id]
            .items
            .iter()
            .find(|parsed| parsed.id == item.local_id);
        items.push(AdvisorPayloadItem {
            id: item.id,
            name: &item.name,
            kind: format!("{:?}", item.kind),
            signature: parsed
                .map(|parsed| truncate_chars(parsed.signature(), 180))
                .unwrap_or_default(),
            from_file: &discovery.files[item.file_id].path,
            assigned_module: &assignment.module_name,
            evidence_counts: evidence_counts_for_item(symbols, item.id),
        });
    }
    AdvisorPayload {
        items,
        evidence: advisor_evidence_summary(symbols),
        diagnostics: symbols
            .diagnostics
            .iter()
            .map(|diagnostic| diagnostic.message.as_str())
            .collect(),
    }
}

fn evidence_counts_for_item(
    symbols: &SymbolGraph,
    item_id: GlobalItemId,
) -> BTreeMap<&'static str, usize> {
    let mut out = BTreeMap::new();
    for edge in &symbols.edges {
        if edge.from.item_id != Some(item_id) {
            continue;
        }
        let key = match edge.confidence {
            EvidenceConfidence::Exact => "exact",
            EvidenceConfidence::Semantic => "semantic",
            EvidenceConfidence::ScopedCandidate => "scoped_candidate",
            EvidenceConfidence::TextCandidate => "text_candidate",
            EvidenceConfidence::Ambiguous => "ambiguous",
            EvidenceConfidence::Unresolved => "unresolved",
        };
        *out.entry(key).or_default() += 1;
    }
    out
}

fn advisor_evidence_summary(symbols: &SymbolGraph) -> AdvisorEvidenceSummary {
    AdvisorEvidenceSummary {
        tokensave: match &symbols.tokensave {
            TokenSaveStatus::Available { .. } => "available".to_string(),
            TokenSaveStatus::Unavailable { message } => format!("unavailable: {message}"),
        },
        exact_or_semantic_edges: symbols
            .edges
            .iter()
            .filter(|edge| {
                matches!(
                    edge.confidence,
                    EvidenceConfidence::Exact | EvidenceConfidence::Semantic
                )
            })
            .count(),
        scoped_candidates: symbols
            .edges
            .iter()
            .filter(|edge| edge.confidence == EvidenceConfidence::ScopedCandidate)
            .count(),
        text_candidates: symbols
            .edges
            .iter()
            .filter(|edge| edge.confidence == EvidenceConfidence::TextCandidate)
            .count(),
        ambiguous: symbols
            .edges
            .iter()
            .filter(|edge| edge.confidence == EvidenceConfidence::Ambiguous)
            .count(),
    }
}

fn send_llm_request(cfg: &LlmConfig, body: serde_json::Value) -> Result<String> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(std::time::Duration::from_secs(cfg.timeout_secs)))
        .build()
        .into();
    let mut request = agent
        .post(&cfg.endpoint)
        .header("content-type", "application/json");
    if let Some(key) = cfg.api_key.as_deref()
        && !key.is_empty()
    {
        request = request.header("authorization", &format!("Bearer {key}"));
    }
    let response: serde_json::Value = request
        .send_json(body)
        .with_context(|| format!("POST {}", cfg.endpoint))?
        .body_mut()
        .read_json()
        .context("decode local LLM response")?;
    response["choices"][0]["message"]["content"]
        .as_str()
        .map(str::to_string)
        .context("local LLM response missing choices[0].message.content")
}

fn parse_advisor_reply(raw: &str) -> Result<AdvisorReply> {
    let start = raw
        .find('{')
        .context("local LLM advisor reply did not contain JSON")?;
    let mut depth = 0usize;
    let mut end = None;
    for (offset, ch) in raw[start..].char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    end = Some(start + offset + 1);
                    break;
                }
            }
            _ => {}
        }
    }
    let end = end.context("local LLM advisor reply contained unbalanced JSON")?;
    serde_json::from_str(&raw[start..end]).context("parse local LLM advisor JSON")
}

fn apply_advisor_reply(
    spec: &Spec,
    discovery: &Discovery,
    assignments: &mut BTreeMap<GlobalItemId, global_plan::ModuleAssignment>,
    reply: AdvisorReply,
) -> Vec<PlanDiagnostic> {
    let mut diagnostics = Vec::new();
    let valid_ids: BTreeSet<GlobalItemId> = discovery.items.iter().map(|item| item.id).collect();

    for (old, new_name) in reply.renames {
        let new_name = crate::names::sanitize_module(&new_name);
        let affected: Vec<GlobalItemId> = assignments
            .iter()
            .filter(|(_, assignment)| assignment.module_name == old)
            .map(|(id, _)| *id)
            .collect();
        if affected.is_empty() {
            diagnostics.push(PlanDiagnostic {
                severity: DiagnosticSeverity::Warning,
                kind: "llm_advisor_rejected".to_string(),
                message: format!("rename ignored unknown module `{old}`"),
                path: None,
            });
            continue;
        }
        for id in affected {
            assignments.insert(
                id,
                global_plan::assignment_for_module(
                    discovery,
                    spec,
                    id,
                    &new_name,
                    format!("LLM advisor renamed `{old}` to `{new_name}`"),
                ),
            );
        }
        diagnostics.push(PlanDiagnostic {
            severity: DiagnosticSeverity::Info,
            kind: "llm_advisor_applied".to_string(),
            message: format!("renamed assignment bucket `{old}` to `{new_name}`"),
            path: None,
        });
    }

    let mut moved = BTreeSet::new();
    for mv in reply.moves {
        if !valid_ids.contains(&mv.id) {
            diagnostics.push(PlanDiagnostic {
                severity: DiagnosticSeverity::Warning,
                kind: "llm_advisor_rejected".to_string(),
                message: format!("move ignored unknown item id {}", mv.id),
                path: None,
            });
            continue;
        }
        if !moved.insert(mv.id) {
            diagnostics.push(PlanDiagnostic {
                severity: DiagnosticSeverity::Warning,
                kind: "llm_advisor_rejected".to_string(),
                message: format!("move ignored duplicate item id {}", mv.id),
                path: None,
            });
            continue;
        }
        let to = crate::names::sanitize_module(&mv.to);
        assignments.insert(
            mv.id,
            global_plan::assignment_for_module(
                discovery,
                spec,
                mv.id,
                &to,
                format!("LLM advisor moved item to `{to}`"),
            ),
        );
        diagnostics.push(PlanDiagnostic {
            severity: DiagnosticSeverity::Info,
            kind: "llm_advisor_applied".to_string(),
            message: format!("moved item {} to `{to}`", mv.id),
            path: None,
        });
    }
    diagnostics
}

fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max).collect();
    out.push_str("...");
    out
}

fn derive_migration_edits(
    workspace: &WorkspaceModel,
    discovery: &Discovery,
    assignments: &BTreeMap<GlobalItemId, global_plan::ModuleAssignment>,
) -> Vec<PlanEdit> {
    let mut edits = Vec::new();
    let mut target_contents: BTreeMap<PathBuf, Vec<String>> = BTreeMap::new();
    let mut source_modules: BTreeSet<(FileId, String, String)> = BTreeSet::new();
    let mut reexports: BTreeSet<(FileId, String)> = BTreeSet::new();

    for (id, assignment) in assignments {
        let item = &discovery.items[*id];
        let source = &discovery.files[item.file_id].path;
        if source == &assignment.target_file {
            continue;
        }
        let span = SourceSpan {
            line_start: item.line_start,
            line_end: item.line_end,
        };
        edits.push(PlanEdit::MoveItem {
            item: *id,
            from: item.file_id,
            to: assignment.target_file.clone(),
            span,
        });
        if needs_visibility_edit(item, &assignment.visibility) {
            edits.push(PlanEdit::AdjustVisibility {
                item: *id,
                visibility: assignment.visibility.clone(),
            });
        }
        target_contents
            .entry(assignment.target_file.clone())
            .or_default()
            .push(source_for_item(discovery, item, &assignment.visibility));
        if let Some(module) = module_name_for_target(&assignment.target_file) {
            source_modules.insert((
                item.file_id,
                module.clone(),
                module_decl_for_target(&assignment.target_file),
            ));
            if !item.name.is_empty() && matches!(item.vis, ItemVis::Public | ItemVis::Crate) {
                reexports.insert((item.file_id, format!("{module}::{}", item.name)));
            }
        }
    }

    for (path, parts) in target_contents {
        edits.push(PlanEdit::CreateFile {
            path,
            contents: parts.join("\n\n") + "\n",
        });
    }
    for (file, module, declaration) in source_modules {
        if workspace.files.get(file).is_some() {
            edits.push(PlanEdit::InsertModDecl {
                file,
                module,
                declaration,
            });
        }
    }
    for (file, path) in reexports {
        edits.push(PlanEdit::AddReExport { file, path });
    }
    edits.sort_by(|a, b| format!("{a:?}").cmp(&format!("{b:?}")));
    edits
}

fn safety_for_plan(
    opts: &RefactorOptions,
    symbols: &SymbolGraph,
    diagnostics: &[PlanDiagnostic],
    edits: &[PlanEdit],
) -> PlanSafety {
    let hard_blockers: Vec<String> = diagnostics
        .iter()
        .filter(|d| d.severity == DiagnosticSeverity::Error)
        .map(|d| d.message.clone())
        .collect();
    let mut blockers = hard_blockers.clone();
    let warnings: Vec<String> = diagnostics
        .iter()
        .filter(|d| d.severity != DiagnosticSeverity::Error)
        .map(|d| d.message.clone())
        .collect();
    if opts.require_tokensave_for_rewrites {
        for edit in edits {
            if let PlanEdit::RewritePath { evidence, .. } = edit
                && !matches!(
                    evidence,
                    EvidenceConfidence::Exact | EvidenceConfidence::Semantic
                )
            {
                blockers.push(format!(
                    "rewrite with {evidence:?} evidence is preview-only; write requires exact/semantic tokensave evidence"
                ));
            }
        }
        let has_rewrite = edits
            .iter()
            .any(|edit| matches!(edit, PlanEdit::RewritePath { .. }));
        if opts.mode == RefactorMode::Write
            && has_rewrite
            && !symbols.has_write_grade_cross_file_evidence()
        {
            blockers
                .push("cross-file rewrites require exact/semantic tokensave evidence".to_string());
        }
    }
    PlanSafety {
        can_preview: hard_blockers.is_empty(),
        can_write: blockers.is_empty(),
        blockers,
        warnings,
    }
}

fn workspace_summary(workspace: &WorkspaceModel) -> WorkspaceSummary {
    WorkspaceSummary {
        root: workspace.root.clone(),
        packages: workspace.packages.len(),
        targets: workspace.targets.len(),
        files: workspace.files.len(),
        modules: workspace.modules.len(),
    }
}

fn apply_plan_to_root(
    plan: &MigrationPlan,
    original_root: &Path,
    target_root: &Path,
) -> Result<Vec<PathBuf>> {
    let mut changed = BTreeSet::new();
    let mut removals: BTreeMap<FileId, Vec<SourceSpan>> = BTreeMap::new();

    for edit in &plan.edits {
        match edit {
            PlanEdit::MoveItem { from, span, .. } => {
                removals.entry(*from).or_default().push(*span);
            }
            PlanEdit::CreateFile { path, contents } => {
                let out = remap_path(path, original_root, target_root);
                if let Some(parent) = out.parent() {
                    fs::create_dir_all(parent)
                        .with_context(|| format!("mkdir {}", parent.display()))?;
                }
                fs::write(&out, contents).with_context(|| format!("write {}", out.display()))?;
                changed.insert(out);
            }
            _ => {}
        }
    }

    for (file_id, spans) in removals {
        if let Some(source_path) = file_path_from_plan(plan, file_id) {
            let out = remap_path(&source_path, original_root, target_root);
            let src =
                fs::read_to_string(&out).with_context(|| format!("read {}", out.display()))?;
            let rewritten = remove_line_spans(&src, &spans);
            fs::write(&out, rewritten).with_context(|| format!("write {}", out.display()))?;
            changed.insert(out);
        }
    }

    for edit in &plan.edits {
        match edit {
            PlanEdit::InsertModDecl {
                file, declaration, ..
            } => {
                if let Some(path) = file_path_from_plan(plan, *file) {
                    let out = remap_path(&path, original_root, target_root);
                    insert_unique_preamble_line(&out, declaration)?;
                    changed.insert(out);
                }
            }
            PlanEdit::AddReExport { file, path } => {
                if let Some(source_path) = file_path_from_plan(plan, *file) {
                    let out = remap_path(&source_path, original_root, target_root);
                    insert_unique_preamble_line(&out, &format!("pub use {path};"))?;
                    changed.insert(out);
                }
            }
            PlanEdit::AddUse { file, path } => {
                if let Some(source_path) = file_path_from_plan(plan, *file) {
                    let out = remap_path(&source_path, original_root, target_root);
                    insert_unique_preamble_line(&out, &format!("use {path};"))?;
                    changed.insert(out);
                }
            }
            PlanEdit::RewritePath {
                file,
                span,
                old,
                new,
                ..
            } => {
                if let Some(source_path) = file_path_from_plan(plan, *file) {
                    let out = remap_path(&source_path, original_root, target_root);
                    rewrite_line_span(&out, *span, old, new)?;
                    changed.insert(out);
                }
            }
            _ => {}
        }
    }

    Ok(changed.into_iter().collect())
}

fn file_path_from_plan(plan: &MigrationPlan, file_id: FileId) -> Option<PathBuf> {
    plan.assignments
        .iter()
        .find(|a| a.from_file == file_id)
        .map(|a| a.from_path.clone())
}

fn planned_changed_paths(plan: &MigrationPlan) -> Vec<PathBuf> {
    let mut out = BTreeSet::new();
    for assignment in &plan.assignments {
        if assignment.from_path != assignment.to_file {
            out.insert(assignment.from_path.clone());
        }
    }
    for edit in &plan.edits {
        match edit {
            PlanEdit::CreateFile { path, .. } | PlanEdit::MoveItem { to: path, .. } => {
                out.insert(path.clone());
            }
            _ => {}
        }
    }
    out.into_iter().collect()
}

fn run_preview_checks(
    root: &Path,
    changed_files: &[PathBuf],
    format_temp: bool,
    run_full_tests: bool,
) -> Vec<CheckResult> {
    let mut checks = Vec::new();
    if root.join("Cargo.toml").is_file() && format_temp {
        checks.push(run_command(root, "cargo", &["fmt", "--all"]));
    }

    for path in changed_files {
        if path.extension().and_then(|s| s.to_str()) == Some("rs") {
            let success = fs::read_to_string(path)
                .ok()
                .and_then(|src| syn::parse_file(&src).ok())
                .is_some();
            checks.push(CheckResult {
                command: format!("syn::parse_file {}", path.display()),
                success,
                stdout: String::new(),
                stderr: if success {
                    String::new()
                } else {
                    "Rust file failed to parse".to_string()
                },
            });
        }
    }

    if root.join("Cargo.toml").is_file() {
        let packages = affected_package_names(root, changed_files).unwrap_or_default();
        if packages.is_empty() {
            checks.push(run_command(root, "cargo", &["check"]));
            if run_full_tests {
                checks.push(run_command(root, "cargo", &["test"]));
            }
        } else {
            for package in packages {
                checks.push(run_command(root, "cargo", &["check", "-p", &package]));
                if run_full_tests {
                    checks.push(run_command(root, "cargo", &["test", "-p", &package]));
                }
            }
        }
    }
    checks
}

#[derive(Debug, Deserialize)]
struct CheckMetadata {
    packages: Vec<CheckPackage>,
}

#[derive(Debug, Deserialize)]
struct CheckPackage {
    name: String,
    manifest_path: PathBuf,
}

fn affected_package_names(root: &Path, changed_files: &[PathBuf]) -> Result<BTreeSet<String>> {
    let output = std::process::Command::new("cargo")
        .arg("metadata")
        .arg("--format-version")
        .arg("1")
        .arg("--no-deps")
        .current_dir(root)
        .output()
        .context("run cargo metadata for affected package checks")?;
    if !output.status.success() {
        bail!(
            "cargo metadata failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let metadata: CheckMetadata =
        serde_json::from_slice(&output.stdout).context("parse cargo metadata")?;
    let mut packages = BTreeSet::new();
    let canonical_changed: Vec<PathBuf> = changed_files
        .iter()
        .map(|path| path.canonicalize().unwrap_or_else(|_| path.clone()))
        .collect();
    for package in metadata.packages {
        let manifest_dir = package
            .manifest_path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| root.to_path_buf());
        let manifest_dir = manifest_dir
            .canonicalize()
            .unwrap_or_else(|_| manifest_dir.clone());
        if canonical_changed
            .iter()
            .any(|path| path.starts_with(&manifest_dir))
        {
            packages.insert(package.name);
        }
    }
    Ok(packages)
}

fn run_command(root: &Path, program: &str, args: &[&str]) -> CheckResult {
    let output = std::process::Command::new(program)
        .args(args)
        .current_dir(root)
        .output();
    match output {
        Ok(output) => CheckResult {
            command: format!("{} {}", program, args.join(" ")),
            success: output.status.success(),
            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        },
        Err(e) => CheckResult {
            command: format!("{} {}", program, args.join(" ")),
            success: false,
            stdout: String::new(),
            stderr: e.to_string(),
        },
    }
}

fn render_diff(original_root: &Path, temp_root: &Path, changed_files: &[PathBuf]) -> String {
    let mut out = String::new();
    for temp_path in changed_files {
        let original = original_for_temp(temp_path, original_root, temp_root);
        let old = fs::read_to_string(&original).unwrap_or_default();
        let new = fs::read_to_string(temp_path).unwrap_or_default();
        if old != new {
            out.push_str(&format!(
                "--- {}\n+++ {}\n",
                original.display(),
                temp_path.display()
            ));
            out.push_str(&new);
            if !new.ends_with('\n') {
                out.push('\n');
            }
        }
    }
    out
}

fn original_for_temp(temp_path: &Path, original_root: &Path, temp_root: &Path) -> PathBuf {
    temp_path
        .strip_prefix(temp_root)
        .map(|rel| original_root.join(rel))
        .unwrap_or_else(|_| temp_path.to_path_buf())
}

fn remap_path(path: &Path, original_root: &Path, target_root: &Path) -> PathBuf {
    path.strip_prefix(original_root)
        .map(|rel| target_root.join(rel))
        .unwrap_or_else(|_| path.to_path_buf())
}

fn remove_line_spans(src: &str, spans: &[SourceSpan]) -> String {
    let mut remove = BTreeSet::new();
    for span in spans {
        for line in span.line_start..=span.line_end {
            remove.insert(line);
        }
    }
    src.lines()
        .enumerate()
        .filter(|(idx, _)| !remove.contains(&(idx + 1)))
        .map(|(_, line)| line)
        .collect::<Vec<_>>()
        .join("\n")
        + "\n"
}

fn insert_unique_preamble_line(path: &Path, line: &str) -> Result<()> {
    let src = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    if src.lines().any(|existing| existing.trim() == line) {
        return Ok(());
    }
    let insert_after = inner_attr_end_line(&src);
    let mut lines: Vec<&str> = src.lines().collect();
    let idx = insert_after.min(lines.len());
    lines.insert(idx, line);
    let mut out = lines.join("\n");
    if src.ends_with('\n') {
        out.push('\n');
    }
    fs::write(path, out).with_context(|| format!("write {}", path.display()))
}

fn rewrite_line_span(path: &Path, span: SourceSpan, old: &str, new: &str) -> Result<()> {
    let src = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let mut lines: Vec<String> = src.lines().map(str::to_string).collect();
    for line_no in span.line_start..=span.line_end {
        let Some(line) = lines.get_mut(line_no.saturating_sub(1)) else {
            continue;
        };
        *line = line.replace(old, new);
    }
    let mut out = lines.join("\n");
    if src.ends_with('\n') {
        out.push('\n');
    }
    fs::write(path, out).with_context(|| format!("write {}", path.display()))
}

fn inner_attr_end_line(src: &str) -> usize {
    let Ok(file) = syn::parse_file(src) else {
        return 0;
    };
    file.attrs
        .iter()
        .filter(|attr| matches!(attr.style, syn::AttrStyle::Inner(_)))
        .map(|attr| attr.span().end().line)
        .max()
        .unwrap_or(0)
}

fn source_for_item(discovery: &Discovery, item: &GlobalItem, visibility: &str) -> String {
    let source = discovery.files[item.file_id]
        .items
        .iter()
        .find(|parsed| parsed.id == item.local_id)
        .map(|parsed| parsed.source.clone())
        .unwrap_or_default();
    if !needs_visibility_edit(item, visibility) {
        return source;
    }
    apply_visibility_to_source(&source, visibility)
}

fn needs_visibility_edit(item: &GlobalItem, visibility: &str) -> bool {
    visibility != "private" && matches!(item.vis, ItemVis::Private)
}

fn apply_visibility_to_source(source: &str, visibility: &str) -> String {
    if visibility == "private" {
        return source.to_string();
    }
    let mut out = Vec::new();
    let mut applied = false;
    for line in source.lines() {
        if !applied {
            let trimmed = line.trim_start();
            let indent_len = line.len() - trimmed.len();
            if starts_item_keyword(trimmed) && !trimmed.starts_with("pub ") {
                out.push(format!("{}{} {}", &line[..indent_len], visibility, trimmed));
                applied = true;
                continue;
            }
        }
        out.push(line.to_string());
    }
    out.join("\n")
}

fn starts_item_keyword(trimmed: &str) -> bool {
    matches!(
        trimmed.split_whitespace().next(),
        Some("fn")
            | Some("struct")
            | Some("enum")
            | Some("trait")
            | Some("type")
            | Some("const")
            | Some("static")
            | Some("mod")
    )
}

fn module_name_for_target(path: &Path) -> Option<String> {
    path.file_stem()
        .and_then(|s| s.to_str())
        .map(crate::names::escape_keyword)
}

fn module_decl_for_target(path: &Path) -> String {
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("module");
    let module = crate::names::escape_keyword(stem);
    if stem == module {
        format!("mod {module};")
    } else {
        let file_name = path.file_name().and_then(|s| s.to_str()).unwrap_or(stem);
        format!("#[path = \"{file_name}\"]\nmod {module};")
    }
}

fn backup_path(path: &Path) -> PathBuf {
    let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("file");
    path.with_file_name(format!("{name}.bak"))
}

fn copy_tree(from: &Path, to: &Path) -> Result<()> {
    if !to.exists() {
        fs::create_dir_all(to).with_context(|| format!("mkdir {}", to.display()))?;
    }
    for entry in fs::read_dir(from).with_context(|| format!("read_dir {}", from.display()))? {
        let entry = entry?;
        let path = entry.path();
        let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
        if matches!(name, ".git" | "target" | ".tokensave") {
            continue;
        }
        let dest = to.join(entry.file_name());
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            copy_tree(&path, &dest)?;
        } else if file_type.is_file() {
            fs::copy(&path, &dest)
                .with_context(|| format!("copy {} -> {}", path.display(), dest.display()))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec::{ModuleSpec, Pattern};

    fn advisor_fixture() -> (
        tempfile::TempDir,
        Spec,
        Discovery,
        BTreeMap<GlobalItemId, global_plan::ModuleAssignment>,
    ) {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(
            tmp.path().join("Cargo.toml"),
            r#"[package]
name = "demo"
version = "0.1.0"
edition = "2021"
"#,
        )
        .unwrap();
        fs::create_dir_all(tmp.path().join("src")).unwrap();
        fs::write(
            tmp.path().join("src/lib.rs"),
            "pub fn alpha() {}\npub fn beta() {}\n",
        )
        .unwrap();
        let spec = Spec {
            modules: vec![ModuleSpec {
                name: "app".to_string(),
                patterns: vec![Pattern::Glob("*".to_string())],
                visibility: "pub".to_string(),
                output: None,
            }],
            ..Default::default()
        };
        let discovery = crate::discover::discover_dir(tmp.path(), false).unwrap();
        let global = global_plan::build_plan(&discovery, &spec);
        (tmp, spec, discovery, global.assignments)
    }

    #[test]
    fn advisor_rejects_unknown_and_duplicate_ids() {
        let (_tmp, spec, discovery, mut assignments) = advisor_fixture();

        let diagnostics = apply_advisor_reply(
            &spec,
            &discovery,
            &mut assignments,
            AdvisorReply {
                renames: BTreeMap::new(),
                moves: vec![
                    AdvisorMove {
                        id: 99,
                        to: "elsewhere".to_string(),
                    },
                    AdvisorMove {
                        id: 0,
                        to: "core".to_string(),
                    },
                    AdvisorMove {
                        id: 0,
                        to: "second".to_string(),
                    },
                ],
            },
        );

        assert!(
            diagnostics
                .iter()
                .any(|d| d.message.contains("unknown item id 99"))
        );
        assert!(
            diagnostics
                .iter()
                .any(|d| d.message.contains("duplicate item id 0"))
        );
        assert_eq!(assignments.get(&0).unwrap().module_name, "core");
    }

    #[test]
    fn advisor_sanitizes_module_names() {
        let (_tmp, spec, discovery, mut assignments) = advisor_fixture();

        apply_advisor_reply(
            &spec,
            &discovery,
            &mut assignments,
            AdvisorReply {
                renames: [("app".to_string(), "Core Helpers.rs".to_string())]
                    .into_iter()
                    .collect(),
                moves: Vec::new(),
            },
        );

        assert!(
            assignments
                .values()
                .all(|assignment| assignment.module_name == "core_helpers")
        );
    }
}
