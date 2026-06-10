use anyhow::{Result, anyhow};
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::pipeline::SplitWriteTree;
use crate::write::WriteReport;

#[derive(Debug, Clone, serde::Serialize)]
pub struct CargoVerifyReport {
    pub manifest: PathBuf,
    pub commands: Vec<CargoVerifyCommand>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct CargoVerifyCommand {
    pub argv: Vec<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct CargoVerifyFailure {
    pub manifest: Option<PathBuf>,
    pub completed: Vec<CargoVerifyCommand>,
    pub failed: CargoVerifyFailedCommand,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct CargoVerifyFailedCommand {
    pub argv: Vec<String>,
    pub status: String,
    pub stdout: String,
    pub stderr: String,
}

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct SplitRollbackReport {
    pub restored_files: Vec<PathBuf>,
    pub removed_files: Vec<PathBuf>,
    pub removed_dirs: Vec<PathBuf>,
    pub errors: Vec<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct SplitVerifyFailureReport {
    pub bug_report: PathBuf,
    pub rollback: SplitRollbackReport,
}

pub fn verify_split_with_cargo(file: &Path) -> Result<CargoVerifyReport> {
    match run_cargo_verification(file) {
        Ok(report) => Ok(report),
        Err(failure) => {
            let rollback = SplitRollbackReport::default();
            let bug_report = match write_bug_report(file, None, &failure, &rollback) {
                Ok(path) => {
                    eprintln!("[verify] bug report -> {}", path.display());
                    path
                }
                Err(err) => {
                    eprintln!("[verify] failed to write bug report: {err}");
                    PathBuf::new()
                }
            };
            if bug_report.as_os_str().is_empty() {
                Err(anyhow!("{failure}\n[verify] bug report generation failed"))
            } else {
                Err(anyhow!(
                    "{failure}\n[verify] bug report -> {}",
                    bug_report.display()
                ))
            }
        }
    }
}

pub fn verify_split_with_rollback(file: &Path, tree: &SplitWriteTree) -> Result<CargoVerifyReport> {
    match run_cargo_verification(file) {
        Ok(report) => Ok(report),
        Err(failure) => fail_with_rollback_report(file, std::slice::from_ref(tree), *failure),
    }
}

pub fn fail_with_rollback_report(
    file: &Path,
    trees: &[SplitWriteTree],
    failure: CargoVerifyFailure,
) -> Result<CargoVerifyReport> {
    let failure_text = failure.to_string();
    let reports = fail_with_rollback_reports(file, trees, vec![failure])?;
    Err(anyhow!(
        "{failure_text}\n[verify] split was rolled back; bug report -> {}",
        reports
            .bug_reports
            .first()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "<failed>".to_string())
    ))
}

#[derive(Debug, Clone)]
pub struct SplitVerifyFailureReports {
    pub bug_reports: Vec<PathBuf>,
    pub rollback: SplitRollbackReport,
}

pub fn fail_with_rollback_reports(
    file: &Path,
    trees: &[SplitWriteTree],
    failures: Vec<CargoVerifyFailure>,
) -> Result<SplitVerifyFailureReports> {
    let rollback = rollback_split_trees(trees);
    eprintln!(
        "[verify] rolled back split: {} restored, {} removed, {} error(s)",
        rollback.restored_files.len(),
        rollback.removed_files.len(),
        rollback.errors.len()
    );
    let mut bug_reports = Vec::new();
    let mut report_errors = Vec::new();
    for failure in &failures {
        match write_bug_report(file, Some(trees), failure, &rollback) {
            Ok(path) => {
                eprintln!("[verify] bug report -> {}", path.display());
                bug_reports.push(path);
            }
            Err(err) => {
                eprintln!("[verify] failed to write bug report: {err}");
                report_errors.push(err.to_string());
            }
        }
    }
    if bug_reports.is_empty() {
        Err(anyhow!(
            "post-split verification failed for {} command(s)\n[verify] split was rolled back, but bug report generation failed: {}",
            failures.len(),
            report_errors.join("; ")
        ))
    } else {
        Ok(SplitVerifyFailureReports {
            bug_reports,
            rollback,
        })
    }
}

fn run_cargo_verification(
    file: &Path,
) -> std::result::Result<CargoVerifyReport, Box<CargoVerifyFailure>> {
    let manifest = locate_package_manifest(file).ok_or_else(|| {
        Box::new(CargoVerifyFailure::for_launch_error(
            None,
            Vec::new(),
            vec!["cargo".to_string(), "check".to_string()],
            format!(
                "could not find a Cargo.toml with [package] above {}",
                file.display()
            ),
        ))
    })?;
    let commands = vec![
        CargoInvocation::new("check", &manifest),
        CargoInvocation::new("clippy", &manifest),
    ];

    let mut completed = Vec::new();
    for invocation in commands {
        eprintln!("[verify] running {}", invocation.display());
        let output = invocation.command().output().map_err(|err| {
            Box::new(CargoVerifyFailure::for_launch_error(
                Some(manifest.clone()),
                completed.clone(),
                invocation.argv_strings(),
                err.to_string(),
            ))
        })?;
        if !output.status.success() {
            return Err(Box::new(CargoVerifyFailure::for_output(
                manifest.clone(),
                completed.clone(),
                &invocation,
                output,
            )));
        }
        eprintln!("[verify] ok {}", invocation.display());
        completed.push(CargoVerifyCommand {
            argv: invocation.argv_strings(),
        });
    }

    Ok(CargoVerifyReport {
        manifest,
        commands: completed,
    })
}

pub fn rollback_split_tree(tree: &SplitWriteTree) -> SplitRollbackReport {
    rollback_split_trees(std::slice::from_ref(tree))
}

pub fn rollback_split_trees(trees: &[SplitWriteTree]) -> SplitRollbackReport {
    let mut report = SplitRollbackReport::default();
    for tree in trees.iter().rev() {
        rollback_tree_inner(tree, &mut report);
    }
    report
}

fn rollback_tree_inner(tree: &SplitWriteTree, rollback: &mut SplitRollbackReport) {
    for child in tree.children.iter().rev() {
        rollback_tree_inner(child, rollback);
        remove_generated_child_backup(&child.report.backup, rollback);
    }
    rollback_write_report(&tree.report, rollback);
}

fn remove_generated_child_backup(path: &Path, rollback: &mut SplitRollbackReport) {
    match fs::remove_file(path) {
        Ok(()) => rollback.removed_files.push(path.to_path_buf()),
        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
        Err(err) => rollback
            .errors
            .push(format!("remove generated backup {}: {err}", path.display())),
    }
}

fn rollback_write_report(report: &WriteReport, rollback: &mut SplitRollbackReport) {
    match fs::copy(&report.backup, &report.facade) {
        Ok(_) => rollback.restored_files.push(report.facade.clone()),
        Err(err) => rollback.errors.push(format!(
            "restore {} from {}: {err}",
            report.facade.display(),
            report.backup.display()
        )),
    }

    for path in report.written_files.iter().rev() {
        if path == &report.facade {
            continue;
        }
        match fs::remove_file(path) {
            Ok(()) => rollback.removed_files.push(path.clone()),
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(err) => rollback
                .errors
                .push(format!("remove generated file {}: {err}", path.display())),
        }
    }

    for removed in report.removed_file_backups.iter().rev() {
        match fs::copy(&removed.backup, &removed.path) {
            Ok(_) => rollback.restored_files.push(removed.path.clone()),
            Err(err) => rollback.errors.push(format!(
                "restore removed file {} from {}: {err}",
                removed.path.display(),
                removed.backup.display()
            )),
        }
    }

    if let Some(dir) = &report.target_dir {
        match fs::remove_dir(dir) {
            Ok(()) => rollback.removed_dirs.push(dir.clone()),
            Err(err)
                if matches!(
                    err.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::DirectoryNotEmpty
                ) => {}
            Err(err) => rollback
                .errors
                .push(format!("remove generated dir {}: {err}", dir.display())),
        }
    }
}

fn write_bug_report(
    file: &Path,
    trees: Option<&[SplitWriteTree]>,
    failure: &CargoVerifyFailure,
    rollback: &SplitRollbackReport,
) -> Result<PathBuf> {
    let dir = bug_report_dir(file, failure);
    fs::create_dir_all(&dir)?;
    let path = unique_bug_report_path(&dir)?;
    fs::write(&path, render_bug_report(file, trees, failure, rollback))?;
    Ok(path)
}

fn bug_report_dir(file: &Path, failure: &CargoVerifyFailure) -> PathBuf {
    failure
        .manifest
        .as_ref()
        .and_then(|manifest| manifest.parent())
        .or_else(|| file.parent())
        .unwrap_or_else(|| Path::new("."))
        .join(".r2factor")
        .join("bug-reports")
}

fn unique_bug_report_path(dir: &Path) -> Result<PathBuf> {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    for suffix in 0..1000 {
        let name = if suffix == 0 {
            format!("split-verify-failure-{secs}.md")
        } else {
            format!("split-verify-failure-{secs}-{suffix}.md")
        };
        let path = dir.join(name);
        if !path.exists() {
            return Ok(path);
        }
    }
    Err(anyhow!(
        "could not allocate unique bug report path in {}",
        dir.display()
    ))
}

fn render_bug_report(
    file: &Path,
    trees: Option<&[SplitWriteTree]>,
    failure: &CargoVerifyFailure,
    rollback: &SplitRollbackReport,
) -> String {
    use std::fmt::Write as _;

    let mut out = String::new();
    let _ = writeln!(out, "# r2factor split verification failure");
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "Review this report before sharing it. Remove proprietary code, private paths, credentials, tokens, and any sensitive build output."
    );
    let _ = writeln!(out);
    let _ = writeln!(out, "## Input");
    let _ = writeln!(out);
    let _ = writeln!(out, "- file: `{}`", file.display());
    if let Some(manifest) = &failure.manifest {
        let _ = writeln!(out, "- manifest: `{}`", manifest.display());
    }
    let _ = writeln!(out);
    let _ = writeln!(out, "## Failed Command");
    let _ = writeln!(out);
    let _ = writeln!(out, "- command: `{}`", failure.failed.argv.join(" "));
    let _ = writeln!(out, "- status: `{}`", failure.failed.status);
    let _ = writeln!(out);
    let _ = writeln!(out, "## Rollback");
    let _ = writeln!(out);
    let _ = writeln!(out, "- restored files: {}", rollback.restored_files.len());
    for path in &rollback.restored_files {
        let _ = writeln!(out, "  - `{}`", path.display());
    }
    let _ = writeln!(
        out,
        "- removed generated files: {}",
        rollback.removed_files.len()
    );
    for path in &rollback.removed_files {
        let _ = writeln!(out, "  - `{}`", path.display());
    }
    let _ = writeln!(
        out,
        "- removed generated dirs: {}",
        rollback.removed_dirs.len()
    );
    for path in &rollback.removed_dirs {
        let _ = writeln!(out, "  - `{}`", path.display());
    }
    if !rollback.errors.is_empty() {
        let _ = writeln!(out, "- rollback errors:");
        for err in &rollback.errors {
            let _ = writeln!(out, "  - `{}`", err);
        }
    }
    let _ = writeln!(out);
    let _ = writeln!(out, "## Split Tree");
    if let Some(trees) = trees {
        for tree in trees {
            render_tree(&mut out, tree, 0);
        }
    } else {
        let _ = writeln!(out);
        let _ = writeln!(
            out,
            "- not available; verification ran without a split write tree"
        );
    }
    let _ = writeln!(out);
    let _ = writeln!(out, "## Stdout");
    let _ = writeln!(out);
    let _ = writeln!(out, "```text");
    out.push_str(&failure.failed.stdout);
    if !failure.failed.stdout.ends_with('\n') {
        let _ = writeln!(out);
    }
    let _ = writeln!(out, "```");
    let _ = writeln!(out);
    let _ = writeln!(out, "## Stderr");
    let _ = writeln!(out);
    let _ = writeln!(out, "```text");
    out.push_str(&failure.failed.stderr);
    if !failure.failed.stderr.ends_with('\n') {
        let _ = writeln!(out);
    }
    let _ = writeln!(out, "```");
    out
}

fn render_tree(out: &mut String, tree: &SplitWriteTree, depth: usize) {
    use std::fmt::Write as _;

    let indent = "  ".repeat(depth);
    let _ = writeln!(out, "{indent}- facade: `{}`", tree.report.facade.display());
    let _ = writeln!(out, "{indent}  backup: `{}`", tree.report.backup.display());
    if let Some(dir) = &tree.report.target_dir {
        let _ = writeln!(out, "{indent}  target: `{}`", dir.display());
    }
    for file in &tree.report.written_files {
        let _ = writeln!(out, "{indent}  wrote: `{}`", file.display());
    }
    for child in &tree.children {
        render_tree(out, child, depth + 1);
    }
}

pub fn locate_package_manifest(path: &Path) -> Option<PathBuf> {
    let mut dir = manifest_search_start(path)?;
    loop {
        let manifest = dir.join("Cargo.toml");
        if manifest.is_file() && manifest_has_package(&manifest) {
            return Some(manifest);
        }
        if !dir.pop() {
            return None;
        }
    }
}

fn manifest_search_start(path: &Path) -> Option<PathBuf> {
    let mut dir = if path.is_dir() {
        path.to_path_buf()
    } else {
        path.parent()?.to_path_buf()
    };
    if dir.as_os_str().is_empty() || dir.as_os_str() == "." {
        dir = std::env::current_dir().ok()?;
    } else if dir.is_relative() {
        dir = std::env::current_dir().ok()?.join(dir);
    }
    Some(dir)
}

fn manifest_has_package(manifest: &Path) -> bool {
    std::fs::read_to_string(manifest)
        .map(|src| src.lines().any(|line| line.trim() == "[package]"))
        .unwrap_or(false)
}

struct CargoInvocation {
    subcommand: &'static str,
    manifest: PathBuf,
}

impl CargoInvocation {
    fn new(subcommand: &'static str, manifest: &Path) -> Self {
        Self {
            subcommand,
            manifest: manifest.to_path_buf(),
        }
    }

    fn command(&self) -> Command {
        let mut command = Command::new("cargo");
        command
            .arg(self.subcommand)
            .arg("--manifest-path")
            .arg(&self.manifest);
        command
    }

    fn display(&self) -> String {
        self.argv_strings().join(" ")
    }

    fn argv_strings(&self) -> Vec<String> {
        vec![
            "cargo".to_string(),
            self.subcommand.to_string(),
            "--manifest-path".to_string(),
            self.manifest.display().to_string(),
        ]
    }
}

impl CargoVerifyFailure {
    pub(crate) fn for_command_output(
        manifest: Option<PathBuf>,
        completed: Vec<CargoVerifyCommand>,
        argv: Vec<String>,
        output: Output,
    ) -> Self {
        Self {
            manifest,
            completed,
            failed: CargoVerifyFailedCommand {
                argv,
                status: output.status.to_string(),
                stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            },
        }
    }

    fn for_output(
        manifest: PathBuf,
        completed: Vec<CargoVerifyCommand>,
        invocation: &CargoInvocation,
        output: Output,
    ) -> Self {
        Self {
            manifest: Some(manifest),
            completed,
            failed: CargoVerifyFailedCommand {
                argv: invocation.argv_strings(),
                status: output.status.to_string(),
                stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            },
        }
    }

    pub(crate) fn for_launch_error(
        manifest: Option<PathBuf>,
        completed: Vec<CargoVerifyCommand>,
        argv: Vec<String>,
        message: String,
    ) -> Self {
        Self {
            manifest,
            completed,
            failed: CargoVerifyFailedCommand {
                argv,
                status: "not run".to_string(),
                stdout: String::new(),
                stderr: message,
            },
        }
    }
}

impl fmt::Display for CargoVerifyFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "post-split verification failed: {}\nstatus: {}",
            self.failed.argv.join(" "),
            self.failed.status
        )
    }
}

impl std::error::Error for CargoVerifyFailure {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::SplitWriteTree;
    use crate::write::{RemovedFileBackup, WriteReport};
    use std::sync::{Mutex, OnceLock};

    #[test]
    fn locate_package_manifest_finds_nearest_package() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            tmp.path().join("Cargo.toml"),
            "[workspace]\nmembers = [\"crates/app\"]\n",
        )
        .expect("write workspace manifest");
        let crate_dir = tmp.path().join("crates/app");
        std::fs::create_dir_all(crate_dir.join("src/nested")).expect("mkdirs");
        std::fs::write(
            crate_dir.join("Cargo.toml"),
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n",
        )
        .expect("write package manifest");
        let src = crate_dir.join("src/nested/mod.rs");
        std::fs::write(&src, "").expect("write src");
        let expected = crate_dir.join("Cargo.toml");

        assert_eq!(
            locate_package_manifest(&src).as_deref(),
            Some(expected.as_path())
        );
    }

    #[test]
    fn locate_package_manifest_resolves_relative_file_from_src_dir() {
        let _lock = current_dir_lock().lock().expect("lock cwd");
        let tmp = tempfile::tempdir().expect("tempdir");
        let crate_dir = tmp.path().join("crates/app");
        let src_dir = crate_dir.join("src");
        std::fs::create_dir_all(&src_dir).expect("mkdir src");
        std::fs::write(
            crate_dir.join("Cargo.toml"),
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n",
        )
        .expect("write package manifest");
        std::fs::write(src_dir.join("lib.rs"), "").expect("write lib");
        let _cwd = CurrentDirGuard::push(&src_dir);
        let expected = src_dir
            .canonicalize()
            .expect("canonical src dir")
            .parent()
            .expect("crate dir")
            .join("Cargo.toml");

        assert_eq!(
            locate_package_manifest(Path::new("lib.rs")).as_deref(),
            Some(expected.as_path())
        );
    }

    #[test]
    fn locate_package_manifest_ignores_workspace_only_manifest() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(tmp.path().join("Cargo.toml"), "[workspace]\n").expect("write manifest");
        let src = tmp.path().join("src/lib.rs");
        std::fs::create_dir_all(src.parent().unwrap()).expect("mkdir src");
        std::fs::write(&src, "").expect("write src");

        assert_eq!(locate_package_manifest(&src), None);
    }

    #[test]
    fn cargo_invocation_renders_expected_command() {
        let invocation = CargoInvocation::new("check", Path::new("/tmp/demo/Cargo.toml"));
        assert_eq!(
            invocation.argv_strings(),
            vec!["cargo", "check", "--manifest-path", "/tmp/demo/Cargo.toml"]
        );
    }

    #[test]
    fn rollback_restores_facade_and_removes_generated_files() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let facade = tmp.path().join("foo.rs");
        let backup = tmp.path().join("foo.rs.bak");
        let target = tmp.path().join("foo");
        let generated = target.join("part.rs");
        fs::create_dir_all(&target).expect("mkdir target");
        fs::write(&facade, "split").expect("write facade");
        fs::write(&backup, "original").expect("write backup");
        fs::write(&generated, "generated").expect("write generated");
        let tree = SplitWriteTree {
            report: WriteReport {
                backup,
                removed_file_backups: Vec::new(),
                target_dir: Some(target.clone()),
                written_files: vec![generated.clone()],
                facade: facade.clone(),
            },
            children: Vec::new(),
        };

        let report = rollback_split_tree(&tree);

        assert!(
            report.errors.is_empty(),
            "rollback errors: {:?}",
            report.errors
        );
        assert_eq!(
            fs::read_to_string(&facade).expect("read facade"),
            "original"
        );
        assert!(!generated.exists(), "generated file should be removed");
        assert!(
            !target.exists(),
            "empty generated target dir should be removed"
        );
    }

    #[test]
    fn rollback_restores_force_removed_target_files() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let facade = tmp.path().join("foo.rs");
        let backup = tmp.path().join("foo.rs.bak");
        let target = tmp.path().join("foo");
        let replaced = target.join("part.rs");
        let replaced_backup = target.join("part.rs.bak");
        fs::create_dir_all(&target).expect("mkdir target");
        fs::write(&facade, "split").expect("write facade");
        fs::write(&backup, "original").expect("write backup");
        fs::write(&replaced, "generated").expect("write generated replacement");
        fs::write(&replaced_backup, "preexisting").expect("write replaced backup");
        let tree = SplitWriteTree {
            report: WriteReport {
                backup,
                removed_file_backups: vec![RemovedFileBackup {
                    path: replaced.clone(),
                    backup: replaced_backup,
                }],
                target_dir: Some(target.clone()),
                written_files: vec![replaced.clone()],
                facade: facade.clone(),
            },
            children: Vec::new(),
        };

        let report = rollback_split_tree(&tree);

        assert!(
            report.errors.is_empty(),
            "rollback errors: {:?}",
            report.errors
        );
        assert_eq!(
            fs::read_to_string(&facade).expect("read facade"),
            "original"
        );
        assert_eq!(
            fs::read_to_string(&replaced).expect("read restored target"),
            "preexisting"
        );
        assert!(
            target.exists(),
            "target dir should remain for restored file"
        );
    }

    #[test]
    fn verify_failure_rolls_back_and_writes_bug_report() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let facade = tmp.path().join("foo.rs");
        let backup = tmp.path().join("foo.rs.bak");
        let target = tmp.path().join("foo");
        let generated = target.join("part.rs");
        fs::create_dir_all(&target).expect("mkdir target");
        fs::write(&facade, "split").expect("write facade");
        fs::write(&backup, "original").expect("write backup");
        fs::write(&generated, "generated").expect("write generated");
        let tree = SplitWriteTree {
            report: WriteReport {
                backup,
                removed_file_backups: Vec::new(),
                target_dir: Some(target),
                written_files: vec![generated.clone()],
                facade: facade.clone(),
            },
            children: Vec::new(),
        };

        let err = verify_split_with_rollback(&facade, &tree).expect_err("verification fails");

        assert!(err.to_string().contains("split was rolled back"));
        assert_eq!(
            fs::read_to_string(&facade).expect("read facade"),
            "original"
        );
        assert!(!generated.exists(), "generated file should be removed");
        let report_dir = tmp.path().join(".r2factor/bug-reports");
        let reports = fs::read_dir(&report_dir)
            .expect("bug report dir")
            .collect::<std::result::Result<Vec<_>, _>>()
            .expect("bug report entries");
        assert_eq!(reports.len(), 1);
        let report = fs::read_to_string(reports[0].path()).expect("read bug report");
        assert!(report.contains("Remove proprietary code"), "{report}");
        assert!(report.contains("could not find a Cargo.toml"), "{report}");
    }

    #[test]
    fn verify_cargo_failure_without_tree_writes_bug_report() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let facade = tmp.path().join("foo.rs");
        fs::write(&facade, "pub struct Foo;\n").expect("write facade");

        let err = verify_split_with_cargo(&facade).expect_err("verification fails");

        assert!(err.to_string().contains("bug report ->"), "{err}");
        let report_dir = tmp.path().join(".r2factor/bug-reports");
        let reports = fs::read_dir(&report_dir)
            .expect("bug report dir")
            .collect::<std::result::Result<Vec<_>, _>>()
            .expect("bug report entries");
        assert_eq!(reports.len(), 1);
        let report = fs::read_to_string(reports[0].path()).expect("read bug report");
        assert!(report.contains("could not find a Cargo.toml"), "{report}");
        assert!(
            report.contains("verification ran without a split write tree"),
            "{report}"
        );
    }

    fn current_dir_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    struct CurrentDirGuard {
        original: PathBuf,
    }

    impl CurrentDirGuard {
        fn push(dir: &Path) -> Self {
            let original = std::env::current_dir().expect("current dir");
            std::env::set_current_dir(dir).expect("set current dir");
            Self { original }
        }
    }

    impl Drop for CurrentDirGuard {
        fn drop(&mut self) {
            let _ = std::env::set_current_dir(&self.original);
        }
    }
}
