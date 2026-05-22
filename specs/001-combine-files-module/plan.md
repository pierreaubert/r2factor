# Implementation Plan: Combine Files into Module

**Branch**: `001-combine-files-module` | **Date**: 2026-05-21 | **Spec**: [spec.md](./spec.md)

**Input**: Feature specification from `/specs/001-combine-files-module/spec.md`

**Note**: This template is filled in by the `/speckit-plan` command. See `.specify/templates/plan-template.md` for the execution workflow.

## Summary

Implement a `combine` subcommand for r2factor that takes two peer `.rs` files and
creates a new parent module directory containing them as submodules. The command
 generates a facade file with `mod` declarations and `pub use` re-exports, rewrites
relative paths inside the moved files, updates the nearest parent module
declaration (`lib.rs` or `mod.rs`), and supports an optional tokensave-based
consumer impact report. The feature follows the existing dry-run-by-default + `--write`
pattern established by `split`, `consolidate`, and `flatten`.

## Technical Context

**Language/Version**: Rust (latest stable, tracking `cargo` MSRV policy from constitution)

**Primary Dependencies**: `syn` (AST parsing), `quote` (code generation), `clap` (CLI args), `tokensave` (optional cross-symbol index for impact reports)

**Storage**: N/A — file-system operations only

**Testing**: `cargo test` (unit + E2E); E2E must validate on a real Rust workspace before merge per constitution Principle III

**Target Platform**: Cross-platform CLI (macOS, Linux, Windows); MCP server over stdio

**Project Type**: CLI tool / MCP server

**Performance Goals**: Combine two 500-line files in under 5 seconds (SC-001)

**Constraints**: Offline-capable; core functionality works without tokensave or LLM (Principle IV); zero manual fixup for standard layouts (Principle I)

**Scale/Scope**: Exactly two peer `.rs` files per invocation; standard Rust module layouts

## Constitution Check

*GATE: Must pass before Phase 0 research. Re-check after Phase 1 design.*

| Principle | Check | Status |
|-----------|-------|--------|
| I. Deterministic Correctness | Output MUST compile without manual fixup | ✅ Passing — FR-010 enforces this; path rewriting + parent declaration updates cover standard layouts |
| II. Safety by Default | Dry-run default, backups, `--force` required | ✅ Passing — FR-007, FR-008, FR-009 align |
| III. Test-First | E2E tests on real workspace required | ✅ Passing — acceptance scenarios require `cargo check` on fixtures + real workspace |
| IV. Graceful Degradation | Core works without optional integrations | ✅ Passing — tokensave impact report is optional (FR-015); combine works standalone |
| V. Observability | Exact dry-run output, stderr for logs | ✅ Passing — FR-013 defines human + JSON dry-run output; stdout reserved for results |

**Re-check after Phase 1**: No constitution violations identified. All gates pass.

## Project Structure

### Documentation (this feature)

```text
specs/001-combine-files-module/
├── plan.md              # This file
├── research.md          # Phase 0 output
├── data-model.md        # Phase 1 output
├── quickstart.md        # Phase 1 output
├── contracts/           # Phase 1 output
└── tasks.md             # Phase 2 output (speckit-tasks)
```

### Source Code (repository root)

```text
src/
  main.rs            — CLI entry point: add `combine` subcommand to clap
  lib.rs             — pub module wiring: add `pub mod combine;`
  combine.rs         — combine orchestrator (public API + CLI handler)
  combine/
    plan.rs          — dry-run plan generation (file moves, facade content, path rewrites)
    facade.rs        — facade file generation (mod declarations + pub use re-exports + collision aliases)
    rewrite.rs       — path rewriting inside moved files (use statements, relative paths)
    parent.rs        — parent module declaration updates (add mod <new>, remove mod <old>)
    impact.rs        — optional tokensave-based consumer impact report
    report.rs        — human-readable + JSON dry-run report formatting
  mcp.rs             — add `combine_dry_run` and `combine_write` MCP tools
  pipeline.rs        — add `run_combine` orchestrator call
```

**Structure Decision**: Single-project Rust CLI, extending existing `src/` layout. New `combine/` submodule mirrors the existing `plan/` and `write/` subdirectories used by `split`. `combine.rs` follows the pattern of `consolidate.rs` and `flatten.rs` as a top-level command module.

## Complexity Tracking

> **Fill ONLY if Constitution Check has violations that must be justified**

No constitution violations identified. Complexity tracking not required.
