# CLI Contract: `r2factor combine`

**Date**: 2026-05-21
**Feature**: Combine Files into Module

## Command Signature

```
r2factor combine <file1> <file2> [OPTIONS]
```

## Positional Arguments

| Argument | Type | Required | Description |
|----------|------|----------|-------------|
| `file1` | `PathBuf` | yes | First `.rs` file to combine |
| `file2` | `PathBuf` | yes | Second `.rs` file to combine |

## Options

| Flag | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| `--name` | `String` | no | stem of `file1` | Name for the new parent module directory |
| `--write` | `bool` | no | `false` | Perform the combine (default is dry-run) |
| `--force` | `bool` | no | `false` | Overwrite existing target directory |
| `--json` | `bool` | no | `false` | Output dry-run plan as JSON |
| `--preview-impacts` | `bool` | no | `false` | Show consumer impact report (requires tokensave) |
| `--no-tokensave` | `bool` | no | `false` | Skip tokensave discovery even if available |

## Exit Codes

| Code | Meaning |
|------|---------|
| `0` | Success (dry-run completed or write completed) |
| `1` | General error (parsing failure, IO error) |
| `2` | Invalid input (not `.rs`, `lib.rs`/`main.rs`, files not peers) |
| `3` | Target already exists (without `--force`) |
| `4` | Tokensave unavailable for `--preview-impacts` |

## Dry-Run Output (Human-Readable)

```
r2factor combine — 2 files into 1 module
== front_end/mod.rs (facade) ==
  mod parser;
  mod lexer;
  pub use parser::parse;
  pub use lexer::tokenize;

[move] src/parser.rs -> src/front_end/parser.rs
[move] src/lexer.rs -> src/front_end/lexer.rs
[update] src/lib.rs: add `mod front_end;`, remove `mod parser;`, remove `mod lexer;`
[backup] src/parser.rs.bak
[backup] src/lexer.rs.bak
[backup] src/lib.rs.bak
```

## Dry-Run Output (JSON)

```json
{
  "module_name": "front_end",
  "facade_path": "src/front_end/mod.rs",
  "facade_content": "mod parser;\nmod lexer;\n...",
  "moved_files": [
    { "from": "src/parser.rs", "to": "src/front_end/parser.rs" },
    { "from": "src/lexer.rs", "to": "src/front_end/lexer.rs" }
  ],
  "parent_update": {
    "path": "src/lib.rs",
    "add": "mod front_end;",
    "remove": ["mod parser;", "mod lexer;"]
  },
  "backups": [
    "src/parser.rs.bak",
    "src/lexer.rs.bak",
    "src/lib.rs.bak"
  ]
}
```
