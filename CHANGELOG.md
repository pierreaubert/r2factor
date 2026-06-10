# Changelog

## 0.1.5 - 2026-06-09

### Fixed

- Keep proc-macro entrypoints at crate root after splitting by generating facade wrappers for `#[proc_macro]`, `#[proc_macro_derive]`, and `#[proc_macro_attribute]` exports.
- Move proc-macro entrypoint bodies into renamed child-module implementation functions without carrying proc-macro export attributes into child modules.
- Avoid re-exporting child modules solely because they contain proc-macro implementation functions, which keeps generated proc-macro crates valid.
- Preserve `extern crate` declarations at facade scope so proc-macro roots keep access to `proc_macro` after splitting.
- Run post-write Cargo verification for package file splits through CLI and MCP paths, and create bug reports on verification failures.
- Roll back directory splits and write a bug report when affected-package `cargo check -p ...` verification fails.
- Continue affected-package verification after a directory split check failure so one run can collect all package bug reports before rolling back.
- Clean up backups for recursively generated child files during rollback so generated split directories can be removed.
- Preserve `crate::Name` references from existing external module files by lifting only the referenced moved private root items to `pub(crate)` and aliasing them from the facade.
- Preserve `std::io::Write` imports for `write_all` method syntax in generated subfiles.
- Preserve method-only trait imports for Rayon preludes, Syn `Spanned`, AEAD traits, Core Text trait accessors, and byteorder `ReadBytesExt` when pruning generated bucket imports.
- Lift private local extension traits used only through method syntax so sibling generated buckets can import them.
- Preserve imports used only by Rust format-string captures such as `format!("{MPD_VERSION}")`.
- Add an end-to-end proc-macro split regression that compiles the generated crate as `--crate-type=proc-macro`.

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
