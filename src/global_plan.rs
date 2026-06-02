//! Global planner: take a `Discovery` (global index + graph) and a `Spec`, and
//! produce a `GlobalPlan` that assigns every item to a target module, detects
//! architecture violations, and derives file-level operations.

use crate::discover::{CargoPackageInfo, Discovery, GlobalItemId};
use crate::spec::{Spec, match_item};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// Data model

/// A plan for restructuring a codebase according to an architecture spec.
#[derive(Debug, Clone, Serialize)]
pub struct GlobalPlan {
    /// Which module each item is assigned to.
    pub assignments: BTreeMap<GlobalItemId, ModuleAssignment>,
    /// Detected violations of boundary rules or other constraints.
    pub violations: Vec<Violation>,
    /// Derived file-level operations (dry-run only; not executed here).
    pub operations: Vec<Operation>,
}

/// Where a single item should live after refactoring.
#[derive(Debug, Clone, Serialize)]
pub struct ModuleAssignment {
    pub module_name: String,
    pub target_file: PathBuf,
    pub visibility: String,
    pub rationale: String,
}

/// An architecture violation.
#[derive(Debug, Clone, Serialize)]
pub struct Violation {
    pub kind: ViolationKind,
    pub from_item: GlobalItemId,
    pub to_item: GlobalItemId,
    pub from_module: String,
    pub to_module: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize)]
pub enum ViolationKind {
    BoundaryViolation,
    NameCollision,
}

/// A coarse-grained operation derived from the assignments.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "op")]
pub enum Operation {
    CreateFile {
        path: PathBuf,
        reason: String,
    },
    MoveItems {
        items: Vec<MoveItem>,
        from: PathBuf,
        to: PathBuf,
    },
    RewritePaths {
        file: PathBuf,
        note: String,
    },
}

#[derive(Debug, Clone, Serialize)]
pub struct MoveItem {
    pub item_id: GlobalItemId,
    pub name: String,
}

// ---------------------------------------------------------------------------
// Builder

/// Build a global plan from a discovery run and an architecture spec.
pub fn build_plan(discovery: &Discovery, spec: &Spec) -> GlobalPlan {
    let mut assignments: BTreeMap<GlobalItemId, ModuleAssignment> = BTreeMap::new();

    // 1. Pattern-based assignment (first match wins).
    for item in &discovery.items {
        if let Some(module) = match_item(
            spec,
            &item.name,
            &item_kind_label(&item.kind),
            &discovery.files[item.file_id].path,
        ) {
            assignments.insert(
                item.id,
                make_assignment(discovery, spec, item.id, &module.name, "pattern match"),
            );
        }
    }

    // 2. Call-graph clustering for unmatched items.
    let unmatched: Vec<GlobalItemId> = discovery
        .items
        .iter()
        .map(|i| i.id)
        .filter(|id| !assignments.contains_key(id))
        .collect();

    for id in unmatched {
        let mut module_counts: BTreeMap<String, usize> = BTreeMap::new();
        for edge in &discovery.edges {
            if edge.from != id {
                continue;
            }
            if let Some(assignment) = assignments.get(&edge.to) {
                *module_counts
                    .entry(assignment.module_name.clone())
                    .or_default() += 1;
            }
        }

        if let Some((best_module, count)) = module_counts.into_iter().max_by_key(|(_, c)| *c) {
            if count > 0 {
                assignments.insert(
                    id,
                    make_assignment(
                        discovery,
                        spec,
                        id,
                        &best_module,
                        format!("call-graph: {count} ref(s) into `{best_module}`"),
                    ),
                );
            }
        }
    }

    // 3. Fallback to defaults.
    for item in &discovery.items {
        if !assignments.contains_key(&item.id) {
            assignments.insert(
                item.id,
                make_assignment(
                    discovery,
                    spec,
                    item.id,
                    &spec.defaults.unmatched_module,
                    "default fallback",
                ),
            );
        }
    }

    // 4. Detect violations.
    let violations = detect_violations(discovery, spec, &assignments);

    // 5. Derive operations.
    let operations = derive_operations(discovery, &assignments);

    GlobalPlan {
        assignments,
        violations,
        operations,
    }
}

// ---------------------------------------------------------------------------
// Target file computation

fn make_assignment(
    discovery: &Discovery,
    spec: &Spec,
    item_id: GlobalItemId,
    module_name: &str,
    rationale: impl Into<String>,
) -> ModuleAssignment {
    let item = &discovery.items[item_id];
    let source_file = &discovery.files[item.file_id].path;
    ModuleAssignment {
        module_name: module_name.to_string(),
        target_file: compute_target_file(discovery, spec, module_name, source_file),
        visibility: module_visibility(spec, module_name),
        rationale: rationale.into(),
    }
}

fn module_visibility(spec: &Spec, module_name: &str) -> String {
    spec.modules
        .iter()
        .find(|m| m.name == module_name)
        .map(|m| m.visibility.clone())
        .unwrap_or_else(|| spec.defaults.unmatched_visibility.clone())
}

fn compute_target_file(
    discovery: &Discovery,
    spec: &Spec,
    module_name: &str,
    source_file: &Path,
) -> PathBuf {
    let crate_root = crate_root_for_source(discovery, source_file);

    // If the spec module has an explicit output template, use it.
    if let Some(module) = spec.modules.iter().find(|m| m.name == module_name) {
        if let Some(template) = &module.output {
            let path = PathBuf::from(template.replace("{module}", module_name));
            return if path.is_absolute() {
                path
            } else {
                crate_root.join(path)
            };
        }
    }

    crate_root.join("src").join(format!("{module_name}.rs"))
}

fn crate_root_for_source(discovery: &Discovery, source_file: &Path) -> PathBuf {
    if let Some(cargo) = &discovery.cargo {
        if let Some(pkg) = owning_package(&cargo.packages, source_file) {
            return pkg.manifest_dir.clone();
        }
    }

    if discovery.root.file_name().is_some_and(|name| name == "src") {
        discovery
            .root
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| discovery.root.clone())
    } else {
        discovery.root.clone()
    }
}

fn owning_package<'a>(
    packages: &'a [CargoPackageInfo],
    source_file: &Path,
) -> Option<&'a CargoPackageInfo> {
    packages
        .iter()
        .filter(|pkg| source_file.starts_with(&pkg.manifest_dir))
        .max_by_key(|pkg| pkg.manifest_dir.components().count())
}

// ---------------------------------------------------------------------------
// Violation detection

fn detect_violations(
    discovery: &Discovery,
    spec: &Spec,
    assignments: &BTreeMap<GlobalItemId, ModuleAssignment>,
) -> Vec<Violation> {
    let mut violations = Vec::new();

    // Build a quick lookup: module -> set of modules it may depend on.
    let allowed_deps: BTreeMap<&str, BTreeSet<&str>> = spec
        .boundaries
        .iter()
        .map(|b| {
            (
                b.from.as_str(),
                b.may_depend_on.iter().map(|s| s.as_str()).collect(),
            )
        })
        .collect();

    for edge in &discovery.edges {
        let Some(from_assign) = assignments.get(&edge.from) else {
            continue;
        };
        let Some(to_assign) = assignments.get(&edge.to) else {
            continue;
        };

        if from_assign.module_name == to_assign.module_name {
            continue;
        }

        // Check if there is a boundary rule that forbids this dependency.
        if let Some(allowed) = allowed_deps.get(from_assign.module_name.as_str()) {
            if !allowed.contains(to_assign.module_name.as_str()) {
                let from_item = &discovery.items[edge.from];
                let to_item = &discovery.items[edge.to];
                violations.push(Violation {
                    kind: ViolationKind::BoundaryViolation,
                    from_item: edge.from,
                    to_item: edge.to,
                    from_module: from_assign.module_name.clone(),
                    to_module: to_assign.module_name.clone(),
                    message: format!(
                        "`{}` (in `{}`) references `{}` (in `{}`) — boundary rule violated",
                        from_item.name,
                        from_assign.module_name,
                        to_item.name,
                        to_assign.module_name
                    ),
                });
            }
        } else if !allowed_deps.is_empty() {
            let from_item = &discovery.items[edge.from];
            let to_item = &discovery.items[edge.to];
            violations.push(Violation {
                kind: ViolationKind::BoundaryViolation,
                from_item: edge.from,
                to_item: edge.to,
                from_module: from_assign.module_name.clone(),
                to_module: to_assign.module_name.clone(),
                message: format!(
                    "`{}` (in `{}`) references `{}` (in `{}`) but `{}` has no boundary rule",
                    from_item.name,
                    from_assign.module_name,
                    to_item.name,
                    to_assign.module_name,
                    from_assign.module_name
                ),
            });
        }
    }

    // Detect name collisions: two items from different source files assigned
    // to the same target file with the same name.
    let mut by_target: BTreeMap<(PathBuf, &str), Vec<GlobalItemId>> = BTreeMap::new();
    for (id, assign) in assignments {
        by_target
            .entry((assign.target_file.clone(), &assign.module_name))
            .or_default()
            .push(*id);
    }

    for ((_path, _module), ids) in by_target {
        let mut name_counts: BTreeMap<&str, Vec<GlobalItemId>> = BTreeMap::new();
        for id in &ids {
            let item = &discovery.items[*id];
            if !item.name.is_empty() {
                name_counts.entry(&item.name).or_default().push(*id);
            }
        }
        for (_name, colliders) in name_counts {
            if colliders.len() > 1 {
                // Only flag if they come from different source files.
                let files: BTreeSet<usize> = colliders
                    .iter()
                    .map(|id| discovery.items[*id].file_id)
                    .collect();
                if files.len() > 1 {
                    for id in &colliders {
                        violations.push(Violation {
                            kind: ViolationKind::NameCollision,
                            from_item: *id,
                            to_item: colliders[0],
                            from_module: assignments[id].module_name.clone(),
                            to_module: assignments[&colliders[0]].module_name.clone(),
                            message: format!(
                                "name collision: `{}` appears in multiple source files and would end up in the same target module",
                                discovery.items[*id].name
                            ),
                        });
                    }
                }
            }
        }
    }

    violations
}

// ---------------------------------------------------------------------------
// Operation derivation

fn derive_operations(
    discovery: &Discovery,
    assignments: &BTreeMap<GlobalItemId, ModuleAssignment>,
) -> Vec<Operation> {
    let mut operations: Vec<Operation> = Vec::new();

    // Group items by (source_file, target_file).
    let mut moves: BTreeMap<(PathBuf, PathBuf), Vec<MoveItem>> = BTreeMap::new();
    for (id, assign) in assignments {
        let item = &discovery.items[*id];
        let source_path = discovery.files[item.file_id].path.clone();
        if source_path != assign.target_file {
            moves
                .entry((source_path, assign.target_file.clone()))
                .or_default()
                .push(MoveItem {
                    item_id: *id,
                    name: item.name.clone(),
                });
        }
    }

    for ((from, to), items) in moves {
        operations.push(Operation::MoveItems { items, from, to });
    }

    // Determine which target files need to be created.
    let target_files: BTreeSet<PathBuf> = assignments
        .values()
        .map(|a| a.target_file.clone())
        .collect();

    for path in target_files {
        if !path.exists() {
            operations.push(Operation::CreateFile {
                path,
                reason: "target module does not yet exist".to_string(),
            });
        }
    }

    // Determine which source files will need path rewrites.
    let modified_sources: BTreeSet<PathBuf> = assignments
        .iter()
        .filter(|(id, assign)| {
            let item = &discovery.items[**id];
            discovery.files[item.file_id].path != assign.target_file
        })
        .map(|(id, _)| discovery.files[discovery.items[*id].file_id].path.clone())
        .collect();

    for file in modified_sources {
        operations.push(Operation::RewritePaths {
            file,
            note: "items moved out of or into this file — paths and imports need rewriting"
                .to_string(),
        });
    }

    // Sort for deterministic output.
    operations.sort_by(|a, b| format!("{a:?}").cmp(&format!("{b:?}")));
    operations
}

// ---------------------------------------------------------------------------
// Helpers

fn item_kind_label(kind: &crate::item::ItemKind) -> String {
    use crate::item::ItemKind;
    match kind {
        ItemKind::Fn { .. } => "fn".to_string(),
        ItemKind::Struct => "struct".to_string(),
        ItemKind::Enum => "enum".to_string(),
        ItemKind::Union => "union".to_string(),
        ItemKind::Trait => "trait".to_string(),
        ItemKind::TraitAlias => "trait_alias".to_string(),
        ItemKind::Impl { .. } => "impl".to_string(),
        ItemKind::Macro => "macro".to_string(),
        ItemKind::Const => "const".to_string(),
        ItemKind::Static => "static".to_string(),
        ItemKind::TypeAlias => "type".to_string(),
        ItemKind::Use => "use".to_string(),
        ItemKind::ExternCrate => "extern_crate".to_string(),
        ItemKind::Mod => "mod".to_string(),
        ItemKind::ForeignMod => "foreign_mod".to_string(),
        ItemKind::Verbatim => "verbatim".to_string(),
    }
}

// ---------------------------------------------------------------------------
// Human-readable report

pub fn human_report(plan: &GlobalPlan, discovery: &Discovery) -> String {
    let mut out = String::new();

    out.push_str("Global Plan\n===========\n\n");

    // Group assignments by target file.
    let mut by_target: BTreeMap<&Path, Vec<GlobalItemId>> = BTreeMap::new();
    for (id, assign) in &plan.assignments {
        by_target.entry(&assign.target_file).or_default().push(*id);
    }

    for (path, ids) in by_target {
        out.push_str(&format!("{} ({} item(s))\n", path.display(), ids.len()));
        for id in ids {
            let item = &discovery.items[id];
            let assign = &plan.assignments[&id];
            out.push_str(&format!(
                "  {:4} {}  L{}-{}  {}  — {}\n",
                item.id,
                item.name,
                item.line_start,
                item.line_end,
                assign.module_name,
                assign.rationale
            ));
        }
        out.push('\n');
    }

    if !plan.violations.is_empty() {
        out.push_str(&format!("Violations ({}):\n", plan.violations.len()));
        for v in &plan.violations {
            let icon = match v.kind {
                ViolationKind::BoundaryViolation => "🚫",
                ViolationKind::NameCollision => "⚠️",
            };
            out.push_str(&format!("  {icon} {}\n", v.message));
        }
        out.push('\n');
    }

    if !plan.operations.is_empty() {
        out.push_str(&format!("Operations ({}):\n", plan.operations.len()));
        for op in &plan.operations {
            match op {
                Operation::CreateFile { path, reason } => {
                    out.push_str(&format!("  [create] {} ({})\n", path.display(), reason));
                }
                Operation::MoveItems { items, from, to } => {
                    out.push_str(&format!(
                        "  [move] {} item(s) from {} -> {}\n",
                        items.len(),
                        from.display(),
                        to.display()
                    ));
                }
                Operation::RewritePaths { file, note } => {
                    out.push_str(&format!("  [rewrite] {} — {note}\n", file.display()));
                }
            }
        }
    }

    out
}

// ---------------------------------------------------------------------------
// Tests

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discover::{CargoInfo, CargoPackageInfo, DiscoveredFile, GlobalItem, RefEdge};
    use crate::item::{ItemKind, ItemVis};
    use crate::spec::{BoundarySpec, DefaultsSpec, ModuleSpec, Pattern, Spec};

    fn fake_discovery(items: Vec<GlobalItem>, edges: Vec<RefEdge>) -> Discovery {
        let files: Vec<DiscoveredFile> = vec![DiscoveredFile {
            path: PathBuf::from("src/lib.rs"),
            source: String::new(),
            items: Vec::new(),
        }];
        Discovery {
            root: PathBuf::from("."),
            files,
            items,
            edges,
            cargo: None,
        }
    }

    fn fake_item(id: GlobalItemId, name: &str, kind: ItemKind) -> GlobalItem {
        GlobalItem {
            id,
            file_id: 0,
            local_id: id,
            name: name.to_string(),
            kind,
            vis: ItemVis::Public,
            line_start: id + 1,
            line_end: id + 1,
        }
    }

    fn simple_spec() -> Spec {
        Spec {
            modules: vec![
                ModuleSpec {
                    name: "domain".to_string(),
                    patterns: vec![Pattern::Glob("*Model".to_string())],
                    visibility: "pub(crate)".to_string(),
                    output: None,
                },
                ModuleSpec {
                    name: "app".to_string(),
                    patterns: vec![Pattern::Glob("*Service".to_string())],
                    visibility: "pub".to_string(),
                    output: None,
                },
            ],
            boundaries: vec![
                BoundarySpec {
                    from: "domain".to_string(),
                    may_depend_on: vec![],
                },
                BoundarySpec {
                    from: "app".to_string(),
                    may_depend_on: vec!["domain".to_string()],
                },
            ],
            defaults: DefaultsSpec::default(),
        }
    }

    #[test]
    fn pattern_assignment_works() {
        let items = vec![
            fake_item(0, "UserModel", ItemKind::Struct),
            fake_item(1, "UserService", ItemKind::Fn { is_test: false }),
        ];
        let disc = fake_discovery(items, vec![]);
        let spec = simple_spec();
        let plan = build_plan(&disc, &spec);

        assert_eq!(plan.assignments[&0].module_name, "domain");
        assert_eq!(plan.assignments[&0].visibility, "pub(crate)");
        assert_eq!(plan.assignments[&1].module_name, "app");
        assert_eq!(plan.assignments[&1].visibility, "pub");
    }

    #[test]
    fn call_graph_clustering_assigns_unmatched() {
        let items = vec![
            fake_item(0, "UserModel", ItemKind::Struct),
            fake_item(1, "Helper", ItemKind::Fn { is_test: false }),
        ];
        // Helper references UserModel.
        let edges = vec![RefEdge { from: 1, to: 0 }];
        let disc = fake_discovery(items, edges);
        let spec = simple_spec();
        let plan = build_plan(&disc, &spec);

        assert_eq!(plan.assignments[&0].module_name, "domain");
        assert_eq!(plan.assignments[&1].module_name, "domain");
        assert!(plan.assignments[&1].rationale.contains("call-graph"));
    }

    #[test]
    fn default_fallback_for_orphans() {
        let items = vec![fake_item(0, "Orphan", ItemKind::Fn { is_test: false })];
        let disc = fake_discovery(items, vec![]);
        let spec = simple_spec();
        let plan = build_plan(&disc, &spec);

        assert_eq!(plan.assignments[&0].module_name, "misc");
        assert_eq!(plan.assignments[&0].visibility, "private");
    }

    #[test]
    fn boundary_violation_detected() {
        let items = vec![
            fake_item(0, "UserModel", ItemKind::Struct),
            fake_item(1, "UserService", ItemKind::Fn { is_test: false }),
        ];
        // domain -> app is NOT allowed by the boundary rule (only app -> domain is).
        let edges = vec![RefEdge { from: 0, to: 1 }];
        let disc = fake_discovery(items, edges);
        let spec = simple_spec();
        let plan = build_plan(&disc, &spec);

        assert!(!plan.violations.is_empty());
        assert!(
            plan.violations
                .iter()
                .any(|v| matches!(v.kind, ViolationKind::BoundaryViolation))
        );
    }

    #[test]
    fn allowed_dependency_no_violation() {
        let items = vec![
            fake_item(0, "UserModel", ItemKind::Struct),
            fake_item(1, "UserService", ItemKind::Fn { is_test: false }),
        ];
        // app -> domain IS allowed.
        let edges = vec![RefEdge { from: 1, to: 0 }];
        let disc = fake_discovery(items, edges);
        let spec = simple_spec();
        let plan = build_plan(&disc, &spec);

        assert!(
            !plan
                .violations
                .iter()
                .any(|v| matches!(v.kind, ViolationKind::BoundaryViolation))
        );
    }

    #[test]
    fn missing_boundary_rule_is_a_violation_when_policy_exists() {
        let items = vec![
            fake_item(0, "UserModel", ItemKind::Struct),
            fake_item(1, "UserService", ItemKind::Fn { is_test: false }),
        ];
        let edges = vec![RefEdge { from: 0, to: 1 }];
        let disc = fake_discovery(items, edges);
        let mut spec = simple_spec();
        spec.boundaries = vec![BoundarySpec {
            from: "app".to_string(),
            may_depend_on: vec!["domain".to_string()],
        }];

        let plan = build_plan(&disc, &spec);

        assert!(plan.violations.iter().any(|v| {
            matches!(v.kind, ViolationKind::BoundaryViolation)
                && v.message.contains("has no boundary rule")
        }));
    }

    #[test]
    fn workspace_targets_stay_in_owning_package() {
        let root = PathBuf::from("/workspace");
        let mut foo_item = fake_item(0, "UserModel", ItemKind::Struct);
        foo_item.file_id = 0;
        let mut bar_item = fake_item(1, "OrderModel", ItemKind::Struct);
        bar_item.file_id = 1;

        let disc = Discovery {
            root: root.clone(),
            files: vec![
                DiscoveredFile {
                    path: root.join("crates/foo/src/lib.rs"),
                    source: String::new(),
                    items: Vec::new(),
                },
                DiscoveredFile {
                    path: root.join("crates/bar/src/lib.rs"),
                    source: String::new(),
                    items: Vec::new(),
                },
            ],
            items: vec![foo_item, bar_item],
            edges: vec![],
            cargo: Some(CargoInfo {
                workspace_root: root.clone(),
                packages: vec![
                    CargoPackageInfo {
                        name: "foo".to_string(),
                        manifest_dir: root.join("crates/foo"),
                        targets: Vec::new(),
                    },
                    CargoPackageInfo {
                        name: "bar".to_string(),
                        manifest_dir: root.join("crates/bar"),
                        targets: Vec::new(),
                    },
                ],
            }),
        };

        let spec = simple_spec();
        let plan = build_plan(&disc, &spec);

        assert_eq!(
            plan.assignments[&0].target_file,
            root.join("crates/foo/src/domain.rs")
        );
        assert_eq!(
            plan.assignments[&1].target_file,
            root.join("crates/bar/src/domain.rs")
        );
    }

    #[test]
    fn explicit_output_is_relative_to_owning_crate() {
        let root = PathBuf::from("/workspace");
        let mut item = fake_item(0, "UserService", ItemKind::Fn { is_test: false });
        item.file_id = 0;
        let disc = Discovery {
            root: root.clone(),
            files: vec![DiscoveredFile {
                path: root.join("crates/foo/src/lib.rs"),
                source: String::new(),
                items: Vec::new(),
            }],
            items: vec![item],
            edges: vec![],
            cargo: Some(CargoInfo {
                workspace_root: root.clone(),
                packages: vec![CargoPackageInfo {
                    name: "foo".to_string(),
                    manifest_dir: root.join("crates/foo"),
                    targets: Vec::new(),
                }],
            }),
        };
        let mut spec = simple_spec();
        spec.modules[1].output = Some("src/app/{module}.rs".to_string());

        let plan = build_plan(&disc, &spec);

        assert_eq!(
            plan.assignments[&0].target_file,
            root.join("crates/foo/src/app/app.rs")
        );
    }

    #[test]
    fn operations_include_moves_and_creates() {
        let items = vec![
            fake_item(0, "UserModel", ItemKind::Struct),
            fake_item(1, "UserService", ItemKind::Fn { is_test: false }),
        ];
        let disc = fake_discovery(items, vec![]);
        let spec = simple_spec();
        let plan = build_plan(&disc, &spec);

        assert!(
            plan.operations
                .iter()
                .any(|op| matches!(op, Operation::MoveItems { .. }))
        );
        assert!(
            plan.operations
                .iter()
                .any(|op| matches!(op, Operation::CreateFile { .. }))
        );
    }
}
