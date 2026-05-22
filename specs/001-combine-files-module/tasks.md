# Tasks: Combine Files into Module

**Input**: Design documents from `/specs/001-combine-files-module/`

**Prerequisites**: plan.md (required), spec.md (required for user stories), research.md, data-model.md, contracts/

**Tests**: Tests are included per constitution Principle III (Test-First for Transformations). E2E tests MUST be written first and fail before implementation.

**Organization**: Tasks are grouped by user story to enable independent implementation and testing of each story.

## Format: `[ID] [P?] [Story] Description`

- **[P]**: Can run in parallel (different files, no dependencies)
- **[Story]**: Which user story this task belongs to (e.g., US1, US2, US3)
- Include exact file paths in descriptions

## Path Conventions

- **Single project**: `src/`, `tests/` at repository root

---

## Phase 1: Setup (Shared Infrastructure)

**Purpose**: Project initialization and basic structure

- [ ] T001 Register `Combine` clap subcommand with args (`file1`, `file2`, `--name`, `--write`, `--force`, `--json`) in `src/main.rs`
- [ ] T002 Add `pub mod combine;` to `src/lib.rs`
- [ ] T003 [P] Create `src/combine/` directory with `mod.rs`, `plan.rs`, `facade.rs`, `rewrite.rs`, `parent.rs`, `impact.rs`, `report.rs`, and `write.rs`

---

## Phase 2: Foundational (Blocking Prerequisites)

**Purpose**: Core infrastructure that MUST be complete before ANY user story can be implemented

**⚠️ CRITICAL**: No user story work can begin until this phase is complete

- [ ] T004 Implement input file parsing and `SourceFile` validation (`.rs` check, peer check, lib.rs/main.rs rejection) in `src/combine/plan.rs`
- [ ] T005 Implement directory and facade path resolution (`mod.rs` vs `<name>.rs` context) in `src/combine/plan.rs`
- [ ] T006 Implement facade `mod` declaration generation (`mod <name>;`) in `src/combine/facade.rs`
- [ ] T007 Implement public item extraction from parsed AST in `src/combine/facade.rs`
- [ ] T008 Implement `pub use` re-export generation with collision aliases (`as <submodule>_<name>`) in `src/combine/facade.rs`
- [ ] T009 [P] Implement path rewriting visitor (`use` statements, relative paths) for moved files in `src/combine/rewrite.rs`

**Checkpoint**: Foundation ready — plan generation, facade generation, and path rewriting are functional.

---

## Phase 3: User Story 1 - Combine Two Peer Files into a Parent Module (Priority: P1) 🎯 MVP

**Goal**: Core combine functionality — dry-run preview, `--write` execution with backups, parent module declaration updates, and JSON output.

**Independent Test**: Run `r2factor combine` on two peer fixture files, verify the dry-run shows correct facade and moves, then run with `--write` and verify `cargo check` passes without manual edits.

### Tests for User Story 1

> **NOTE: Write these tests FIRST, ensure they FAIL before implementation**

- [ ] T010 [US1] Add E2E dry-run test in `tests/combine_e2e.rs` — verify output shows facade, file moves, and parent updates without touching files
- [ ] T011 [US1] Add E2E `--write` test in `tests/combine_e2e.rs` — verify files are moved, facade is created, parent is updated, backups exist, and `cargo check` passes
- [ ] T012 [US1] Add E2E `--force` overwrite test in `tests/combine_e2e.rs` — verify existing target is purged and replaced

### Implementation for User Story 1

- [ ] T013 [US1] Implement combine orchestrator (`run_combine`) in `src/combine.rs` — coordinates parse, plan, dry-run, and write phases
- [ ] T014 [US1] Implement human-readable dry-run report formatting in `src/combine/report.rs`
- [ ] T015 [US1] Implement file move, backup, and directory creation logic in `src/combine/write.rs`
- [ ] T016 [US1] Implement parent module declaration update (add `mod <new>`, remove `mod <old>`) in `src/combine/parent.rs`
- [ ] T017 [US1] Implement JSON dry-run output format in `src/combine/report.rs`
- [ ] T018 [US1] Register `combine_dry_run` and `combine_write` MCP tools in `src/mcp.rs`

**Checkpoint**: At this point, User Story 1 should be fully functional and testable independently.

---

## Phase 4: User Story 2 - Preview Combine Impact on Crate Consumers (Priority: P2)

**Goal**: Optional tokensave-based consumer impact report showing affected `use` statements across the crate.

**Independent Test**: Run `r2factor combine --preview-impacts` on a workspace with a tokensave index, verify the report lists affected files and suggested path rewrites. Run without tokensave and verify a clear unavailable message is shown.

### Tests for User Story 2

> **NOTE: Write these tests FIRST, ensure they FAIL before implementation**

- [ ] T019 [US2] Add E2E impact report test with tokensave in `tests/combine_impact_e2e.rs` — verify report lists affected consumers and line numbers
- [ ] T020 [US2] Add E2E impact report unavailable test in `tests/combine_impact_e2e.rs` — verify graceful message when tokensave is missing

### Implementation for User Story 2

- [ ] T021 [P] [US2] Implement tokensave discovery and cross-symbol query in `src/combine/impact.rs`
- [ ] T022 [P] [US2] Implement consumer impact report generation (`ConsumerImpact` list) in `src/combine/impact.rs`
- [ ] T023 [US2] Add `--preview-impacts` CLI flag to combine subcommand in `src/main.rs`
- [ ] T024 [US2] Update `combine_dry_run` MCP tool to support `preview_impacts` parameter in `src/mcp.rs`

**Checkpoint**: At this point, User Stories 1 AND 2 should both work independently.

---

## Phase 5: User Story 3 - Control Facade Visibility and Re-exports (Priority: P3)

**Goal**: Fine-grained control over which items are re-exported at the parent module level via `--re-export-filter`.

**Independent Test**: Run `r2factor combine --re-export-filter "parse|tokenize"` and verify only matching items appear in the facade re-exports.

### Tests for User Story 3

> **NOTE: Write these tests FIRST, ensure they FAIL before implementation**

- [ ] T025 [US3] Add E2E `--re-export-filter` test in `tests/combine_filter_e2e.rs` — verify filtered re-exports in generated facade

### Implementation for User Story 3

- [ ] T026 [US3] Implement `--re-export-filter` pattern parsing and application in `src/combine/facade.rs`
- [ ] T027 [US3] Add `--re-export-filter` CLI flag to combine subcommand in `src/main.rs`

**Checkpoint**: All user stories should now be independently functional.

---

## Phase 6: Polish & Cross-Cutting Concerns

**Purpose**: Improvements that affect multiple user stories

- [ ] T028 [P] Update `README.md` with `combine` command documentation, examples, and MCP install notes
- [ ] T029 [P] Run `cargo fmt` and `cargo clippy --all-targets` — fix all warnings
- [ ] T030 [P] Run quickstart.md validation — execute each command example against fixtures and verify output
- [ ] T031 Performance benchmark: verify combine completes in under 5 seconds for two 500-line files
- [ ] T032 [P] Run full test suite `cargo test` and ensure all tests pass
- [ ] T033 [P] Verify `--help` output for combine subcommand shows all flags with accurate descriptions

---

## Dependencies & Execution Order

### Phase Dependencies

- **Setup (Phase 1)**: No dependencies — can start immediately
- **Foundational (Phase 2)**: Depends on Setup completion — BLOCKS all user stories
- **User Stories (Phase 3–5)**: All depend on Foundational phase completion
  - User stories can proceed in parallel (if staffed)
  - Or sequentially in priority order (P1 → P2 → P3)
- **Polish (Phase 6)**: Depends on all desired user stories being complete

### User Story Dependencies

- **User Story 1 (P1)**: Can start after Foundational (Phase 2) — No dependencies on other stories
- **User Story 2 (P2)**: Can start after Foundational (Phase 2) — Depends on US1 core combine logic but impact report is independently testable
- **User Story 3 (P3)**: Can start after Foundational (Phase 2) — Depends on US1 facade generation but filter is independently testable

### Within Each User Story

- Tests MUST be written and FAIL before implementation
- Foundational modules before orchestrator
- Report formatting before CLI integration
- Core implementation before MCP tool registration
- Story complete before moving to next priority

### Parallel Opportunities

- All Setup tasks marked [P] can run in parallel
- T009 (rewrite visitor) can run in parallel with T006–T008 (facade generation)
- T021 and T022 (impact report) can run in parallel
- All Polish tasks marked [P] can run in parallel
- Different user stories can be worked on in parallel by different team members

---

## Parallel Example: User Story 1

```bash
# Launch all tests for User Story 1 together:
Task: "Add E2E dry-run test in tests/combine_e2e.rs"
Task: "Add E2E --write test in tests/combine_e2e.rs"
Task: "Add E2E --force test in tests/combine_e2e.rs"

# Launch foundational implementation tasks in parallel:
Task: "Implement facade mod declaration generation in src/combine/facade.rs"
Task: "Implement path rewriting visitor in src/combine/rewrite.rs"
```

---

## Implementation Strategy

### MVP First (User Story 1 Only)

1. Complete Phase 1: Setup
2. Complete Phase 2: Foundational (CRITICAL — blocks all stories)
3. Complete Phase 3: User Story 1
4. **STOP and VALIDATE**: Test User Story 1 independently on a real workspace
5. Deploy/demo if ready

### Incremental Delivery

1. Complete Setup + Foundational → Foundation ready
2. Add User Story 1 → Test independently → Deploy/Demo (MVP!)
3. Add User Story 2 → Test independently → Deploy/Demo
4. Add User Story 3 → Test independently → Deploy/Demo
5. Each story adds value without breaking previous stories

### Parallel Team Strategy

With multiple developers:

1. Team completes Setup + Foundational together
2. Once Foundational is done:
   - Developer A: User Story 1 (core combine)
   - Developer B: User Story 2 (impact report)
   - Developer C: User Story 3 (re-export filter)
3. Stories complete and integrate independently

---

## Notes

- [P] tasks = different files, no dependencies
- [Story] label maps task to specific user story for traceability
- Each user story should be independently completable and testable
- Verify tests fail before implementing
- Commit after each task or logical group
- Stop at any checkpoint to validate story independently
- Avoid: vague tasks, same file conflicts, cross-story dependencies that break independence
