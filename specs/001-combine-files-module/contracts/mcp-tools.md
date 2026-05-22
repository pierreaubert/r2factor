# MCP Tools Contract: Combine

**Date**: 2026-05-21
**Feature**: Combine Files into Module

## Tools Added

### `combine_dry_run`

Non-destructive. Returns the proposed combine plan.

**Input**

| field | type | required | default | meaning |
|-------|------|----------|---------|---------|
| `file1` | `string` | yes | — | Absolute or relative path to first `.rs` file |
| `file2` | `string` | yes | — | Absolute or relative path to second `.rs` file |
| `name` | `string` | no | stem of `file1` | Name for the new parent module |
| `json` | `bool` | no | `false` | Return structured JSON plan instead of human text |
| `preview_impacts` | `bool` | no | `false` | Include consumer impact report (requires tokensave) |
| `use_tokensave` | `bool` | no | `true` | Allow tokensave discovery for impact report |

**Returns** (as text content of the tool result)

Human-readable plan (default) or JSON object matching the CLI JSON schema.

### `combine_write`

Destructive. Performs the combine.

**Input**

| field | type | required | default | meaning |
|-------|------|----------|---------|---------|
| `file1` | `string` | yes | — | Same as `combine_dry_run` |
| `file2` | `string` | yes | — | Same as `combine_dry_run` |
| `name` | `string` | no | stem of `file1` | Same as `combine_dry_run` |
| `force` | `bool` | no | `false` | Overwrite existing target directory |
| `use_tokensave` | `bool` | no | `true` | Same as `combine_dry_run` |

**Returns** (JSON)

```json
{
  "module_name": "front_end",
  "facade_path": "src/front_end/mod.rs",
  "moved_files": [
    { "from": "src/parser.rs", "to": "src/front_end/parser.rs" }
  ],
  "parent_update": {
    "path": "src/lib.rs",
    "add": "mod front_end;",
    "remove": ["mod parser;"]
  },
  "backups": [
    "src/parser.rs.bak",
    "src/lexer.rs.bak",
    "src/lib.rs.bak"
  ]
}
```

## Error Responses

Both tools return error objects via MCP on failure:

```json
{
  "error": {
    "code": "INVALID_INPUT",
    "message": "lib.rs cannot be combined into a submodule"
  }
}
```

Valid error codes:
- `INVALID_INPUT` — `lib.rs`/`main.rs`, non-peer files, non-`.rs` files
- `TARGET_EXISTS` — target directory exists without `force`
- `TOKENSAVE_UNAVAILABLE` — `preview_impacts` requested but no index found
- `IO_ERROR` — disk read/write failure
