use r2factor::refactor::{
    RefactorMode, RefactorOptions, refactor_plan, refactor_preview, refactor_rollback_manifest,
    refactor_write,
};
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

fn make_crate() -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().expect("tempdir");
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
        "pub fn user_service() -> i32 { 42 }\n",
    )
    .unwrap();
    let spec = tmp.path().join("spec.yaml");
    fs::write(
        &spec,
        r#"
modules:
  - name: app
    patterns: ["*service"]
    visibility: pub
"#,
    )
    .unwrap();
    (tmp, spec)
}

fn make_crate_with_lib(lib_src: &str) -> (tempfile::TempDir, PathBuf) {
    let (tmp, spec) = make_crate();
    fs::write(tmp.path().join("src/lib.rs"), lib_src).unwrap();
    (tmp, spec)
}

fn make_crate_with_spec(spec_src: &str) -> (tempfile::TempDir, PathBuf) {
    let (tmp, spec) = make_crate();
    fs::write(&spec, spec_src).unwrap();
    (tmp, spec)
}

fn opts(mode: RefactorMode) -> RefactorOptions {
    RefactorOptions {
        use_tokensave: false,
        require_tokensave_for_rewrites: true,
        use_rg_candidates: false,
        llm: None,
        run_full_tests: false,
        mode,
    }
}

#[test]
fn refactor_plan_reports_tokensave_unavailable_but_can_preview() {
    let (tmp, spec_path) = make_crate();
    let spec = r2factor::spec::parse_spec(&spec_path).unwrap();

    let plan = refactor_plan(tmp.path(), &spec, &opts(RefactorMode::Plan)).unwrap();

    assert!(plan.safety.can_preview);
    assert!(
        plan.diagnostics
            .iter()
            .any(|d| d.kind == "tokensave_unavailable")
    );
    assert!(
        plan.edits
            .iter()
            .any(|e| matches!(e, r2factor::refactor::PlanEdit::MoveItem { .. }))
    );
}

#[test]
fn refactor_preview_applies_to_temp_tree_and_checks() {
    let (tmp, spec_path) = make_crate();
    let spec = r2factor::spec::parse_spec(&spec_path).unwrap();

    let preview = refactor_preview(tmp.path(), &spec, &opts(RefactorMode::Preview)).unwrap();

    assert!(preview.passed, "checks: {:?}", preview.checks);
    assert!(preview.temp_root.join("src/app.rs").exists());
    assert!(
        fs::read_to_string(tmp.path().join("src/lib.rs"))
            .unwrap()
            .contains("user_service")
    );
}

#[test]
fn refactor_preview_preserves_inner_attrs_before_generated_preamble() {
    let (tmp, spec_path) =
        make_crate_with_lib("#![allow(dead_code)]\n\npub fn user_service() -> i32 { 42 }\n");
    let spec = r2factor::spec::parse_spec(&spec_path).unwrap();

    let preview = refactor_preview(tmp.path(), &spec, &opts(RefactorMode::Preview)).unwrap();

    assert!(preview.passed, "checks: {:?}", preview.checks);
    let rewritten = fs::read_to_string(preview.temp_root.join("src/lib.rs")).unwrap();
    let attr_pos = rewritten.find("#![allow(dead_code)]").unwrap();
    let mod_pos = rewritten.find("mod app;").unwrap();
    assert!(attr_pos < mod_pos, "rewritten lib.rs:\n{rewritten}");
}

#[test]
fn refactor_preview_escapes_keyword_module_names() {
    let (tmp, spec_path) = make_crate_with_spec(
        r#"
modules:
  - name: type
    patterns: ["*service"]
    visibility: pub
"#,
    );
    let spec = r2factor::spec::parse_spec(&spec_path).unwrap();

    let preview = refactor_preview(tmp.path(), &spec, &opts(RefactorMode::Preview)).unwrap();

    assert!(preview.passed, "checks: {:?}", preview.checks);
    let rewritten = fs::read_to_string(preview.temp_root.join("src/lib.rs")).unwrap();
    assert!(rewritten.contains("#[path = \"type.rs\"]"));
    assert!(rewritten.contains("mod type_;"));
    assert!(rewritten.contains("pub use type_::user_service;"));
}

#[test]
fn refactor_write_creates_manifest_and_rollback_restores_backup() {
    let (tmp, spec_path) = make_crate();
    let spec = r2factor::spec::parse_spec(&spec_path).unwrap();

    let report = refactor_write(tmp.path(), &spec, &opts(RefactorMode::Write)).unwrap();

    assert!(report.passed);
    assert!(!report.rolled_back);
    assert!(report.manifest_path.exists());
    assert!(tmp.path().join("src/app.rs").exists());
    assert!(
        fs::read_to_string(tmp.path().join("src/lib.rs"))
            .unwrap()
            .contains("pub use app::user_service")
    );

    let rollback = refactor_rollback_manifest(&report.manifest_path).unwrap();
    assert!(
        rollback
            .restored
            .iter()
            .any(|p| p.ends_with(Path::new("src/lib.rs")))
    );
    assert!(
        fs::read_to_string(tmp.path().join("src/lib.rs"))
            .unwrap()
            .contains("pub fn user_service")
    );
    assert!(!tmp.path().join("src/app.rs").exists());
}

#[test]
fn refactor_plan_blocks_existing_target_file() {
    let (tmp, spec_path) = make_crate();
    fs::write(tmp.path().join("src/app.rs"), "pub fn existing() {}\n").unwrap();
    let spec = r2factor::spec::parse_spec(&spec_path).unwrap();

    let plan = refactor_plan(tmp.path(), &spec, &opts(RefactorMode::Plan)).unwrap();

    assert!(!plan.safety.can_preview);
    assert!(
        plan.safety
            .blockers
            .iter()
            .any(|blocker| blocker.contains("target file already exists"))
    );
}

#[test]
fn refactor_plan_blocks_name_collision_before_preview() {
    let (tmp, spec_path) = make_crate_with_spec(
        r#"
modules:
  - name: app
    patterns: ["same"]
    visibility: pub
"#,
    );
    fs::create_dir_all(tmp.path().join("src")).unwrap();
    fs::write(tmp.path().join("src/lib.rs"), "mod a;\nmod b;\n").unwrap();
    fs::write(tmp.path().join("src/a.rs"), "pub fn same() {}\n").unwrap();
    fs::write(tmp.path().join("src/b.rs"), "pub fn same() {}\n").unwrap();
    let spec = r2factor::spec::parse_spec(&spec_path).unwrap();

    let plan = refactor_plan(tmp.path(), &spec, &opts(RefactorMode::Plan)).unwrap();

    assert!(!plan.safety.can_preview);
    assert!(
        plan.safety
            .blockers
            .iter()
            .any(|blocker| blocker.contains("name collision"))
    );
}

#[test]
fn refactor_preview_uses_package_scoped_check() {
    let (tmp, spec_path) = make_crate();
    let spec = r2factor::spec::parse_spec(&spec_path).unwrap();

    let preview = refactor_preview(tmp.path(), &spec, &opts(RefactorMode::Preview)).unwrap();

    assert!(preview.passed, "checks: {:?}", preview.checks);
    assert!(
        preview
            .checks
            .iter()
            .any(|check| check.command == "cargo check -p demo")
    );
}

#[test]
fn refactor_preview_can_run_full_tests_when_requested() {
    let (tmp, spec_path) = make_crate_with_lib(
        r#"
pub fn user_service() -> i32 { 42 }

#[cfg(test)]
mod tests {
    #[test]
    fn service_returns_answer() {
        assert_eq!(super::user_service(), 42);
    }
}
"#,
    );
    let spec = r2factor::spec::parse_spec(&spec_path).unwrap();
    let mut options = opts(RefactorMode::Preview);
    options.run_full_tests = true;

    let preview = refactor_preview(tmp.path(), &spec, &options).unwrap();

    assert!(preview.passed, "checks: {:?}", preview.checks);
    assert!(
        preview
            .checks
            .iter()
            .any(|check| check.command == "cargo test -p demo")
    );
}

struct McpSession {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl McpSession {
    fn start() -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_r2factor"))
            .arg("mcp")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn mcp");
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        Self {
            child,
            stdin,
            stdout,
        }
    }

    fn request(&mut self, req: serde_json::Value) -> serde_json::Value {
        writeln!(self.stdin, "{}", req).unwrap();
        self.stdin.flush().unwrap();
        let mut line = String::new();
        self.stdout.read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap()
    }
}

impl Drop for McpSession {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}

#[test]
fn mcp_refactor_plan_and_preview_return_reports() {
    let (tmp, spec_path) = make_crate();
    let mut sess = McpSession::start();

    let plan_resp = sess.request(serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {
            "name": "refactor_plan",
            "arguments": {
                "path": tmp.path(),
                "spec": spec_path,
                "use_tokensave": false,
                "use_rg_candidates": false
            }
        }
    }));
    let plan_text = plan_resp["result"]["content"][0]["text"].as_str().unwrap();
    let plan_payload: serde_json::Value = serde_json::from_str(plan_text).unwrap();
    let plan_data = &plan_payload["data"];
    assert_eq!(plan_data["safety"]["can_preview"], true);
    assert!(
        plan_data["edits"]
            .as_array()
            .unwrap()
            .iter()
            .any(|edit| { edit["edit"] == "move_item" })
    );

    let preview_resp = sess.request(serde_json::json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/call",
        "params": {
            "name": "refactor_preview",
            "arguments": {
                "path": tmp.path(),
                "spec": spec_path,
                "use_tokensave": false,
                "use_rg_candidates": false
            }
        }
    }));
    let preview_text = preview_resp["result"]["content"][0]["text"]
        .as_str()
        .unwrap();
    let preview_payload: serde_json::Value = serde_json::from_str(preview_text).unwrap();
    assert_eq!(preview_payload["data"]["passed"], true);
}

#[test]
fn mcp_refactor_write_requires_confirmation() {
    let (tmp, spec_path) = make_crate();
    let mut sess = McpSession::start();

    let resp = sess.request(serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {
            "name": "refactor_write",
            "arguments": {
                "path": tmp.path(),
                "spec": spec_path,
                "use_tokensave": false,
                "use_rg_candidates": false
            }
        }
    }));
    let text = resp["result"]["content"][0]["text"].as_str().unwrap();
    let payload: serde_json::Value = serde_json::from_str(text).unwrap();
    assert_eq!(payload["ok"], false);
    assert!(payload["error"].as_str().unwrap().contains("confirm"));
}
