<!--
SYNC IMPACT REPORT
==================
Version change: template → 1.0.0
Modified principles: All placeholders replaced with concrete principles
  - PRINCIPLE_1: Deterministic Correctness
  - PRINCIPLE_2: Safety by Default
  - PRINCIPLE_3: Test-First for Transformations
  - PRINCIPLE_4: Graceful Degradation
  - PRINCIPLE_5: Observability and Transparency
Added sections:
  - Core Principles (5 principles)
  - Quality Gates & Constraints
  - Development Workflow
  - Governance
Removed sections: None
Templates requiring updates:
  - .specify/templates/plan-template.md ✅ verified (Constitution Check gate present, no agent-specific refs)
  - .specify/templates/spec-template.md ✅ verified (no outdated principle refs)
  - .specify/templates/tasks-template.md ✅ verified (task categories align with principles)
  - .specify/templates/checklist-template.md ✅ verified
Follow-up TODOs:
  - TODO(RATIFICATION_DATE): project inception date unknown; fill when historical date is confirmed
-->

# r2factor Constitution

## Core Principles

### I. Deterministic Correctness

Every code transformation MUST produce compilable Rust output without manual fixup.
Visibility rebasing, path rewriting, and cross-import lifting MUST be handled
automatically. The tool's primary value proposition is that users can run it and
then immediately run `cargo check` or `cargo build` with zero manual edits.

**Rationale**: If the output does not compile, the tool fails its sole purpose.
Determinism is non-negotiable.

### II. Safety by Default

All file-system mutations require an explicit `--write` flag. Dry-run mode MUST
be the default. When `--write` is used, the original file MUST be backed up to a
`.bak` timestamped copy before any overwrite. Re-running a transformation on an
already-generated facade MUST be refused via an explicit sentinel guard.

**Rationale**: Users trust the tool with their source code. Accidental data loss
or double-transformation is unacceptable.

### III. Test-First for Transformations

New transformation logic MUST be accompanied by end-to-end tests on real Rust
workspaces before merge. Bug fixes MUST follow Red-Green-Refactor: write a
failing E2E test that reproduces the issue, then fix it. Unit tests are
encouraged but do not replace E2E validation on non-trivial codebases.

**Rationale**: Rust's module system, visibility rules, and macro hygiene contain
edge cases that synthetic unit tests cannot reliably surface. Real-world
validation is the only trustworthy signal.

### IV. Graceful Degradation

Core functionality MUST work without optional integrations. The splitter,
consolidator, and flattener MUST operate correctly when `tokensave` is
unavailable or disabled and when the LLM advisor is not configured. The MCP
server MUST remain stateless and protocol-compliant regardless of environment.

**Rationale**: The tool is used in CI pipelines, minimal containers, and
air-gapped environments. Optional features must not become hard dependencies.

### V. Observability and Transparency

Dry-run output MUST show the exact proposed changes: bucket names, item counts,
file paths, and cohesion metrics. Rationale for each clustering decision MUST be
exposed in the plan. Logs and diagnostics MUST go to stderr so stdout remains
safe for structured protocol output (JSON-RPC, JSON reports).

**Rationale**: Users must be able to inspect, diff, and approve changes before
trusting the tool with their codebase. Clean separation of diagnostic and
protocol streams prevents MCP framing corruption.

## Quality Gates & Constraints

- **Rust toolchain**: Latest stable channel; `cargo clippy --all-targets` MUST
  produce zero warnings.
- **Test gate**: `cargo test` MUST pass entirely before any PR is merged.
- **E2E gate**: At least one non-trivial real-workspace validation MUST pass
  before a release is tagged.
- **MSRV policy**: Track latest stable Rust; MSRV bumps are MINOR version
  changes.
- **Formatting**: `cargo fmt` clean; enforced in CI.
- **Documentation**: Public APIs and CLI flags MUST be documented in README.md;
  internal modules SHOULD carry module-level doc comments.

## Development Workflow

1. **Specification before implementation**: Every feature MUST start with a
   `spec.md` and `plan.md` under `.specify/` (or `specs/` if the feature is
   large).
2. **Constitution check**: The plan MUST pass a Constitution Check gate before
   implementation begins. Violations MUST be justified in the Complexity
   Tracking section.
3. **Branch hygiene**: Feature work happens on dedicated branches; main is
   always green.
4. **Review requirements**: PRs require passing CI and at least one human
   review. E2E test additions are strongly preferred for any transformation
   change.
5. **Release tagging**: Releases follow SemVer for the binary artifact. Patch
  bumps for fixes, MINOR for new flags or MCP tools, MAJOR for breaking CLI
  contract changes.

## Governance

This constitution supersedes all other project practices. Amendments require:

- A documented rationale in the pull request.
- A version bump according to semantic versioning rules:
  - MAJOR: backward-incompatible governance or principle removal/redefinition.
  - MINOR: new principle or materially expanded guidance.
  - PATCH: clarifications, wording, typo fixes, non-semantic refinements.
- Explicit approval and merge by a project maintainer.

Compliance review is expected before every release. Runtime development
guidance lives in `README.md`.

**Version**: 1.0.0 | **Ratified**: TODO(RATIFICATION_DATE): project inception date unknown | **Last Amended**: 2026-05-21
