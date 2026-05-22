# Data Model: Combine Files into Module

**Date**: 2026-05-21
**Feature**: specs/001-combine-files-module/spec.md

## Entities

### SourceFile

An input `.rs` file parsed into an AST representation.

| Field | Type | Description |
|-------|------|-------------|
| `path` | `PathBuf` | Absolute path to the original file (e.g., `src/parser.rs`) |
| `ast` | `syn::File` | Parsed AST of the file |
| `items` | `Vec<ParsedItem>` | Top-level items extracted from the AST |
| `public_items` | `Vec<(Ident, ItemKind)>` | Public items eligible for re-export in the facade |
| `submodule_name` | `Ident` | Name used for the `mod` declaration (e.g., `parser`) |

**Validation rules**:
- File MUST have `.rs` extension
- File MUST NOT be `lib.rs` or `main.rs` (FR-012)
- File MUST be in the same parent directory as the other input

### CombinedModule

The generated parent module directory and its contents.

| Field | Type | Description |
|-------|------|-------------|
| `name` | `Ident` | Module name (e.g., `front_end`) |
| `directory` | `PathBuf` | Absolute path to the new directory (e.g., `src/front_end/`) |
| `facade_path` | `PathBuf` | Path to the generated facade file (`mod.rs` or `<name>.rs`) |
| `facade_ast` | `syn::File` | Generated facade AST |
| `moved_files` | `Vec<SourceFile>` | The two input files with rewritten paths and ASTs |
| `parent_module_path` | `Option<PathBuf>` | Path to the parent `lib.rs` or `mod.rs` that will be updated |

**Validation rules**:
- Directory MUST NOT already exist unless `--force` is used (FR-009)
- Facade MUST declare both submodules as `mod <name>;`
- Facade MUST re-export all public items (with collision aliases per FR-014)

### Facade

The generated module declaration file.

| Field | Type | Description |
|-------|------|-------------|
| `mod_decls` | `Vec<syn::ItemMod>` | `mod parser;` and `mod lexer;` declarations |
| `re_exports` | `Vec<syn::ItemUse>` | `pub use parser::Item as alias;` statements |
| `collision_map` | `HashMap<Ident, Vec<Ident>>` | Tracks which item names collide across submodules |

**Validation rules**:
- Re-exports MUST preserve original visibility (only `pub` items are re-exported)
- Colliding names MUST use `as <submodule>_<name>` alias (FR-014)
- Output MUST be syntactically valid Rust

### ImpactReport

Optional report of affected consumer files.

| Field | Type | Description |
|-------|------|-------------|
| `old_paths` | `Vec<PathBuf>` | Original file paths before combine |
| `new_module_path` | `String` | New module path (e.g., `crate::front_end`) |
| `affected_consumers` | `Vec<ConsumerImpact>` | Files referencing the old paths |

### ConsumerImpact

A single affected consumer file entry.

| Field | Type | Description |
|-------|------|-------------|
| `file_path` | `PathBuf` | Path to the consumer file |
| `old_use` | `String` | Original `use` statement (e.g., `crate::parser::parse`) |
| `new_use` | `String` | Suggested replacement (e.g., `crate::front_end::parser::parse`) |
| `line_number` | `usize` | Line where the reference occurs |

## Relationships

```text
CombinedModule --contains--> Facade
CombinedModule --contains--> SourceFile (moved, 2 instances)
CombinedModule --updates--> ParentModule (optional, 1 instance)
ImpactReport --references--> CombinedModule
ImpactReport --contains--> ConsumerImpact (0..* instances)
```

## State Transitions

```
[Idle] --parse inputs--> [Parsed]
[Parsed] --generate plan--> [Planned]
[Planned] --dry-run output--> [Previewed]
[Previewed] --write + backups--> [Written]
[Written] --verify compile--> [Validated]
```

- `Planned` → `Previewed` is idempotent (no file changes)
- `Previewed` → `Written` requires explicit `--write`
- `Written` → `Validated` requires `cargo check` passing
