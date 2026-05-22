# Quickstart: Combine Files into Module

**Feature**: specs/001-combine-files-module/spec.md

## Prerequisites

- r2factor built from this branch
- Two peer `.rs` files in the same directory
- A compilable Rust workspace

## Dry Run (Default)

Preview the combine without modifying any files:

```bash
r2factor combine src/parser.rs src/lexer.rs --name front_end
```

Output shows the proposed facade, file moves, and parent module updates.

## Execute Combine

Perform the combine with `--write`:

```bash
r2factor combine src/parser.rs src/lexer.rs --name front_end --write
```

This creates:
- `src/front_end/mod.rs` — facade with `mod parser; mod lexer;` and re-exports
- `src/front_end/parser.rs` — moved and path-rewritten
- `src/front_end/lexer.rs` — moved and path-rewritten
- Updates `src/lib.rs` (or nearest parent `mod.rs`) to declare `mod front_end;`
- Backs up originals to `.bak` files

## Verify

```bash
cargo check
```

The workspace should compile without manual edits.

## JSON Output

For programmatic consumption or CI integration:

```bash
r2factor combine src/parser.rs src/lexer.rs --name front_end --json
```

## Preview Consumer Impacts

If a `.tokensave/` index is available in an ancestor directory:

```bash
r2factor combine src/parser.rs src/lexer.rs --name front_end --preview-impacts
```

This lists affected `use` statements in other files.

## Overwrite Existing Target

```bash
r2factor combine src/parser.rs src/lexer.rs --name front_end --write --force
```

⚠️ **Warning**: `--force` overwrites the target directory and purges its `.rs` files.
