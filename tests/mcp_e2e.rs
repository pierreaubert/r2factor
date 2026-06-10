//! End-to-end MCP server tests. Spawn the binary in `mcp` mode, exchange
//! JSON-RPC messages over its stdio, and assert the response shape. This
//! catches regressions in the wire protocol that unit-level tests would
//! miss (e.g. accidentally printing logs to stdout, breaking JSON-RPC
//! framing).

use std::io::{BufRead, BufReader, Write};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_r2factor");

struct McpSession {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl McpSession {
    fn start() -> Self {
        Self::start_with_env([])
    }

    fn start_with_env<const N: usize>(envs: [(&str, String); N]) -> Self {
        let mut command = Command::new(BIN);
        command
            .arg("mcp")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            // Drain stderr so the test process doesn't block on a full
            // pipe; the protocol stream is stdout only.
            .stderr(Stdio::null());
        for (key, value) in envs {
            command.env(key, value);
        }
        let mut child = command.spawn().expect("spawn r2factor mcp");
        let stdin = child.stdin.take().expect("stdin");
        let stdout = BufReader::new(child.stdout.take().expect("stdout"));
        Self {
            child,
            stdin,
            stdout,
        }
    }

    /// Send a request and read its response. The MCP protocol over stdio
    /// is newline-delimited, so one line in == one line out for requests
    /// that carry an `id`.
    fn request(&mut self, body: serde_json::Value) -> serde_json::Value {
        let line = serde_json::to_string(&body).expect("encode");
        writeln!(self.stdin, "{line}").expect("write request");
        self.stdin.flush().expect("flush");
        let mut resp_line = String::new();
        self.stdout
            .read_line(&mut resp_line)
            .expect("read response");
        serde_json::from_str(resp_line.trim()).expect("decode response")
    }
}

#[cfg(unix)]
fn write_fake_cargo(bin_dir: &std::path::Path) -> std::path::PathBuf {
    write_fake_cargo_script(
        bin_dir,
        "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$R2FACTOR_FAKE_CARGO_LOG\"\n",
    )
}

#[cfg(unix)]
fn write_failing_fake_cargo(bin_dir: &std::path::Path) -> std::path::PathBuf {
    write_fake_cargo_script(
        bin_dir,
        "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$R2FACTOR_FAKE_CARGO_LOG\"\nprintf 'synthetic cargo failure\\n' >&2\nexit 1\n",
    )
}

#[cfg(unix)]
fn write_fake_cargo_script(bin_dir: &std::path::Path, script: &str) -> std::path::PathBuf {
    let cargo = bin_dir.join("cargo");
    std::fs::write(&cargo, script).expect("write fake cargo");
    let mut perms = std::fs::metadata(&cargo)
        .expect("fake cargo metadata")
        .permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&cargo, perms).expect("chmod fake cargo");
    cargo
}

impl Drop for McpSession {
    fn drop(&mut self) {
        // Send EOF on stdin so the server exits its read loop. We can't
        // easily move out of `&mut self`, so just kill the child if it's
        // still alive — these are short-lived test sessions.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn tool_payload(resp: &serde_json::Value) -> serde_json::Value {
    serde_json::from_str(
        resp["result"]["content"][0]["text"]
            .as_str()
            .expect("text payload"),
    )
    .expect("tool payload envelope is JSON")
}

fn tool_data(resp: &serde_json::Value) -> serde_json::Value {
    let payload = tool_payload(resp);
    assert_eq!(payload["ok"], true);
    assert_eq!(payload["format"], "json");
    payload["data"].clone()
}

fn tool_error(resp: &serde_json::Value) -> serde_json::Value {
    assert_eq!(resp["result"]["isError"], true);
    let payload = tool_payload(resp);
    assert_eq!(payload["ok"], false);
    payload
}

#[test]
fn initialize_returns_protocol_info() {
    let mut sess = McpSession::start();
    let resp = sess.request(serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {},
    }));
    let result = &resp["result"];
    assert_eq!(result["serverInfo"]["name"], "r2factor");
    assert!(result["protocolVersion"].is_string());
    assert!(result["capabilities"]["tools"].is_object());
}

#[test]
fn tools_list_advertises_available_tools() {
    let mut sess = McpSession::start();
    let resp = sess.request(serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/list",
        "params": {},
    }));
    let tools = resp["result"]["tools"]
        .as_array()
        .expect("tools is an array");
    let names: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
    assert!(names.contains(&"split_dry_run"));
    assert!(names.contains(&"split_write"));
    assert!(names.contains(&"combine_dry_run"));
    assert!(names.contains(&"combine_write"));
    assert!(names.contains(&"combine_suggest"));
    assert!(names.contains(&"check"));
    assert!(names.contains(&"backups_list"));
    assert!(names.contains(&"backup_restore"));
    assert!(names.contains(&"consolidate_dry_run"));
    assert!(names.contains(&"consolidate_write"));
    assert!(names.contains(&"flatten_dry_run"));
    assert!(names.contains(&"flatten_write"));
    // Each tool MUST carry an inputSchema or MCP clients can't render it.
    for t in tools {
        assert!(
            t["inputSchema"]["type"] == "object",
            "{} missing inputSchema",
            t["name"]
        );
    }
}

#[cfg(unix)]
#[test]
fn split_write_tool_runs_cargo_verification_for_package_file() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let src_dir = tmp.path().join("src");
    std::fs::create_dir_all(&src_dir).expect("mkdir src");
    std::fs::write(
        tmp.path().join("Cargo.toml"),
        "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )
    .expect("write manifest");
    let lib = src_dir.join("lib.rs");
    std::fs::write(
        &lib,
        "pub struct Alpha;\npub struct Beta;\npub struct Gamma;\n",
    )
    .expect("write lib");
    let fake_bin = tmp.path().join("fake-bin");
    std::fs::create_dir_all(&fake_bin).expect("mkdir fake bin");
    write_fake_cargo(&fake_bin);
    let log = tmp.path().join("cargo.log");
    let path = format!(
        "{}:{}",
        fake_bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );

    let mut sess = McpSession::start_with_env([
        ("PATH", path),
        ("R2FACTOR_FAKE_CARGO_LOG", log.display().to_string()),
    ]);
    let resp = sess.request(serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {
            "name": "split_write",
            "arguments": {
                "file": lib,
                "use_tokensave": false,
                "max_lines": 0
            },
        },
    }));

    assert!(resp["result"]["isError"].is_null(), "{resp:#}");
    let _tree = tool_data(&resp);
    let log = std::fs::read_to_string(&log).expect("read fake cargo log");
    assert!(log.contains("check --manifest-path"), "log:\n{log}");
    assert!(log.contains("clippy --manifest-path"), "log:\n{log}");
}

#[cfg(unix)]
#[test]
fn split_write_tool_writes_bug_report_when_cargo_verification_fails() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let src_dir = tmp.path().join("src");
    std::fs::create_dir_all(&src_dir).expect("mkdir src");
    std::fs::write(
        tmp.path().join("Cargo.toml"),
        "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )
    .expect("write manifest");
    let lib = src_dir.join("lib.rs");
    std::fs::write(
        &lib,
        "pub struct Alpha;\npub struct Beta;\npub struct Gamma;\n",
    )
    .expect("write lib");
    let fake_bin = tmp.path().join("fake-bin");
    std::fs::create_dir_all(&fake_bin).expect("mkdir fake bin");
    write_failing_fake_cargo(&fake_bin);
    let log = tmp.path().join("cargo.log");
    let path = format!(
        "{}:{}",
        fake_bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );

    let mut sess = McpSession::start_with_env([
        ("PATH", path),
        ("R2FACTOR_FAKE_CARGO_LOG", log.display().to_string()),
    ]);
    let resp = sess.request(serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {
            "name": "split_write",
            "arguments": {
                "file": lib,
                "use_tokensave": false,
                "max_lines": 0
            },
        },
    }));

    let payload = tool_error(&resp);
    let err = payload["error"].as_str().expect("error text");
    assert!(err.contains("bug report ->"), "{err}");
    let report_dir = tmp.path().join(".r2factor/bug-reports");
    let reports = std::fs::read_dir(&report_dir)
        .expect("bug report dir")
        .collect::<std::result::Result<Vec<_>, _>>()
        .expect("bug report entries");
    assert_eq!(reports.len(), 1);
    let report = std::fs::read_to_string(reports[0].path()).expect("read bug report");
    assert!(report.contains("synthetic cargo failure"), "{report}");
    assert!(report.contains("## Split Tree"), "{report}");
    assert!(
        std::fs::read_to_string(&lib)
            .expect("read restored lib")
            .contains("pub struct Alpha;")
    );
}

#[test]
fn backups_list_tool_returns_backup_json() {
    let tmp = tempfile::tempdir().expect("tempdir");
    std::fs::write(tmp.path().join("foo.rs.bak"), "old").unwrap();

    let mut sess = McpSession::start();
    let resp = sess.request(serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {
            "name": "backups_list",
            "arguments": {
                "path": tmp.path(),
            },
        },
    }));
    let backups = tool_data(&resp);
    let backups = backups.as_array().expect("backup array");
    assert_eq!(backups.len(), 1);
    assert!(
        backups[0]["restore_target"]
            .as_str()
            .unwrap()
            .ends_with("foo.rs")
    );
}

#[test]
fn backup_restore_tool_restores_with_force() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let target = tmp.path().join("foo.rs");
    let backup = tmp.path().join("foo.rs.bak");
    std::fs::write(&target, "new").unwrap();
    std::fs::write(&backup, "old").unwrap();

    let mut sess = McpSession::start();
    let resp = sess.request(serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {
            "name": "backup_restore",
            "arguments": {
                "backup": backup,
                "force": true,
            },
        },
    }));
    assert!(resp["result"]["isError"].is_null());
    let report = tool_data(&resp);
    assert_eq!(report["replaced_existing"], true);
    assert_eq!(std::fs::read_to_string(target).unwrap(), "old");
}

#[test]
fn combine_suggest_tool_returns_ranked_json() {
    let tmp = tempfile::tempdir().expect("tempdir");
    std::fs::write(tmp.path().join("parser.rs"), "pub fn parse() {}\n").unwrap();
    std::fs::write(
        tmp.path().join("lexer.rs"),
        "pub fn lex() { crate::parser::parse(); }\n",
    )
    .unwrap();

    let mut sess = McpSession::start();
    let resp = sess.request(serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {
            "name": "combine_suggest",
            "arguments": {
                "path": tmp.path(),
            },
        },
    }));
    let report = tool_data(&resp);
    assert_eq!(report["suggestions"].as_array().unwrap().len(), 1);
    assert!(
        report["suggestions"][0]["reasons"]
            .as_array()
            .unwrap()
            .iter()
            .any(|reason| reason.as_str().unwrap().contains("references `parser`"))
    );
}

#[test]
fn split_dry_run_returns_plan_and_cohesion() {
    let mut sess = McpSession::start();
    let resp = sess.request(serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {
            "name": "split_dry_run",
            "arguments": {
                "file": "fixtures/sample.rs",
                "use_tokensave": false,
            },
        },
    }));
    assert_eq!(resp["result"]["content"][0]["type"], "text");
    let report = tool_data(&resp);
    assert!(report["plan"]["total_items"].as_u64().unwrap() > 0);
    assert!(!report["plan"]["buckets"].as_array().unwrap().is_empty());
    // Cohesion score is always present, even for a trivially-clustered
    // file (1.0 when there are no cross-bucket refs at all).
    let score = report["cohesion"]["score"].as_f64().unwrap();
    assert!((0.0..=1.0).contains(&score));
}

#[test]
fn split_dry_run_reports_error_on_missing_file() {
    let mut sess = McpSession::start();
    let resp = sess.request(serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {
            "name": "split_dry_run",
            "arguments": { "file": "/definitely/does/not/exist.rs" },
        },
    }));
    // MCP convention: tool failures land in `result.content` with
    // `isError: true`, NOT as JSON-RPC errors. Verify that's what we do.
    assert!(resp["error"].is_null(), "should not be a JSON-RPC error");
    assert_eq!(resp["result"]["isError"], true);
    let payload = tool_payload(&resp);
    assert_eq!(payload["ok"], false);
    assert_eq!(payload["tool"], "split_dry_run");
    assert!(payload["error"].as_str().unwrap().contains("read "));
}

#[test]
fn unknown_method_returns_jsonrpc_error() {
    let mut sess = McpSession::start();
    let resp = sess.request(serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "wat/no",
        "params": {},
    }));
    // Protocol-level (not tool-level) failures DO bubble up as JSON-RPC
    // errors per spec.
    assert!(resp["error"]["code"].is_i64());
    assert!(
        resp["error"]["message"]
            .as_str()
            .unwrap()
            .contains("unknown method")
    );
}

#[test]
fn discover_tool_returns_index_and_edges() {
    let tmp = tempfile::tempdir().expect("tempdir");
    std::fs::write(tmp.path().join("a.rs"), "pub fn helper() {}\n").unwrap();
    std::fs::write(tmp.path().join("b.rs"), "pub fn caller() { helper(); }\n").unwrap();

    let mut sess = McpSession::start();
    let resp = sess.request(serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {
            "name": "discover",
            "arguments": {
                "path": tmp.path(),
            },
        },
    }));
    let data = tool_data(&resp);
    let files = data["files"].as_array().expect("files array");
    assert_eq!(files.len(), 2);
    let items = data["items"].as_array().expect("items array");
    assert_eq!(items.len(), 2);
    let edges = data["edges"].as_array().expect("edges array");
    assert!(
        !edges.is_empty(),
        "should find at least one cross-file edge"
    );
}

#[test]
fn spec_validate_tool_reports_errors() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let spec_path = tmp.path().join("bad.yaml");
    std::fs::write(
        &spec_path,
        r#"
modules:
  - name: dup
    patterns: ["*A"]
    visibility: pub
  - name: dup
    patterns: ["*B"]
    visibility: bad_vis
"#,
    )
    .unwrap();

    let mut sess = McpSession::start();
    let resp = sess.request(serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {
            "name": "spec_validate",
            "arguments": {
                "spec": spec_path,
            },
        },
    }));
    let data = tool_data(&resp);
    assert_eq!(data["valid"], false);
    let errors = data["errors"].as_array().expect("errors array");
    assert!(
        errors
            .iter()
            .any(|e| e.as_str().unwrap().contains("duplicate"))
    );
    assert!(
        errors
            .iter()
            .any(|e| e.as_str().unwrap().contains("visibility"))
    );
}
