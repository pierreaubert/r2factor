# r2factor

A Rust CLI that splits oversized `.rs` source files into a facade + focused
submodules — and also runs the inverse (consolidating a multi-file module
back into a single file). Available as an
[MCP](https://modelcontextprotocol.io) server for use from Claude Code,
Claude Desktop, Cursor, Zed, and other MCP-aware tools.

---

## Build prerequisite

`Cargo.toml` currently declares two **local path-deps**:

```toml
tokensave = { path = "/Users/pierre/src/tokensave/tokensave" }
tokensave-large-treesitters = { path = "/Users/pierre/src/tokensave/tokensave-large-treesitters" }
```

A fresh clone will fail to `cargo build` unless those sibling repos are
checked out at the same absolute paths, or you patch the manifest. Until
the deps are switched to crates.io / git refs, plan on either:

1. Cloning [tokensave](https://github.com/aovestdipaperino/tokensave)
   next door and adjusting the paths, or
2. Commenting out the two `tokensave` lines and running with
   `--no-tokensave` (the splitter degrades gracefully without it).

---

## What it does

- Parses one large `.rs` file with [`syn`](https://crates.io/crates/syn),
  clusters its items by type anchors and call-graph proximity, and proposes
  a split into a facade module + sub-files.
- Rewrites visibility (`pub(super)`), rebases relative paths
  (`super::X` → `super::super::X` for items that move one level deeper),
  and lifts cross-bucket field/method access so **the output compiles
  without manual fixup**.
- Validated end-to-end on three real workspaces — a Rust Emacs Lisp
  editor (`rele`), an acoustics simulator (`sonium`), and Polkadot —
  totalling 29/29 testable files that compile after split.

Before:

```
crates/foo/src/big.rs   (4113 lines, 80 items)
```

After `r2factor split big.rs --write`:

```
crates/foo/src/big.rs            (facade — mod decls + pub-use chain)
crates/foo/src/big/types.rs      (data types)
crates/foo/src/big/error.rs      (error type + its impls)
crates/foo/src/big/parser.rs     (Parser + impl)
crates/foo/src/big/eval.rs       (eval_* fn family)
crates/foo/src/big/macros.rs     (macro_rules!)
crates/foo/src/big/tests.rs      (#[cfg(test)] items)
crates/foo/src/big.rs.bak        (timestamped original)
```

`lib.rs` and `mod.rs` are supported with Rust-aware layouts. Splitting
`lib.rs` keeps the facade at `lib.rs`, writes generated files under
`lib/`, and emits `#[path = "lib/<bucket>.rs"] mod <bucket>;` so child
modules stay at `crate::<bucket>`. Splitting `foo/mod.rs` keeps the
facade in place and writes generated files beside it under `foo/`.

---

## Install

```bash
git clone <repo-url> r2factor
cd r2factor
cargo build --release
# Binary at ./target/release/r2factor
# (optional) cp ./target/release/r2factor ~/.local/bin/
```

See [Build prerequisite](#build-prerequisite) above if cargo errors on
the `tokensave` path-deps.

---

## CLI usage

Eight subcommands: `split` (the original), `combine` (group peer files
under a new parent module), `combine-suggest` (find good combine candidates),
`consolidate` (the inverse of split), `flatten` (an optional post-pass after
consolidation), `check` (local readiness diagnostics), `backups` (inspect
`.bak` files), and `restore` (restore one backup).

### `split`

```
r2factor split <file> [--write] [--force] [--no-tokensave] [--llm ...]
```

| flag | what it does |
|---|---|
| (none) | Dry-run. Prints the proposed split + cohesion report to stdout. No files touched. |
| `--write` | Materialize the split. Writes the facade + sub-files. Creates `<file>.bak`. |
| `--force` | Required when generated bucket files would overwrite existing files; also purges stale generated `.rs` files in the split target. |
| `--no-tokensave` | Skip the tokensave cross-symbol index even if a `.tokensave/` is found in an ancestor directory. |
| `--llm` | Run an LLM advisor pass over the deterministic plan (renames misc-bucket names, moves obvious misplacements). |
| `--llm-endpoint <url>` | OpenAI-compatible endpoint (default: local Ollama on `:11434`). |
| `--llm-model <name>` | Model name (default: `llama3.2:3b`). |
| `--llm-api-key <key>` | Bearer token for hosted endpoints. Falls back to `R2FACTOR_LLM_API_KEY` env var. |

Example — split the in-repo fixture:

```bash
$ r2factor split fixtures/sample.rs --write
r2factor split — 20 items across 10 proposed file(s)
== consts.rs  (3 items, ~3 lines) == ...
...
[write] backup -> fixtures/sample.rs.bak
[write] target -> fixtures/sample/
[write]   fixtures/sample/types.rs
[write]   fixtures/sample/eval.rs
...
[write] facade -> fixtures/sample.rs
```

The original is preserved at `fixtures/sample.rs.bak`. Re-running on the
facade is refused (a sentinel comment in the generated facade is the guard).

### `backups` / `restore`

```
r2factor backups [path] [--json]
r2factor restore <file.rs.bak> [--force] [--json]
```

`backups` recursively lists `.bak` files and their restore targets, skipping
`.git`, `.tokensave`, and `target`. `restore` copies one backup back to the
sibling path without deleting the backup; it refuses to overwrite an existing
target unless `--force` is provided.

### `combine`

```
r2factor combine <file1.rs> <file2.rs> [more.rs ...] [--name <module>] [--write] [--force]
```

Combines two or more peer Rust files into a new parent module directory. Dry-run mode
prints the generated facade, planned moves, parent-module update, and rewritten
source previews for the moved files. Write mode creates backups, writes only the
planned target files, preserves unrelated files in an existing target directory
when `--force` is used, and rolls back from backups if the operation fails.

| flag | what it does |
|---|---|
| `--json` | Return a structured dry-run report, including rewritten source previews, planned backups, and the write-mode manifest preview. |
| `--preview-impacts` | Use TokenSave unresolved-reference data to report consumer path updates. |
| `--preview-consumer-rewrites` | Dry-run the local source scanner used by `--rewrite-consumers` and include exact rewritten consumer source, line-level hunks, AST-backed import rewrites, and skipped safety candidates in JSON output. |
| `--re-export-filter <regex>` | Re-export only matching public items from the generated facade. |
| `--rewrite-consumers` | With `--write`, update consumer paths such as `crate::parser::parse` to `crate::front_end::parser::parse`, rewrite simple `use crate::{parser, lexer};` imports, and report skipped candidates that need review. |

### `combine-suggest`

```
r2factor combine-suggest [path] [--json] [--min-score <n>]
```

Ranks sibling `.rs` files that are likely useful `combine` inputs. The scorer
uses deterministic source references such as `crate::parser::parse`, adds a
small boost for shared stem prefixes, and reports whether a TokenSave index is
available for the inspected project.

### `check`

```
r2factor check [path] [--json]
```

Reports the detected Cargo root, TokenSave index availability/statistics, local
path dependencies, warnings such as a zero-edge TokenSave graph, and actionable
suggestions with stable IDs. JSON output includes a `suggestions` array with
optional commands, such as refreshing the TokenSave index.

### `consolidate`

```
r2factor consolidate <path> [--write]
```

Inverse of `split`. Takes a facade file (`foo.rs` next to `foo/`) or a
`foo/mod.rs` (or just the directory), and produces a single merged file
where each sub-file becomes an **inline `mod <name> { ... }` block**
inside the facade.

Inline mods (rather than flat-flattening to one scope) preserve:

- **Names** — two sub-files can each define `fn helper()` without
  conflicting; each lives in its own inline scope.
- **External paths** — `crate::foo::bar::thing` keeps working, since
  `bar` is still a sub-module of `foo`, just inlined.
- **Visibility/scope** — `pub(super)`, `pub(in super::super)`,
  `use super::other::name`, and every other relative path retain their
  original meaning because module depth is unchanged.

The merger also preserves attributes/visibility on the original `mod`
declarations: `#[cfg(test)] mod tests;` becomes `#[cfg(test)] mod tests
{ ... }`, `#[macro_use] mod macros;` becomes `#[macro_use] mod macros
{ ... }`, and `pub mod x;` stays `pub`.

```bash
# Dry-run: print merged content to stdout
r2factor consolidate path/to/foo.rs

# Replace in place: facade gets the merged content, old facade saved to
# `foo.rs.bak`, sub-dir `foo/` is deleted.
r2factor consolidate path/to/foo.rs --write
```

For `foo/mod.rs` input, the merged content lands at `<parent>/foo.rs`
and the `foo/` directory is deleted entirely.

### `flatten`

```
r2factor flatten <file> [--write]
```

Optional post-pass for a consolidated file. It takes a single `.rs` file
containing top-level inline modules, drops those module wrappers, and
renames each named item inside them to `<module>_<item>` so sibling names
do not collide.

```bash
# Dry-run: print flattened content to stdout
r2factor flatten path/to/foo.rs

# Replace in place, backing up the original to `foo.rs.bak`
r2factor flatten path/to/foo.rs --write
```

This first flatten mode is intentionally single-file only. It rewrites
declarations, intra-file paths, simple re-exports, and simple glob
re-exports in the merged file itself. It does not rewrite consumers in the
rest of the repo; a later cross-file mode can use tokensave for that.

---

## MCP server

The headline feature: r2factor speaks
[MCP](https://modelcontextprotocol.io) over stdio so an AI agent or any
MCP-aware tool can call it as a first-class action.

### What `r2factor mcp` is

A JSON-RPC 2.0 stdio server. **You don't launch it manually** — your MCP
client spawns it on demand. Twelve tools are exposed:

- `split_dry_run` — analyze a file, return the proposed plan + cohesion.
- `split_write` — actually perform the split and return a recursive write tree.
- `combine_dry_run` — preview combining two or more peer files.
- `combine_write` — perform combine, optionally rewriting consumers.
- `combine_suggest` — suggest peer files that are good combine candidates.
- `check` — report Cargo/TokenSave/local-path-dependency readiness.
- `backups_list` — list `.bak` files and restore targets.
- `backup_restore` — restore one `.bak` file, requiring `force` to overwrite.
- `consolidate_dry_run` — inverse: return the merged source as text.
- `consolidate_write` — inverse: replace the facade in place (destructive).
- `flatten_dry_run` — flatten a consolidated file into one scope as text.
- `flatten_write` — replace the consolidated file in place (destructive).

Stdout carries the protocol stream. Logs go to stderr so they don't
corrupt the JSON-RPC framing.

### Install — Claude Code (CLI)

```bash
claude mcp add r2factor -- /absolute/path/to/r2factor mcp
claude mcp list   # confirm "r2factor" shows up
```

Then in any Claude Code session the agent will see `split_dry_run` and
`split_write` listed under available tools.

### Install — Claude Desktop

Edit your `claude_desktop_config.json` and add an `mcpServers` entry:

```json
{
  "mcpServers": {
    "r2factor": {
      "command": "/absolute/path/to/r2factor",
      "args": ["mcp"]
    }
  }
}
```

Config file location per platform:

| OS | Path |
|---|---|
| macOS | `~/Library/Application Support/Claude/claude_desktop_config.json` |
| Linux | `~/.config/Claude/claude_desktop_config.json` |
| Windows | `%APPDATA%\Claude\claude_desktop_config.json` |

Restart Claude Desktop after editing.

### Install — Cursor

Edit `~/.cursor/mcp.json` (or use Cursor's MCP UI under Settings →
Features → Model Context Protocol):

```json
{
  "mcpServers": {
    "r2factor": {
      "command": "/absolute/path/to/r2factor",
      "args": ["mcp"]
    }
  }
}
```

### Install — Zed

In Zed's `settings.json`:

```json
{
  "context_servers": {
    "r2factor": {
      "command": {
        "path": "/absolute/path/to/r2factor",
        "args": ["mcp"]
      }
    }
  }
}
```

### Generic MCP config

Any MCP-aware client accepts the same shape: a command + args. If your
client expects per-server JSON, this is the canonical block:

```json
{
  "command": "/absolute/path/to/r2factor",
  "args": ["mcp"]
}
```

No environment variables required. The server is stateless — each tool
call is independent.

### Tools reference

#### `split_dry_run`

Non-destructive. Analyze a file and return the proposed plan.

**Input**

| field | type | required | default | meaning |
|---|---|---|---|---|
| `file` | string | yes | — | Absolute or cwd-relative path to a `.rs` file. `lib.rs` and `mod.rs` are supported; `main.rs` is not yet supported. |
| `use_tokensave` | bool | no | `true` | If a `.tokensave/` database is found in an ancestor directory, fold its cross-symbol edges into the clustering signal. |

**Returns** (as the text content of the tool result)

```json
{
  "plan": {
    "total_items": 20,
    "buckets": [
      {
        "name": "eval",
        "item_count": 3,
        "line_count": 19,
        "items": [
          {
            "id": 7,
            "kind": "fn",
            "name": "eval",
            "line_start": 76,
            "line_end": 83,
            "rationale": "fn name matches `eval` group"
          }
        ]
      }
    ]
  },
  "cohesion": {
    "intra": 7,
    "inter": 19,
    "score": 0.27,
    "top_cross_edges": [
      { "from": "eval", "to": "env",   "weight": 3 },
      { "from": "eval", "to": "error", "weight": 3 }
    ]
  }
}
```

The `cohesion.score` is `intra / (intra + inter)`. 1.0 = every reference
stays inside its bucket; lower = more cross-bucket coupling. `top_cross_edges`
is capped at 5 entries.

#### `split_write`

Destructive. Performs the split.

**Input**

| field | type | required | default | meaning |
|---|---|---|---|---|
| `file` | string | yes | — | Same as `split_dry_run`. |
| `force` | bool | no | `false` | Overwrite an existing target directory and purge its top-level `.rs` files. Use with care. |
| `use_tokensave` | bool | no | `true` | Same as `split_dry_run`. |
| `llm` | bool | no | `false` | Run the LLM advisor before writing. |
| `max_lines` | integer | no | `1000` | Recursively split generated files above this many lines; `0` disables recursion. |

**Returns**

```json
{
  "report": {
    "backup": "fixtures/sample.rs.bak",
    "target_dir": "fixtures/sample",
    "written_files": [
      "fixtures/sample/types.rs",
      "fixtures/sample/eval.rs"
    ],
    "facade": "fixtures/sample.rs"
  },
  "children": []
}
```

`target_dir` is `null` when every bucket ended up at facade scope and no
sub-files were needed.

#### `consolidate_dry_run`

Non-destructive. Returns the merged single-file source as a text payload.

**Input**

| field | type | required | meaning |
|---|---|---|---|
| `path` | string | yes | Path to the facade file (`foo.rs` or `foo/mod.rs`) or the sub-directory itself. |

**Returns** — the merged Rust source as the `text` content (not wrapped in
JSON; the agent gets the file content directly).

#### `consolidate_write`

Destructive. Performs the merge.

**Input**

| field | type | required | meaning |
|---|---|---|---|
| `path` | string | yes | Same as `consolidate_dry_run`. |

**Returns**

```json
{
  "merged_target": "path/to/foo.rs",
  "backup": "path/to/foo.rs.bak",
  "removed_files": [
    "path/to/foo/bar.rs",
    "path/to/foo/baz.rs"
  ],
  "source_bytes": 4321
}
```

For `foo/mod.rs` input, `merged_target` is at the parent level
(`<parent>/foo.rs`) and the old `foo/mod.rs` is included in
`removed_files`.

#### `flatten_dry_run`

Non-destructive. Returns flattened Rust source as a text payload.

**Input**

| field | type | required | meaning |
|---|---|---|---|
| `file` | string | yes | Path to the consolidated `.rs` file containing inline modules. |

#### `flatten_write`

Destructive. Performs the flatten pass in place.

**Input**

| field | type | required | meaning |
|---|---|---|---|
| `file` | string | yes | Same as `flatten_dry_run`. |

**Returns**

```json
{
  "target": "path/to/foo.rs",
  "backup": "path/to/foo.rs.bak",
  "rewrites": 12,
  "warnings": [],
  "source_bytes": 4321
}
```

### Verifying the server

You can drive it by hand to confirm everything's wired up:

```bash
echo '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}' \
  | /absolute/path/to/r2factor mcp
```

Expected output (one line):

```json
{"id":1,"jsonrpc":"2.0","result":{"capabilities":{"tools":{}},"protocolVersion":"2024-11-05","serverInfo":{"name":"r2factor","version":"0.1.0"}}}
```

To list the tools:

```bash
echo '{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}' \
  | /absolute/path/to/r2factor mcp
```

---

## Development

Repo layout:

```
src/
  main.rs        — CLI entry point
  lib.rs         — pub module wiring
  item.rs        — syn-based ParsedItem
  graph.rs       — RefVisitor (intra-file reference graph, incl. macro tokens)
  carve.rs       — heuristic carves: tests, macros, errors, consts, types
  cluster.rs     — type-anchor clustering for what remains
  refine.rs      — fixed-point pull-misc-by-calls
  promote.rs     — visibility lift / cross-imports / body-path rebase
  plan/          — Plan, dry-run + cohesion reports
  write/         — facade + sub-file renderers, marker guard, backup
  mcp.rs         — JSON-RPC over stdio
  health.rs      — readiness diagnostics
  llm.rs         — optional LLM advisor pass
  tokensave.rs   — optional tokensave cross-symbol evidence
  pipeline.rs    — run_split orchestrator
  consolidate.rs — inverse pipeline (merge back into one file)
  flatten.rs     — optional single-file flatten post-pass
tests/
  split_e2e.rs       — splitter on fixtures + rustc compile-check
  mcp_e2e.rs         — drives the MCP server over real stdio
  consolidate_e2e.rs — round-trip + hand-written-module merge tests
  flatten_e2e.rs     — single-file flatten post-pass tests
fixtures/
  sample.rs      — small demo file used by tests and CLI examples
```

Test suite (all green at time of writing):

```bash
cargo test                          # 68 unit + 5 split_e2e + 5 mcp_e2e + 5 consolidate_e2e
cargo test --test mcp_e2e           # just the MCP integration tests
cargo test --test split_e2e         # just the split integration tests
cargo test --test consolidate_e2e   # just the consolidate integration tests
cargo clippy --all-targets          # no warnings
```

---

## License

License is **TBD**. The repo doesn't carry a `LICENSE` file yet.
