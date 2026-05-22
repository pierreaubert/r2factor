# Research: Combine Files into Module

**Date**: 2026-05-21
**Feature**: Combine two peer `.rs` files into a new parent module

## Decisions

### Reuse Existing syn-Based Parsing Infrastructure

**Decision**: Use `syn::File::parse` (already used by split/consolidate/flatten) to parse input files and extract items, visibility, and `use` statements.

**Rationale**: r2factor already depends on `syn` and has proven parsing logic. Introducing a second parser would add maintenance burden and risk inconsistencies.

**Alternatives considered**: None — `syn` is the established standard in this codebase.

### Reuse Existing tokensave Integration Pattern

**Decision**: Follow the same optional `tokensave` integration pattern used by `split` (`--no-tokensave` flag, `.tokensave/` discovery in ancestor directories).

**Rationale**: Principle IV (Graceful Degradation) requires core functionality without optional integrations. The existing pattern already satisfies this.

**Alternatives considered**: Hard dependency on tokensave — rejected because it violates Principle IV.

### Follow Established Dry-Run / Write Pattern

**Decision**: Implement `combine` using the same `Plan → Dry-Run → Write` pipeline pattern as `split` and `consolidate`.

**Rationale**: Consistent UX across all r2factor commands. Users already understand `--write`, `--force`, and dry-run semantics.

**Alternatives considered**: Immediate-write default — rejected because it violates Principle II (Safety by Default).

### Facade Generation Strategy

**Decision**: Generate the facade as a `syn::File` AST, then pretty-print with `quote!` / `prettyplease` (or existing formatter), rather than string concatenation.

**Rationale**: AST-based generation ensures syntactically valid output and makes collision detection (FR-014) straightforward by comparing `syn::Ident` values.

**Alternatives considered**: String templating — rejected because it is fragile and harder to validate for correctness.

### Path Rewriting Approach

**Decision**: Use a `syn::VisitMut` visitor to rewrite `use` statements and path expressions inside moved files, similar to the visibility rebasing in `promote.rs`.

**Rationale**: `promote.rs` already contains path-rebase logic for `split`. Adapting it for `combine` (where files move one level deeper) is simpler than writing new logic from scratch.

**Alternatives considered**: Regex-based rewriting — rejected because it is brittle and unaware of Rust syntax (e.g., strings containing paths).

### Parent Module Declaration Updates

**Decision**: Parse the nearest parent `lib.rs` or `mod.rs` with `syn`, locate `mod` item declarations matching the old file names, remove them, and insert a new `mod <name>;` declaration.

**Rationale**: This is a localized AST transformation. Using `syn` ensures the parent module remains syntactically valid.

**Alternatives considered**: Text-based search/replace — rejected because it could match inside comments or strings.
