# Feature Specification: Combine Files into Module

**Feature Branch**: `001-combine-files-module`

**Created**: 2026-05-21

**Status**: Draft

**Input**: User description: "combine 2 files into a new modules"

## User Scenarios & Testing *(mandatory)*

### User Story 1 - Combine Two Peer Files into a Parent Module (Priority: P1)

A developer has two related `.rs` files in the same directory (e.g., `parser.rs` and `lexer.rs`) that have grown tightly coupled. They want to create a new parent module (e.g., `front_end/`) containing both files as submodules, with a generated facade that declares and re-exports public items so the items remain accessible from the new module path.

**Why this priority**: This is the core value proposition of the feature — enabling structural refactoring without breaking downstream consumers.

**Independent Test**: Run the combine dry-run on two peer fixture files, verify the proposed directory structure, facade content, and adjusted paths. Then run with `--write` and verify the resulting workspace compiles with `cargo check`.

**Acceptance Scenarios**:

1. **Given** `src/parser.rs` and `src/lexer.rs` exist with public items, **When** the developer runs `r2factor combine src/parser.rs src/lexer.rs --name front_end --write`, **Then** `src/front_end/mod.rs` is created, both files are moved to `src/front_end/parser.rs` and `src/front_end/lexer.rs`, and `cargo check` passes without manual edits.
2. **Given** the same inputs without `--write`, **When** the developer runs the dry-run, **Then** the exact proposed facade content, file moves, and path rewrites are printed to stdout and no files are modified.
3. **Given** `src/front_end/` already exists, **When** the developer runs combine without `--force`, **Then** the operation is refused with a clear error message.

---

### User Story 2 - Preview Combine Impact on Crate Consumers (Priority: P2)

Before restructuring, the developer wants to understand how the new module path will affect existing `use` statements and cross-file references in the rest of the workspace. This report is only available when a tokensave cross-symbol index is present.

**Why this priority**: Reduces the risk of breaking downstream code during refactoring. Less critical than the core combine operation but highly valuable for confidence.

**Independent Test**: Run combine dry-run with `--preview-impacts` on a workspace with a tokensave index, and verify the report lists affected `use` statements and suggested rewrites.

**Acceptance Scenarios**:

1. **Given** `crate::parser::parse` is used in three other files and a tokensave index is available, **When** combine dry-run runs with `--preview-impacts`, **Then** the impact report lists the three files and the required path change.

---

### User Story 3 - Control Facade Visibility and Re-exports (Priority: P3)

The developer wants fine-grained control over which items appear at the parent module level (e.g., re-export only `parse` and `tokenize`, not internal helpers).

**Why this priority**: Enables cleaner public APIs after refactoring. Can be deferred to a later iteration.

**Independent Test**: Run combine with `--re-export-filter` (or equivalent) and verify only matching items are re-exported in the facade.

**Acceptance Scenarios**:

1. **Given** a filter pattern `parse|tokenize`, **When** combine generates the facade, **Then** only items matching the pattern are re-exported at `front_end::` scope.

---

### Edge Cases

- What happens when one or both files contain `mod` declarations referencing other files in the original directory?
- When both files contain public items with the same name, the facade MUST re-export them with aliases prefixed by the source submodule name (e.g., `pub use parser::helper as parser_helper;`).
- What happens if the two files have circular `use` dependencies on each other?
- How does the system handle files that are referenced by `#[path = "..."]` attributes?
- The tool MUST refuse to process `lib.rs` or `main.rs` as input files, since they are crate entry points and cannot become submodules.

## Requirements *(mandatory)*

### Functional Requirements

- **FR-001**: System MUST accept exactly two `.rs` file paths as input arguments.
- **FR-002**: System MUST generate a new directory (module) with the name provided via `--name` or derived from the target path.
- **FR-003**: System MUST create a facade file (`mod.rs` or `<name>.rs` depending on context) declaring both moved files as submodules.
- **FR-004**: System MUST move both input files into the new directory and rename them appropriately.
- **FR-005**: System MUST rewrite relative paths and `use` statements inside the moved files so they resolve correctly from their new location.
- **FR-006**: System MUST generate `pub use` re-export statements in the facade for all public items from both submodules, preserving visibility.
- **FR-007**: System MUST default to dry-run mode; destructive changes require `--write`.
- **FR-008**: System MUST create timestamped backups of original files before any overwrite when `--write` is used.
- **FR-009**: System MUST refuse to overwrite existing files or directories without `--force`.
- **FR-010**: System MUST produce compilable output without manual fixup for standard Rust module layouts.
- **FR-011**: System MUST update the nearest parent module declaration (`lib.rs` or enclosing `mod.rs`) to add `mod <new_name>;` and remove any `mod <old_name>;` declarations for the moved files.
- **FR-012**: System MUST refuse to process `lib.rs` or `main.rs` as input files with a clear error message.
- **FR-013**: System MUST produce human-readable dry-run output by default, and support structured JSON output when `--json` is provided.
- **FR-014**: System MUST detect public item name collisions between the two source files. For each collision, the facade MUST re-export the item with an alias prefixed by the source submodule name (e.g., `pub use parser::helper as parser_helper;`).
- **FR-015**: System MUST support an optional `--preview-impacts` flag that produces a consumer impact report when a tokensave cross-symbol index is available. If tokensave is unavailable, the flag MUST produce a clear message indicating the report requires a tokensave index.

### Key Entities

- **SourceFile**: An input `.rs` file containing items, visibility modifiers, and `use` statements.
- **CombinedModule**: The generated parent module directory containing the facade and both submodules.
- **Facade**: The generated `mod.rs` or `<name>.rs` file that declares submodules and re-exports public items.
- **ImpactReport**: A dry-run artifact listing affected consumer files and suggested path rewrites.

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001**: Two peer files can be combined into a compilable parent module in under 5 seconds for files up to 500 lines each.
- **SC-002**: All public items from both source files are reachable via the new parent module path.
- **SC-003**: Zero manual path or visibility adjustments are needed after combine for standard Rust projects (no `#[path]`, no complex `cfg` gates).
- **SC-004**: Developers can preview the full combine plan (file moves, facade content, path rewrites) before any file is modified.
- **SC-005**: Backup files are created for every original file that is moved or overwritten.

## Clarifications

### Session 2026-05-21

- Q: Should the tool automatically update the nearest parent module to declare the new combined module and remove old declarations? → A: Yes — automatically update parent mod declarations and remove old ones.
- Q: How should the tool handle lib.rs or main.rs being passed as input? → A: Refuse the operation with a clear error — lib.rs and main.rs cannot be combined into submodules.
- Q: What output format should the combine dry-run produce? → A: Human-readable text by default, with an optional `--json` flag.
- Q: When both input files contain public items with the same name, how should the facade handle re-exports? → A: Auto-rename conflicting re-exports with a prefix based on the source submodule name.
- Q: Should the consumer impact report be included in v1, and what search scope should it use? → A: Include only when tokensave is available — rely on cross-symbol index for detection.

## Assumptions

- Input files are peer files in the same parent directory.
- The new module name is provided via `--name` or defaults to a sanitized version of the first input file's stem.
- Input files compile independently before the combine operation.
- The combine operation is intended for standard Rust module layouts; exotic `#[path]` attributes or complex `cfg` conditional modules may require manual follow-up.
- The tool operates at the file-system and AST level. It updates parent module declarations to reflect the new structure, but does not rewrite general consumer `use` statements in other files outside the combined module in v1.
- Consumer files may require import path updates after combining; the combine operation guarantees compilability of the new module structure itself.
