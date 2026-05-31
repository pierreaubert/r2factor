# Changelog

## 0.1.2 - 2026-05-31

### Fixed

- Preserve derive macro and serde attribute imports when splitting files so generated buckets compile after moving serialized types.
- Preserve serde string-path defaults, macro-token references, and other attribute-only references when pruning generated bucket imports.
- Keep extension trait imports such as CPAL traits, `Sample`, and serde's `SerializeStruct` when buckets use trait-provided method syntax.
- Keep external `mod` declarations, include macros, and thread-local macro items at facade scope when splitting files.
- Generate targeted sibling imports and facade reexports instead of relying on broad parent-module glob imports.
- Rename generated buckets around existing external module files to avoid duplicate module declarations.
- Promote private signature types used by promoted cross-bucket helpers so sibling modules can call those helpers without private-type errors.
- Avoid emitting invalid cross-bucket imports for synthetic impl-block symbols.
- Support splitting `main.rs` and path-attribute generated modules for Cargo crate-root files such as `bin/*.rs`, `examples/*.rs`, and integration-test roots.
- Unwrap inline test modules into generated `tests.rs` files and rebase relative `include_str!` / `include_bytes!` paths for moved items.
- Add split regression coverage for private return types, nested test modules, include rebasing, derive imports, and CPAL trait imports.
