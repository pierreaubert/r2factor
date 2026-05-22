//! MCP (Model Context Protocol) server for r2factor. Speaks newline-
//! delimited JSON-RPC 2.0 over stdio so an AI agent (Claude Code, an MCP-
//! aware IDE, …) can discover and invoke r2factor's split pipeline as
//! first-class tools.
//!
//! The core tools are:
//!   * `split_dry_run` — analyze a file, return the proposed plan and
//!     cohesion report as JSON.
//!   * `split_write` — actually materialize the split (destructive: takes
//!     a `force` flag).
//!   * `consolidate_*` — merge a facade + sub-files into inline modules.
//!   * `flatten_*` — dissolve those inline modules into one scope.
//!
//! The protocol stream lives on stdout; logs and diagnostics go to stderr,
//! which most MCP clients display in a separate channel.

use anyhow::Result;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;

const PROTOCOL_VERSION: &str = "2024-11-05";
const SERVER_NAME: &str = "r2factor";
const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");

pub fn serve() -> Result<()> {
    let stdin = std::io::stdin();
    let mut stdin = BufReader::new(stdin.lock());
    let stdout = std::io::stdout();
    let mut stdout = stdout.lock();

    let mut line = String::new();
    loop {
        line.clear();
        let n = stdin.read_line(&mut line)?;
        if n == 0 {
            return Ok(()); // EOF — client closed the pipe.
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Some(resp) = handle_message(trimmed) {
            stdout.write_all(resp.as_bytes())?;
            stdout.write_all(b"\n")?;
            stdout.flush()?;
        }
    }
}

/// Top-level dispatch. Returns the response JSON to write, or `None` when
/// the inbound message was a notification (no id, no response expected).
fn handle_message(line: &str) -> Option<String> {
    let req: serde_json::Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(e) => {
            return Some(error_response(
                serde_json::Value::Null,
                -32700,
                &format!("parse error: {e}"),
            ));
        }
    };
    let id = req.get("id").cloned().unwrap_or(serde_json::Value::Null);
    let is_notification = req.get("id").is_none();
    let method = match req.get("method").and_then(|m| m.as_str()) {
        Some(m) => m,
        None => return Some(error_response(id, -32600, "missing method")),
    };
    let params = req
        .get("params")
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));

    // Notifications (e.g. `notifications/initialized`) never get a reply
    // even on error — per JSON-RPC 2.0.
    if is_notification {
        return None;
    }

    let result = match method {
        "initialize" => Ok(handle_initialize(&params)),
        "tools/list" => Ok(handle_tools_list()),
        "tools/call" => handle_tools_call(params),
        // `ping` is a common MCP keepalive; reply with empty result.
        "ping" => Ok(serde_json::json!({})),
        _ => Err(format!("unknown method: {method}")),
    };
    match result {
        Ok(v) => Some(success_response(id, v)),
        Err(e) => Some(error_response(id, -32603, &e)),
    }
}

fn success_response(id: serde_json::Value, result: serde_json::Value) -> String {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result,
    })
    .to_string()
}

fn error_response(id: serde_json::Value, code: i32, message: &str) -> String {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message },
    })
    .to_string()
}

fn handle_initialize(_params: &serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "protocolVersion": PROTOCOL_VERSION,
        "capabilities": { "tools": {} },
        "serverInfo": { "name": SERVER_NAME, "version": SERVER_VERSION },
    })
}

fn handle_tools_list() -> serde_json::Value {
    serde_json::json!({
        "tools": [
            split_dry_run_descriptor(),
            split_write_descriptor(),
            combine_dry_run_descriptor(),
            combine_write_descriptor(),
            consolidate_dry_run_descriptor(),
            consolidate_write_descriptor(),
            flatten_dry_run_descriptor(),
            flatten_write_descriptor(),
        ],
    })
}

fn split_dry_run_descriptor() -> serde_json::Value {
    serde_json::json!({
        "name": "split_dry_run",
        "description": "Analyze a large .rs file and return r2factor's proposed split: which items go into which sub-modules, the rationale per item, and a cohesion score (intra- vs cross-bucket refs). Non-destructive — no files are touched. Use this first to preview, then call `split_write` to commit.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "file": {
                    "type": "string",
                    "description": "Absolute or cwd-relative path to a .rs file. lib.rs and mod.rs are supported; main.rs is not yet supported.",
                },
                "use_tokensave": {
                    "type": "boolean",
                    "description": "If true and a .tokensave/ database is found in an ancestor directory, fold in cross-symbol edges for better clustering. Defaults to true.",
                },
            },
            "required": ["file"],
        },
    })
}

fn split_write_descriptor() -> serde_json::Value {
    serde_json::json!({
        "name": "split_write",
        "description": "DESTRUCTIVE: replace the input .rs file with a facade module and write the proposed sub-modules. Normal files use a sibling directory, lib.rs uses lib/, and mod.rs writes beside the facade. The original is backed up to <file>.bak. Run `split_dry_run` first to preview.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "file": { "type": "string" },
                "force": {
                    "type": "boolean",
                    "description": "Overwrite an existing target directory and purge its stale .rs files. Use with care. Defaults to false.",
                },
                "use_tokensave": { "type": "boolean" },
                "max_lines": {
                    "type": "integer",
                    "description": "Recursively split generated files above this many lines. Defaults to 1000; use 0 to disable.",
                },
            },
            "required": ["file"],
        },
    })
}

fn handle_tools_call(params: serde_json::Value) -> Result<serde_json::Value, String> {
    let name = params
        .get("name")
        .and_then(|n| n.as_str())
        .ok_or("missing tool name")?;
    let arguments = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));

    let outcome = match name {
        "split_dry_run" => tool_split_dry_run(&arguments),
        "split_write" => tool_split_write(&arguments),
        "combine_dry_run" => tool_combine_dry_run(&arguments),
        "combine_write" => tool_combine_write(&arguments),
        "consolidate_dry_run" => tool_consolidate_dry_run(&arguments),
        "consolidate_write" => tool_consolidate_write(&arguments),
        "flatten_dry_run" => tool_flatten_dry_run(&arguments),
        "flatten_write" => tool_flatten_write(&arguments),
        _ => return Err(format!("unknown tool: {name}")),
    };
    // MCP convention: a *tool* error is data (isError: true in the result),
    // not a JSON-RPC error. Protocol-level errors (bad method name, etc.)
    // do bubble up as JSON-RPC errors.
    let content = match outcome {
        Ok(text) => serde_json::json!({
            "content": [{ "type": "text", "text": text }],
        }),
        Err(e) => serde_json::json!({
            "content": [{ "type": "text", "text": format!("error: {e}") }],
            "isError": true,
        }),
    };
    Ok(content)
}

fn combine_dry_run_descriptor() -> serde_json::Value {
    serde_json::json!({
        "name": "combine_dry_run",
        "description": "Non-destructive. Analyze two peer .rs files and return the proposed combine plan: new parent module directory, facade content with mod declarations and re-exports, path rewrites, and parent module updates.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "file1": { "type": "string", "description": "Path to first .rs file" },
                "file2": { "type": "string", "description": "Path to second .rs file" },
                "name": { "type": "string", "description": "Name for the new parent module. Defaults to stem of file1." },
                "json": { "type": "boolean", "description": "Return structured JSON instead of human text." },
                "preview_impacts": { "type": "boolean", "description": "Include consumer impact report (requires tokensave)." },
                "use_tokensave": { "type": "boolean", "description": "Allow tokensave discovery. Defaults to true." },
                "re_export_filter": { "type": "string", "description": "Regex filter for re-exports." },
            },
            "required": ["file1", "file2"],
        },
    })
}

fn combine_write_descriptor() -> serde_json::Value {
    serde_json::json!({
        "name": "combine_write",
        "description": "DESTRUCTIVE: combine two peer .rs files into a new parent module. Backs up originals to .bak, creates facade, moves files, updates parent module declaration.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "file1": { "type": "string" },
                "file2": { "type": "string" },
                "name": { "type": "string" },
                "force": { "type": "boolean", "description": "Overwrite existing target directory." },
                "use_tokensave": { "type": "boolean" },
                "re_export_filter": { "type": "string" },
            },
            "required": ["file1", "file2"],
        },
    })
}

fn tool_combine_dry_run(args: &serde_json::Value) -> Result<String, String> {
    let file1 = require_string(args, "file1")?;
    let file2 = require_string(args, "file2")?;
    let name = args.get("name").and_then(|v| v.as_str()).map(|s| s.to_string());
    let json = args.get("json").and_then(|b| b.as_bool()).unwrap_or(false);
    let preview_impacts = args.get("preview_impacts").and_then(|b| b.as_bool()).unwrap_or(false);
    let use_tokensave = args.get("use_tokensave").and_then(|b| b.as_bool()).unwrap_or(true);
    let re_export_filter = args.get("re_export_filter").and_then(|v| v.as_str()).map(|s| s.to_string());
    let opts = crate::combine::CombineOptions {
        module_name: name,
        write: false,
        force: false,
        json,
        preview_impacts,
        use_tokensave,
        re_export_filter,
    };
    crate::combine::combine_dry_run(Path::new(file1), Path::new(file2), &opts)
        .map_err(|e| format!("combine dry-run: {e}"))
}

fn tool_combine_write(args: &serde_json::Value) -> Result<String, String> {
    let file1 = require_string(args, "file1")?;
    let file2 = require_string(args, "file2")?;
    let name = args.get("name").and_then(|v| v.as_str()).map(|s| s.to_string());
    let force = args.get("force").and_then(|b| b.as_bool()).unwrap_or(false);
    let use_tokensave = args.get("use_tokensave").and_then(|b| b.as_bool()).unwrap_or(true);
    let re_export_filter = args.get("re_export_filter").and_then(|v| v.as_str()).map(|s| s.to_string());
    let opts = crate::combine::CombineOptions {
        module_name: name,
        write: true,
        force,
        json: false,
        preview_impacts: false,
        use_tokensave,
        re_export_filter,
    };
    let report = crate::combine::combine_write(Path::new(file1), Path::new(file2), &opts)
        .map_err(|e| format!("combine write: {e}"))?;
    serde_json::to_string_pretty(&report).map_err(|e| e.to_string())
}

fn consolidate_dry_run_descriptor() -> serde_json::Value {
    serde_json::json!({
        "name": "consolidate_dry_run",
        "description": "Inverse of `split_dry_run`. Given a facade file (`foo.rs` next to `foo/`, or `foo/mod.rs`) or a directory containing the sub-buckets, return the merged single-file source as text. Non-destructive — no files are touched.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the facade file or the sub-directory.",
                },
            },
            "required": ["path"],
        },
    })
}

fn consolidate_write_descriptor() -> serde_json::Value {
    serde_json::json!({
        "name": "consolidate_write",
        "description": "DESTRUCTIVE: produce the merged single-file content AND write it to disk. Backs up the facade to <facade>.bak, writes the merged content to the resolved target path, and deletes the sub-directory. Run `consolidate_dry_run` first to preview.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "path": { "type": "string" },
            },
            "required": ["path"],
        },
    })
}

fn flatten_dry_run_descriptor() -> serde_json::Value {
    serde_json::json!({
        "name": "flatten_dry_run",
        "description": "Post-pass for `consolidate`: given a single .rs file with top-level inline `mod bucket { ... }` blocks, return flattened source that drops the wrappers and renames named items to `bucket_name`. Non-destructive — no files are touched.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "file": {
                    "type": "string",
                    "description": "Path to the consolidated .rs file containing inline modules.",
                },
            },
            "required": ["file"],
        },
    })
}

fn flatten_write_descriptor() -> serde_json::Value {
    serde_json::json!({
        "name": "flatten_write",
        "description": "DESTRUCTIVE: flatten a consolidated .rs file in place. Backs up the original to <file>.bak, drops top-level inline module wrappers, and renames named items to `bucket_name`. This single-file mode does not rewrite other files.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "file": { "type": "string" },
            },
            "required": ["file"],
        },
    })
}

fn tool_consolidate_dry_run(args: &serde_json::Value) -> Result<String, String> {
    let path = require_string(args, "path")?;
    let merged = crate::consolidate::consolidate_dry_run(Path::new(path))
        .map_err(|e| format!("consolidate {path}: {e}"))?;
    Ok(merged)
}

fn tool_consolidate_write(args: &serde_json::Value) -> Result<String, String> {
    let path = require_string(args, "path")?;
    let opts = crate::consolidate::ConsolidateOptions { write: true };
    let report = crate::consolidate::consolidate_write(Path::new(path), &opts)
        .map_err(|e| format!("consolidate {path}: {e}"))?;
    serde_json::to_string_pretty(&report).map_err(|e| e.to_string())
}

fn tool_flatten_dry_run(args: &serde_json::Value) -> Result<String, String> {
    let file = require_string(args, "file")?;
    crate::flatten::flatten_dry_run(Path::new(file)).map_err(|e| format!("flatten {file}: {e}"))
}

fn tool_flatten_write(args: &serde_json::Value) -> Result<String, String> {
    let file = require_string(args, "file")?;
    let opts = crate::flatten::FlattenOptions { write: true };
    let report = crate::flatten::flatten_write(Path::new(file), &opts)
        .map_err(|e| format!("flatten {file}: {e}"))?;
    serde_json::to_string_pretty(&report).map_err(|e| e.to_string())
}

fn require_string<'a>(args: &'a serde_json::Value, key: &str) -> Result<&'a str, String> {
    args.get(key)
        .and_then(|v| v.as_str())
        .ok_or_else(|| format!("missing required argument `{key}`"))
}

fn tool_split_dry_run(args: &serde_json::Value) -> Result<String, String> {
    let file = require_string(args, "file")?;
    let use_tokensave = args
        .get("use_tokensave")
        .and_then(|b| b.as_bool())
        .unwrap_or(true);
    let path = Path::new(file);
    let src = std::fs::read_to_string(path).map_err(|e| format!("read {file}: {e}"))?;
    if crate::write::is_r2factor_facade(&src) {
        return Err(format!(
            "{file} is already an r2factor facade — won't analyze it"
        ));
    }
    let mut items = crate::item::parse_file(&src).map_err(|e| format!("parse {file}: {e}"))?;
    let evidence = if use_tokensave {
        load_tokensave_evidence(path, &items)
    } else {
        None
    };
    crate::graph::annotate_refs(&mut items, evidence.as_ref());
    let plan = crate::plan::build(&items);
    let dry = crate::plan::dry_run_report(&plan, &items);
    let cohesion = crate::plan::cohesion_report(&plan, &items);
    let payload = serde_json::json!({
        "plan": dry,
        "cohesion": cohesion,
    });
    serde_json::to_string_pretty(&payload).map_err(|e| e.to_string())
}

fn tool_split_write(args: &serde_json::Value) -> Result<String, String> {
    let file = require_string(args, "file")?;
    let force = args.get("force").and_then(|b| b.as_bool()).unwrap_or(false);
    let use_tokensave = args
        .get("use_tokensave")
        .and_then(|b| b.as_bool())
        .unwrap_or(true);
    let max_lines = args
        .get("max_lines")
        .and_then(|n| n.as_u64())
        .map(|n| n as usize)
        .unwrap_or(1000);

    let path = Path::new(file);
    let src = std::fs::read_to_string(path).map_err(|e| format!("read {file}: {e}"))?;
    if crate::write::is_r2factor_facade(&src) {
        return Err(format!(
            "{file} is already an r2factor facade — refusing to re-split"
        ));
    }
    let mut items = crate::item::parse_file(&src).map_err(|e| format!("parse {file}: {e}"))?;
    let evidence = if use_tokensave {
        load_tokensave_evidence(path, &items)
    } else {
        None
    };
    crate::graph::annotate_refs(&mut items, evidence.as_ref());
    let plan = crate::plan::build(&items);
    let opts = crate::write::WriteOptions {
        force,
        recursive_max_lines: Some(max_lines),
    };
    let report =
        crate::write::write_plan(path, &plan, &items, &opts).map_err(|e| format!("write: {e}"))?;
    if max_lines > 0 {
        for path in &report.written_files {
            let src = std::fs::read_to_string(path)
                .map_err(|e| format!("read generated {}: {e}", path.display()))?;
            if src.lines().count() > max_lines {
                crate::pipeline::run_split(
                    path,
                    crate::SplitOptions {
                        use_tokensave,
                        llm: None,
                        write: Some(opts),
                    },
                )
                .map_err(|e| format!("recursive split {}: {e}", path.display()))?;
            }
        }
    }
    serde_json::to_string_pretty(&report).map_err(|e| e.to_string())
}

/// Best-effort tokensave evidence load. Failures fall back to `None` —
/// the splitter degrades gracefully and the MCP caller doesn't need to
/// care whether a `.tokensave/` was available.
fn load_tokensave_evidence(
    path: &Path,
    items: &[crate::item::ParsedItem],
) -> Option<crate::tokensave::CrossFileEvidence> {
    use crate::tokensave::Tokensave;
    let root = Tokensave::locate(path)?;
    let ts = match Tokensave::open(&root) {
        Ok(ts) => ts,
        Err(e) => {
            eprintln!("[r2factor-mcp] tokensave open failed: {e}");
            return None;
        }
    };
    match ts.evidence_for_file(path, items) {
        Ok(ev) => Some(ev),
        Err(e) => {
            eprintln!("[r2factor-mcp] tokensave evidence failed: {e}");
            None
        }
    }
}
