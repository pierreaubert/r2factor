//! End-to-end MCP server tests. Spawn the binary in `mcp` mode, exchange
//! JSON-RPC messages over its stdio, and assert the response shape. This
//! catches regressions in the wire protocol that unit-level tests would
//! miss (e.g. accidentally printing logs to stdout, breaking JSON-RPC
//! framing).

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_r2factor");

struct McpSession {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl McpSession {
    fn start() -> Self {
        let mut child = Command::new(BIN)
            .arg("mcp")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            // Drain stderr so the test process doesn't block on a full
            // pipe; the protocol stream is stdout only.
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn r2factor mcp");
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
