//! Workspace-level Rust model used by the whole-codebase refactor engine.
//!
//! This module is deliberately more explicit than `discover`: it records
//! package, target, file, and module ownership so later phases can avoid
//! workspace-root guesses.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use syn::Item;

pub type PackageId = usize;
pub type TargetId = usize;
pub type FileId = usize;
pub type ModuleId = usize;

#[derive(Debug, Clone, Serialize)]
pub struct WorkspaceModel {
    pub root: PathBuf,
    pub packages: Vec<PackageModel>,
    pub targets: Vec<TargetModel>,
    pub files: Vec<SourceFileModel>,
    pub modules: Vec<ModuleModel>,
    pub diagnostics: Vec<WorkspaceDiagnostic>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PackageModel {
    pub id: PackageId,
    pub name: String,
    pub manifest_dir: PathBuf,
}

#[derive(Debug, Clone, Serialize)]
pub struct TargetModel {
    pub id: TargetId,
    pub package_id: PackageId,
    pub name: String,
    pub kind: String,
    pub src_path: PathBuf,
}

#[derive(Debug, Clone, Serialize)]
pub struct SourceFileModel {
    pub id: FileId,
    pub path: PathBuf,
    pub package_id: Option<PackageId>,
    pub target_ids: Vec<TargetId>,
    pub role: FileRole,
}

#[derive(Debug, Clone, Serialize)]
pub struct ModuleModel {
    pub id: ModuleId,
    pub file_id: FileId,
    pub package_id: Option<PackageId>,
    pub path: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FileRole {
    CrateRoot,
    ModuleFile,
    ExternalModDeclTarget,
    InlineModuleSource,
    Unowned,
}

#[derive(Debug, Clone, Serialize)]
pub struct WorkspaceDiagnostic {
    pub kind: String,
    pub message: String,
    pub path: Option<PathBuf>,
}

#[derive(Debug, Deserialize)]
struct CargoMetadata {
    packages: Vec<CargoPackage>,
    workspace_root: String,
}

#[derive(Debug, Deserialize)]
struct CargoPackage {
    name: String,
    manifest_path: String,
    targets: Vec<CargoTarget>,
}

#[derive(Debug, Deserialize)]
struct CargoTarget {
    name: String,
    kind: Vec<String>,
    src_path: String,
}

pub fn build_workspace_model(root: &Path) -> Result<WorkspaceModel> {
    let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let metadata = cargo_metadata(&root);
    let mut diagnostics = Vec::new();

    let (workspace_root, packages, targets) = match metadata {
        Ok(meta) => metadata_to_models(meta),
        Err(e) => {
            diagnostics.push(WorkspaceDiagnostic {
                kind: "cargo_metadata_unavailable".to_string(),
                message: format!("cargo metadata unavailable: {e}"),
                path: Some(root.clone()),
            });
            (root.clone(), Vec::new(), Vec::new())
        }
    };

    let rust_files = collect_rust_files(&root)?;
    let target_by_path = target_index(&targets);
    let mut files = Vec::with_capacity(rust_files.len());
    for path in rust_files {
        let id = files.len();
        let package_id = owning_package(&packages, &path).map(|p| p.id);
        let target_ids = target_by_path.get(&path).cloned().unwrap_or_default();
        let role = if package_id.is_none() && !packages.is_empty() {
            FileRole::Unowned
        } else if !target_ids.is_empty() {
            FileRole::CrateRoot
        } else {
            FileRole::ModuleFile
        };
        if role == FileRole::Unowned {
            diagnostics.push(WorkspaceDiagnostic {
                kind: "unowned_file".to_string(),
                message: "Rust file is not inside any discovered Cargo package".to_string(),
                path: Some(path.clone()),
            });
        }
        files.push(SourceFileModel {
            id,
            path,
            package_id,
            target_ids,
            role,
        });
    }

    let modules = derive_modules(&workspace_root, &packages, &mut files, &mut diagnostics);

    Ok(WorkspaceModel {
        root: workspace_root,
        packages,
        targets,
        files,
        modules,
        diagnostics,
    })
}

impl WorkspaceModel {
    pub fn file_for_path(&self, path: &Path) -> Option<&SourceFileModel> {
        let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        self.files.iter().find(|f| f.path == canonical)
    }

    pub fn package(&self, id: PackageId) -> Option<&PackageModel> {
        self.packages.get(id)
    }

    pub fn module_for_file(&self, file_id: FileId) -> Option<&ModuleModel> {
        self.modules.iter().find(|m| m.file_id == file_id)
    }
}

fn cargo_metadata(root: &Path) -> Result<CargoMetadata> {
    let manifest_dir = find_manifest(root)
        .and_then(|p| p.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| root.to_path_buf());
    let output = std::process::Command::new("cargo")
        .arg("metadata")
        .arg("--format-version")
        .arg("1")
        .arg("--no-deps")
        .current_dir(&manifest_dir)
        .output()
        .with_context(|| format!("run cargo metadata in {}", manifest_dir.display()))?;
    if !output.status.success() {
        anyhow::bail!(
            "cargo metadata failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    serde_json::from_slice(&output.stdout).context("parse cargo metadata")
}

fn metadata_to_models(meta: CargoMetadata) -> (PathBuf, Vec<PackageModel>, Vec<TargetModel>) {
    let workspace_root = PathBuf::from(meta.workspace_root);
    let mut packages = Vec::new();
    let mut targets = Vec::new();
    for pkg in meta.packages {
        let package_id = packages.len();
        let manifest_dir = PathBuf::from(&pkg.manifest_path)
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| workspace_root.clone());
        for target in pkg.targets {
            let target_id = targets.len();
            targets.push(TargetModel {
                id: target_id,
                package_id,
                name: target.name,
                kind: target
                    .kind
                    .first()
                    .cloned()
                    .unwrap_or_else(|| "lib".to_string()),
                src_path: PathBuf::from(target.src_path),
            });
        }
        packages.push(PackageModel {
            id: package_id,
            name: pkg.name,
            manifest_dir,
        });
    }
    (workspace_root, packages, targets)
}

fn target_index(targets: &[TargetModel]) -> BTreeMap<PathBuf, Vec<TargetId>> {
    let mut out: BTreeMap<PathBuf, Vec<TargetId>> = BTreeMap::new();
    for target in targets {
        out.entry(canonical_or_self(&target.src_path))
            .or_default()
            .push(target.id);
    }
    out
}

fn owning_package<'a>(packages: &'a [PackageModel], path: &Path) -> Option<&'a PackageModel> {
    packages
        .iter()
        .filter(|pkg| path.starts_with(&pkg.manifest_dir))
        .max_by_key(|pkg| pkg.manifest_dir.components().count())
}

fn derive_modules(
    root: &Path,
    packages: &[PackageModel],
    files: &mut [SourceFileModel],
    diagnostics: &mut Vec<WorkspaceDiagnostic>,
) -> Vec<ModuleModel> {
    let declared_modules = declared_module_targets(root, packages, files, diagnostics);
    let mut modules = Vec::new();
    let mut seen: BTreeSet<(Option<PackageId>, String)> = BTreeSet::new();
    for file in files {
        let path = declared_modules
            .get(&file.id)
            .cloned()
            .unwrap_or_else(|| module_path_for_file(root, packages, file));
        if !seen.insert((file.package_id, path.clone())) {
            diagnostics.push(WorkspaceDiagnostic {
                kind: "duplicate_module_path".to_string(),
                message: format!("multiple files resolve to module path `{path}`"),
                path: Some(file.path.clone()),
            });
        }
        modules.push(ModuleModel {
            id: modules.len(),
            file_id: file.id,
            package_id: file.package_id,
            path,
        });
    }
    modules
}

fn declared_module_targets(
    root: &Path,
    packages: &[PackageModel],
    files: &mut [SourceFileModel],
    diagnostics: &mut Vec<WorkspaceDiagnostic>,
) -> BTreeMap<FileId, String> {
    let file_by_path: BTreeMap<PathBuf, FileId> =
        files.iter().map(|f| (f.path.clone(), f.id)).collect();
    let file_snapshots: Vec<SourceFileModel> = files.to_vec();
    let mut declared = BTreeMap::new();

    for file in &file_snapshots {
        if matches!(file.role, FileRole::Unowned) {
            continue;
        }
        let parent_module = module_path_for_file(root, packages, file);
        let Ok(src) = std::fs::read_to_string(&file.path) else {
            continue;
        };
        let Ok(ast) = syn::parse_file(&src) else {
            diagnostics.push(WorkspaceDiagnostic {
                kind: "unparseable_rust_file".to_string(),
                message: "Rust file could not be parsed while deriving module ownership"
                    .to_string(),
                path: Some(file.path.clone()),
            });
            continue;
        };
        collect_module_decls(
            file,
            &parent_module,
            &ast.items,
            &file_by_path,
            &mut declared,
            diagnostics,
        );
    }

    for file_id in declared.keys().copied() {
        if let Some(file) = files.get_mut(file_id)
            && !matches!(file.role, FileRole::CrateRoot)
        {
            file.role = FileRole::ExternalModDeclTarget;
        }
    }
    declared
}

fn collect_module_decls(
    file: &SourceFileModel,
    parent_module: &str,
    items: &[Item],
    file_by_path: &BTreeMap<PathBuf, FileId>,
    declared: &mut BTreeMap<FileId, String>,
    diagnostics: &mut Vec<WorkspaceDiagnostic>,
) {
    for item in items {
        let Item::Mod(module) = item else {
            continue;
        };
        let name = module.ident.to_string();
        let child_module = if parent_module == "crate" {
            format!("crate::{name}")
        } else {
            format!("{parent_module}::{name}")
        };

        if let Some((_, nested_items)) = &module.content {
            diagnostics.push(WorkspaceDiagnostic {
                kind: "inline_module_source".to_string(),
                message: format!(
                    "inline module `{name}` is represented by its containing file; nested external modules are treated conservatively"
                ),
                path: Some(file.path.clone()),
            });
            collect_module_decls(
                file,
                &child_module,
                nested_items,
                file_by_path,
                declared,
                diagnostics,
            );
            continue;
        }

        let candidates = module_decl_candidates(file, module);
        let existing: Vec<PathBuf> = candidates
            .iter()
            .map(|p| canonical_or_self(p))
            .filter(|p| file_by_path.contains_key(p))
            .collect();
        match existing.as_slice() {
            [target] => {
                let Some(file_id) = file_by_path.get(target).copied() else {
                    continue;
                };
                if let Some(previous) = declared.insert(file_id, child_module.clone())
                    && previous != child_module
                {
                    diagnostics.push(WorkspaceDiagnostic {
                        kind: "duplicate_module_path".to_string(),
                        message: format!(
                            "file is declared as both `{previous}` and `{child_module}`"
                        ),
                        path: Some(target.clone()),
                    });
                }
            }
            [] => diagnostics.push(WorkspaceDiagnostic {
                kind: "unresolved_module_path".to_string(),
                message: format!(
                    "could not resolve external module declaration `{name}` from {}",
                    file.path.display()
                ),
                path: candidates.first().cloned(),
            }),
            many => diagnostics.push(WorkspaceDiagnostic {
                kind: "duplicate_module_file".to_string(),
                message: format!(
                    "module `{name}` can resolve to multiple files: {}",
                    many.iter()
                        .map(|p| p.display().to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
                path: Some(file.path.clone()),
            }),
        }
    }
}

fn module_decl_candidates(parent_file: &SourceFileModel, module: &syn::ItemMod) -> Vec<PathBuf> {
    if let Some(path) = path_attr_value(&module.attrs) {
        return parent_file
            .path
            .parent()
            .map(|dir| vec![dir.join(path)])
            .unwrap_or_default();
    }
    let name = module.ident.to_string();
    let base = module_child_base(&parent_file.path);
    vec![
        base.join(format!("{name}.rs")),
        base.join(&name).join("mod.rs"),
    ]
}

fn module_child_base(parent: &Path) -> PathBuf {
    let Some(parent_dir) = parent.parent() else {
        return PathBuf::new();
    };
    match parent.file_name().and_then(|s| s.to_str()) {
        Some("lib.rs" | "main.rs" | "mod.rs") => parent_dir.to_path_buf(),
        Some(name) => parent_dir.join(name.trim_end_matches(".rs")),
        None => parent_dir.to_path_buf(),
    }
}

fn path_attr_value(attrs: &[syn::Attribute]) -> Option<String> {
    attrs.iter().find_map(|attr| {
        if !attr.path().is_ident("path") {
            return None;
        }
        let syn::Meta::NameValue(name_value) = &attr.meta else {
            return None;
        };
        let syn::Expr::Lit(expr_lit) = &name_value.value else {
            return None;
        };
        let syn::Lit::Str(lit) = &expr_lit.lit else {
            return None;
        };
        Some(lit.value())
    })
}

fn module_path_for_file(root: &Path, packages: &[PackageModel], file: &SourceFileModel) -> String {
    if matches!(file.role, FileRole::CrateRoot) {
        return "crate".to_string();
    }
    let base = file
        .package_id
        .and_then(|id| packages.get(id))
        .map(|pkg| pkg.manifest_dir.join("src"))
        .unwrap_or_else(|| root.join("src"));
    let rel = file.path.strip_prefix(&base).unwrap_or(&file.path);
    let mut parts: Vec<String> = rel
        .components()
        .filter_map(|c| c.as_os_str().to_str().map(|s| s.to_string()))
        .collect();
    if let Some(last) = parts.last_mut() {
        if last == "mod.rs" {
            parts.pop();
        } else if let Some(stem) = last.strip_suffix(".rs") {
            *last = stem.to_string();
        }
    }
    parts.retain(|p| !p.is_empty());
    if parts.is_empty() {
        "crate".to_string()
    } else {
        format!("crate::{}", parts.join("::"))
    }
}

fn collect_rust_files(root: &Path) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let file_type = match entry.file_type() {
                Ok(file_type) => file_type,
                Err(_) => continue,
            };
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
                out.push(canonical_or_self(&path));
            }
        }
    }
    out.sort();
    Ok(out)
}

fn find_manifest(start: &Path) -> Option<PathBuf> {
    let mut cur = if start.is_file() {
        start.parent()?.to_path_buf()
    } else {
        start.to_path_buf()
    };
    if cur.as_os_str().is_empty() || cur.as_os_str() == "." {
        cur = std::env::current_dir().ok()?;
    }
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

fn canonical_or_self(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unowned_file_reports_diagnostic() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("Cargo.toml"),
            r#"[workspace]
members = ["app"]
"#,
        )
        .unwrap();
        std::fs::create_dir_all(tmp.path().join("app/src")).unwrap();
        std::fs::write(
            tmp.path().join("app/Cargo.toml"),
            r#"[package]
name = "app"
version = "0.1.0"
edition = "2021"
"#,
        )
        .unwrap();
        std::fs::write(tmp.path().join("app/src/lib.rs"), "pub fn app() {}\n").unwrap();
        std::fs::write(tmp.path().join("loose.rs"), "fn loose() {}\n").unwrap();

        let model = build_workspace_model(tmp.path()).unwrap();

        assert!(model.diagnostics.iter().any(|d| d.kind == "unowned_file"));
    }

    #[test]
    fn workspace_files_keep_package_ownership() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("Cargo.toml"),
            r#"[workspace]
members = ["a", "b"]
"#,
        )
        .unwrap();
        for name in ["a", "b"] {
            let dir = tmp.path().join(name);
            std::fs::create_dir_all(dir.join("src")).unwrap();
            std::fs::write(
                dir.join("Cargo.toml"),
                format!(
                    r#"[package]
name = "{name}"
version = "0.1.0"
edition = "2021"
"#
                ),
            )
            .unwrap();
            std::fs::write(dir.join("src/lib.rs"), "pub fn root() {}\n").unwrap();
        }

        let model = build_workspace_model(tmp.path()).unwrap();

        assert_eq!(model.packages.len(), 2);
        assert!(model.files.iter().all(|f| f.package_id.is_some()));
    }

    #[test]
    fn nested_external_modules_use_declaration_paths() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("Cargo.toml"),
            r#"[package]
name = "app"
version = "0.1.0"
edition = "2021"
"#,
        )
        .unwrap();
        std::fs::create_dir_all(tmp.path().join("src/foo")).unwrap();
        std::fs::write(tmp.path().join("src/lib.rs"), "mod foo;\n").unwrap();
        std::fs::write(tmp.path().join("src/foo.rs"), "mod bar;\n").unwrap();
        std::fs::write(tmp.path().join("src/foo/bar.rs"), "pub fn bar() {}\n").unwrap();

        let model = build_workspace_model(tmp.path()).unwrap();
        let foo_id = model
            .file_for_path(&tmp.path().join("src/foo.rs"))
            .unwrap()
            .id;
        let bar_id = model
            .file_for_path(&tmp.path().join("src/foo/bar.rs"))
            .unwrap()
            .id;

        assert_eq!(model.module_for_file(foo_id).unwrap().path, "crate::foo");
        assert_eq!(
            model.module_for_file(bar_id).unwrap().path,
            "crate::foo::bar"
        );
        assert_eq!(
            model
                .file_for_path(&tmp.path().join("src/foo.rs"))
                .unwrap()
                .role,
            FileRole::ExternalModDeclTarget
        );
    }

    #[test]
    fn path_attribute_module_is_resolved() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("Cargo.toml"),
            r#"[package]
name = "app"
version = "0.1.0"
edition = "2021"
"#,
        )
        .unwrap();
        std::fs::create_dir_all(tmp.path().join("src")).unwrap();
        std::fs::write(
            tmp.path().join("src/lib.rs"),
            "#[path = \"wire/renamed.rs\"]\nmod api;\n",
        )
        .unwrap();
        std::fs::create_dir_all(tmp.path().join("src/wire")).unwrap();
        std::fs::write(tmp.path().join("src/wire/renamed.rs"), "pub fn api() {}\n").unwrap();

        let model = build_workspace_model(tmp.path()).unwrap();
        let api_id = model
            .file_for_path(&tmp.path().join("src/wire/renamed.rs"))
            .unwrap()
            .id;

        assert_eq!(model.module_for_file(api_id).unwrap().path, "crate::api");
        assert_eq!(
            model
                .file_for_path(&tmp.path().join("src/wire/renamed.rs"))
                .unwrap()
                .role,
            FileRole::ExternalModDeclTarget
        );
    }

    #[test]
    fn duplicate_module_layout_reports_diagnostic() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("Cargo.toml"),
            r#"[package]
name = "app"
version = "0.1.0"
edition = "2021"
"#,
        )
        .unwrap();
        std::fs::create_dir_all(tmp.path().join("src/foo")).unwrap();
        std::fs::write(tmp.path().join("src/lib.rs"), "mod foo;\n").unwrap();
        std::fs::write(tmp.path().join("src/foo.rs"), "pub fn a() {}\n").unwrap();
        std::fs::write(tmp.path().join("src/foo/mod.rs"), "pub fn b() {}\n").unwrap();

        let model = build_workspace_model(tmp.path()).unwrap();

        assert!(
            model
                .diagnostics
                .iter()
                .any(|d| d.kind == "duplicate_module_file")
        );
    }
}
