//! End-to-end integration tests for the splitter. These run the full
//! pipeline (`r2factor::run_split`) on a fixture in a temp directory and
//! assert observable properties of the generated facade + sub-files:
//!   * expected files exist,
//!   * each generated file is syntactically valid Rust,
//!   * the macro bucket is wired with `#[macro_use]`,
//!   * the facade marker is present,
//!   * per-bucket `use` preludes are minimal (don't drag in unused imports),
//!   * cross-bucket private refs get explicit `use super::<bkt>::<name>;`
//!     imports in every consumer (regression coverage for the gap where
//!     `pub(super)` alone isn't visible to siblings),
//!   * **the whole tree compiles** under `rustc` — the strongest check we
//!     have, since it catches name-resolution and visibility errors that
//!     `syn::parse_file` (purely syntactic) doesn't.

use r2factor::{SplitOptions, run_split, write::WriteOptions};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Copy `src_text` into a tempdir as `<stem>.rs`, run the split, and return
/// (tempdir, facade_path, sub_dir). The tempdir is kept alive by the caller
/// so the inspect-and-assert code below can read the artefacts.
fn split_in_tempdir(stem: &str, src_text: &str) -> (tempfile::TempDir, PathBuf, PathBuf) {
    let tmp = tempfile::tempdir().expect("create tempdir");
    let file = tmp.path().join(format!("{stem}.rs"));
    fs::write(&file, src_text).expect("write source");

    let opts = SplitOptions {
        // Tests must not depend on a `.tokensave/` database — the splitter
        // already gracefully degrades when one isn't found, but disabling
        // explicitly keeps the test output noise-free.
        use_tokensave: false,
        llm: None,
        write: Some(WriteOptions {
            force: false,
            recursive_max_lines: Some(0),
        }),
    };
    run_split(&file, opts).expect("run_split succeeds");

    let sub_dir = tmp.path().join(stem);
    (tmp, file, sub_dir)
}

fn assert_parses(path: &Path) {
    let src = fs::read_to_string(path).expect("read generated file");
    syn::parse_file(&src)
        .unwrap_or_else(|e| panic!("generated {} does not parse: {e}", path.display()));
}

/// Drop a tiny `lib.rs` next to the facade that declares `pub mod <stem>;`,
/// then invoke `rustc --crate-type=lib --edition=2024` on it. rustc resolves
/// `<stem>.rs` and `<stem>/*.rs` through normal module-path rules, so this
/// compiles the entire split tree.
///
/// We use `rustc` directly rather than `cargo` to avoid pulling in a manifest
/// and a fresh target-dir bootstrap on every test run. Warnings are silenced
/// so the test passes/fails on real errors only — unused-import warnings
/// from the generated code aren't the point of the check.
///
/// The `rustc` binary is guaranteed to be on PATH whenever `cargo test` is
/// running this file, so we don't bother with a graceful "skip if missing"
/// branch.
fn compile_check(tmp_root: &Path, stem: &str) {
    let lib_path = tmp_root.join("lib.rs");
    fs::write(&lib_path, format!("#![allow(warnings)]\npub mod {stem};\n")).expect("write lib.rs");

    let out_dir = tmp_root.join("rustc-out");
    fs::create_dir_all(&out_dir).expect("mkdir rustc-out");

    let output = Command::new("rustc")
        .arg("--edition=2024")
        .arg("--crate-type=lib")
        .arg("--emit=metadata")
        .arg("-Awarnings")
        .arg("--out-dir")
        .arg(&out_dir)
        .arg(&lib_path)
        .output()
        .expect("invoke rustc");

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let facade = fs::read_to_string(tmp_root.join(format!("{stem}.rs")))
            .unwrap_or_else(|_| "<facade unreadable>".into());
        panic!(
            "split output for stem `{stem}` failed to compile.\n\n--- rustc stderr ---\n{stderr}\n--- rustc stdout ---\n{stdout}\n--- facade ---\n{facade}"
        );
    }
}

fn compile_existing_lib(tmp_root: &Path) {
    let lib_path = tmp_root.join("lib.rs");
    let out_dir = tmp_root.join("rustc-out");
    fs::create_dir_all(&out_dir).expect("mkdir rustc-out");

    let output = Command::new("rustc")
        .arg("--edition=2024")
        .arg("--crate-type=lib")
        .arg("--emit=metadata")
        .arg("-Awarnings")
        .arg("--out-dir")
        .arg(&out_dir)
        .arg(&lib_path)
        .output()
        .expect("invoke rustc");

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let lib = fs::read_to_string(&lib_path).unwrap_or_else(|_| "<lib.rs unreadable>".into());
        panic!(
            "split lib.rs failed to compile.\n\n--- rustc stderr ---\n{stderr}\n--- rustc stdout ---\n{stdout}\n--- lib.rs ---\n{lib}"
        );
    }
}

#[test]
fn fixture_sample_split_produces_valid_facade_and_subfiles() {
    let src = fs::read_to_string("fixtures/sample.rs").expect("fixture present");
    let (tmp, facade, sub_dir) = split_in_tempdir("sample", &src);

    let facade_src = fs::read_to_string(&facade).expect("read facade");
    assert!(
        facade_src.contains("r2factor:facade"),
        "facade should carry the regenerate-guard marker"
    );
    // Two independent contains() rather than `"#[macro_use]\nmod macros;"`
    // so the test tolerates cosmetic spacing changes between the attribute
    // and the mod declaration. The invariant we care about is that
    // `#[macro_use]` appears *somewhere* attached to `mod macros;`.
    assert!(
        facade_src.contains("#[macro_use]") && facade_src.contains("mod macros;"),
        "macros bucket must be declared with #[macro_use] so siblings can call its macros; got:\n{facade_src}"
    );
    assert!(
        !facade_src.contains("pub use macros::*;"),
        "macro_rules! aren't items — `pub use macros::*;` would be meaningless and noisy"
    );
    assert_parses(&facade);

    for entry in fs::read_dir(&sub_dir).expect("sub-dir exists") {
        let path = entry.expect("dir entry").path();
        if path.extension().and_then(|s| s.to_str()) == Some("rs") {
            assert_parses(&path);
        }
    }

    // `types.rs` only defines `Token` and `Expr` — it shouldn't drag in
    // `use std::collections::HashMap;` from the original file's prelude.
    let types_src = fs::read_to_string(sub_dir.join("types.rs")).expect("types.rs");
    assert!(
        !types_src.contains("HashMap"),
        "per-bucket use prelude should not import HashMap into types.rs; got:\n{types_src}"
    );

    // `env.rs` does use HashMap — confirm the minimal-prelude logic keeps
    // the import where it's actually needed (and didn't strip everything).
    let env_src = fs::read_to_string(sub_dir.join("env.rs")).expect("env.rs");
    assert!(
        env_src.contains("use std::collections::HashMap;"),
        "env.rs uses HashMap and must keep the import"
    );

    // Strongest check: the whole tree compiles. Catches name-resolution /
    // visibility regressions that the per-file syn::parse_file passes miss.
    compile_check(tmp.path(), "sample");
}

#[test]
fn cross_bucket_private_fn_gets_pub_super_and_explicit_imports() {
    // Two pub structs in different anchor buckets both call a private
    // helper. The pre-fix algorithm produced `pub(super) fn normalize`
    // but no `use super::misc::normalize;` in the consumers, leaving the
    // bare names unresolved.
    let src = r#"use std::collections::HashMap;

pub struct Parser { _h: HashMap<String, u32> }

impl Parser {
    pub fn new() -> Self { Self { _h: HashMap::new() } }
    pub fn parse(&self, s: &str) -> String { normalize(s) }
}

pub struct Lexer;

impl Lexer {
    pub fn lex(&self, s: &str) -> Vec<String> { vec![normalize(s)] }
}

fn normalize(s: &str) -> String { s.trim().to_lowercase() }
"#;
    let (tmp, _facade, sub_dir) = split_in_tempdir("crossref", src);

    let misc = fs::read_to_string(sub_dir.join("misc.rs")).expect("misc.rs");
    assert!(
        misc.contains("pub(super) fn normalize"),
        "normalize should be lifted to pub(super); got:\n{misc}"
    );

    for bucket in ["parser.rs", "lexer.rs"] {
        let body = fs::read_to_string(sub_dir.join(bucket)).expect(bucket);
        assert!(
            body.contains("use super::misc::normalize;"),
            "{bucket} calls normalize and must import super::misc::normalize; got:\n{body}"
        );
        assert_parses(&sub_dir.join(bucket));
    }

    compile_check(tmp.path(), "crossref");
}

#[test]
fn unit_tests_follow_tested_bucket_and_cross_bucket_tests_stay_in_tests() {
    let src = r#"pub struct Parser;

impl Parser {
    pub fn parse(&self, input: &str) -> usize { input.len() }
}

pub struct Lexer;

impl Lexer {
    pub fn lex(&self, input: &str) -> usize { input.bytes().count() }
}

#[test]
fn parser_unit_test() {
    assert_eq!(Parser.parse("abc"), 3);
}

#[test]
fn parser_lexer_integration_test() {
    assert_eq!(Parser.parse("abc"), Lexer.lex("abc"));
}
"#;
    let (tmp, _facade, sub_dir) = split_in_tempdir("test_place", src);

    let parser_src = fs::read_to_string(sub_dir.join("parser.rs")).expect("parser.rs");
    assert!(
        parser_src.contains("fn parser_unit_test"),
        "Parser-only unit test should live with parser bucket; got:\n{parser_src}"
    );
    assert!(
        !parser_src.contains("fn parser_lexer_integration_test"),
        "cross-bucket test should not live in parser bucket; got:\n{parser_src}"
    );

    let tests_src = fs::read_to_string(sub_dir.join("tests.rs")).expect("tests.rs");
    assert!(
        tests_src.contains("fn parser_lexer_integration_test"),
        "cross-bucket test should live in tests.rs; got:\n{tests_src}"
    );
    assert!(
        !tests_src.contains("fn parser_unit_test"),
        "unit test should not be stranded in tests.rs; got:\n{tests_src}"
    );

    compile_check(tmp.path(), "test_place");
}

#[test]
fn facade_primary_referencing_promoted_helper_compiles() {
    // The stem-bucket primary (`Cf3`, since stem is "cf3") references a
    // private `load_config` fn that ends up in a sub-bucket. Without
    // `compute_facade_imports` emitting `use <bkt>::load_config;` at the
    // facade scope, the bare name in the primary's body would fail
    // resolution — even though `load_config` is now `pub(super)`.
    //
    // This is the only e2e path that exercises facade-side imports; the
    // earlier cross-bucket test only covered sub-to-sub imports.
    // Stem-bucket primary `Cf3` references both a private fn (`load_config`)
    // AND reads a private field on a moved struct (`self.cfg.v`). The field
    // access exercises the field-vis lift on promoted structs; without it
    // rustc rejects with E0616 ("field is private").
    let src = r#"pub struct Cf3 {
    cfg: Config,
}

impl Cf3 {
    pub fn new() -> Self {
        Self { cfg: load_config() }
    }
    pub fn version(&self) -> u32 {
        self.cfg.v
    }
}

struct Config { v: u32 }

fn load_config() -> Config { Config { v: 1 } }
"#;
    let (tmp, facade, _sub_dir) = split_in_tempdir("cf3", src);
    let facade_src = fs::read_to_string(&facade).expect("read facade");

    // The exact sub-bucket name depends on clustering decisions (likely
    // "config" since `load_config` refs `Config` and pull_misc_by_calls
    // pulls it there) — we don't pin it. What we DO pin: a bare `use
    // <bkt>::Config;` and `use <bkt>::load_config;` must appear so the
    // primary's bodies resolve.
    let has_config_import = facade_src
        .lines()
        .any(|l| l.trim_start().starts_with("use ") && l.contains("::Config;"));
    let has_loader_import = facade_src
        .lines()
        .any(|l| l.trim_start().starts_with("use ") && l.contains("::load_config;"));
    assert!(
        has_config_import && has_loader_import,
        "facade should `use <bkt>::Config;` and `use <bkt>::load_config;`; got:\n{facade_src}"
    );

    compile_check(tmp.path(), "cf3");
}

#[test]
fn tuple_struct_field_lift_compiles_cross_bucket() {
    // Tuple-field visibility follows the same rule as named-field
    // visibility — `pub(super) struct Tagged(u32)` doesn't let a sibling
    // read `.0`. The field-lift code rewrites the tuple type to
    // `pub(super) u32`. This test fails to compile if that rewrite is
    // skipped.
    let src = r#"pub struct Outer {
    inner: Tagged,
}

impl Outer {
    pub fn new() -> Self { Self { inner: make() } }
    pub fn raw(&self) -> u32 { self.inner.0 }
}

struct Tagged(u32);

fn make() -> Tagged { Tagged(7) }
"#;
    let (tmp, _facade, _sub_dir) = split_in_tempdir("tup", src);
    compile_check(tmp.path(), "tup");
}

#[test]
fn splits_isolated_lib_rs_into_sibling_modules() {
    let src = r#"#![allow(dead_code)]

macro_rules! answer {
    () => { 42 };
}

pub struct Foo;
pub struct Bar;
pub struct Baz;

pub fn value() -> u32 {
    answer!()
}

#[cfg(test)]
fn test_only() {
    assert_eq!(value(), 42);
}
"#;
    let tmp = tempfile::tempdir().expect("tempdir");
    let file = tmp.path().join("lib.rs");
    fs::write(&file, src).expect("write lib.rs");

    run_split(
        &file,
        SplitOptions {
            use_tokensave: false,
            llm: None,
            write: Some(WriteOptions {
                force: false,
                recursive_max_lines: Some(0),
            }),
        },
    )
    .expect("split lib.rs");

    let lib_dir = tmp.path().join("lib");
    assert!(
        tmp.path().join("types.rs").exists(),
        "types.rs should be generated beside isolated lib.rs"
    );
    assert!(
        tmp.path().join("macros.rs").exists(),
        "macros.rs should be generated beside isolated lib.rs"
    );
    assert!(
        !tmp.path().join("tests.rs").exists(),
        "single-bucket unit tests should stay with their tested code instead of creating tests.rs"
    );
    assert!(
        !lib_dir.exists(),
        "isolated lib.rs should not create a lib/ subdirectory"
    );

    let facade_src = fs::read_to_string(&file).expect("read generated lib.rs");
    assert!(
        facade_src.contains("mod types;"),
        "isolated lib.rs facade should use normal sibling mod declarations; got:\n{facade_src}"
    );
    assert!(
        facade_src.contains("#[macro_use]\nmod macros;"),
        "isolated lib.rs facade should not path-attribute macros; got:\n{facade_src}"
    );
    assert!(
        !facade_src.contains("#[path = \"lib/"),
        "isolated lib.rs facade should not use lib/ path attributes; got:\n{facade_src}"
    );
    let macros_src = fs::read_to_string(tmp.path().join("macros.rs")).expect("macros.rs");
    assert!(
        macros_src.contains("fn test_only"),
        "unit test for value() should stay with the bucket containing value(); got:\n{macros_src}"
    );

    assert_parses(&file);
    for path in [tmp.path().join("types.rs"), tmp.path().join("macros.rs")] {
        assert_parses(&path);
    }
    compile_existing_lib(tmp.path());
}

#[test]
fn split_lib_rs_with_existing_lib_dir_uses_path_attributed_child_modules() {
    let src = r#"#![allow(dead_code)]

macro_rules! answer {
    () => { 42 };
}

pub struct Foo;
pub struct Bar;
pub struct Baz;

pub fn value() -> u32 {
    answer!()
}

#[cfg(test)]
fn test_only() {
    assert_eq!(value(), 42);
}
"#;
    let tmp = tempfile::tempdir().expect("tempdir");
    let file = tmp.path().join("lib.rs");
    fs::write(&file, src).expect("write lib.rs");
    let lib_dir = tmp.path().join("lib");
    fs::create_dir_all(&lib_dir).expect("mkdir lib");
    fs::write(
        lib_dir.join("existing.rs"),
        "pub fn helper() -> u32 { 42 }\n",
    )
    .expect("write existing.rs");

    run_split(
        &file,
        SplitOptions {
            use_tokensave: false,
            llm: None,
            write: Some(WriteOptions {
                force: false,
                recursive_max_lines: Some(0),
            }),
        },
    )
    .expect("split lib.rs");

    assert!(
        lib_dir.join("types.rs").exists(),
        "types.rs should be generated under existing lib/"
    );
    let facade_src = fs::read_to_string(&file).expect("read generated lib.rs");
    assert!(
        facade_src.contains("#[path = \"lib/types.rs\"]\nmod types;"),
        "lib.rs facade should path-attribute generated modules when lib/ exists; got:\n{facade_src}"
    );
    assert!(
        facade_src.contains("#[path = \"lib/existing.rs\"]\nmod existing;"),
        "existing lib/ modules should remain path-attributed; got:\n{facade_src}"
    );

    assert_parses(&file);
    for entry in fs::read_dir(&lib_dir).expect("lib dir exists") {
        let path = entry.expect("dir entry").path();
        if path.extension().and_then(|s| s.to_str()) == Some("rs") {
            assert_parses(&path);
        }
    }
    compile_existing_lib(tmp.path());
}

#[test]
fn splits_mod_rs_in_place_without_declaring_mod_mod() {
    let src = r#"pub struct Foo;
pub struct Bar;
pub struct Baz;

pub fn module_value() -> u32 {
    sibling::helper()
}
"#;
    let tmp = tempfile::tempdir().expect("tempdir");
    let module_dir = tmp.path().join("foo");
    fs::create_dir_all(&module_dir).expect("mkdir foo");
    let file = module_dir.join("mod.rs");
    fs::write(&file, src).expect("write mod.rs");
    fs::write(
        module_dir.join("sibling.rs"),
        "pub fn helper() -> u32 { 9 }\n",
    )
    .expect("write sibling.rs");

    run_split(
        &file,
        SplitOptions {
            use_tokensave: false,
            llm: None,
            write: Some(WriteOptions {
                force: false,
                recursive_max_lines: Some(0),
            }),
        },
    )
    .expect("split mod.rs");

    assert!(
        module_dir.join("types.rs").exists(),
        "types.rs should be generated beside mod.rs"
    );
    let facade_src = fs::read_to_string(&file).expect("read generated mod.rs");
    assert!(
        !facade_src.contains("mod mod;"),
        "mod.rs facade must not declare itself as a child module; got:\n{facade_src}"
    );
    assert!(
        facade_src.contains("mod sibling;"),
        "existing sibling modules should be preserved in the facade; got:\n{facade_src}"
    );
    assert!(
        facade_src.contains("mod types;"),
        "generated sibling modules should use normal mod declarations; got:\n{facade_src}"
    );

    assert_parses(&file);
    assert_parses(&module_dir.join("types.rs"));
    compile_check(tmp.path(), "foo");
}

#[test]
fn split_lib_rs_preserves_existing_lib_submodules() {
    let src = r#"pub struct Foo;
pub struct Bar;
pub struct Baz;
"#;
    let tmp = tempfile::tempdir().expect("tempdir");
    let file = tmp.path().join("lib.rs");
    fs::write(&file, src).expect("write lib.rs");
    let lib_dir = tmp.path().join("lib");
    fs::create_dir_all(&lib_dir).expect("mkdir lib");
    fs::write(
        lib_dir.join("existing.rs"),
        "pub fn helper() -> u32 { 42 }\n",
    )
    .expect("write existing.rs");

    run_split(
        &file,
        SplitOptions {
            use_tokensave: false,
            llm: None,
            write: Some(WriteOptions {
                force: false,
                recursive_max_lines: Some(0),
            }),
        },
    )
    .expect("split lib.rs");

    let existing_src = fs::read_to_string(lib_dir.join("existing.rs")).expect("read existing.rs");
    assert_eq!(
        existing_src, "pub fn helper() -> u32 { 42 }\n",
        "existing lib/existing.rs should be preserved"
    );
    let facade_src = fs::read_to_string(&file).expect("read generated lib.rs");
    assert!(
        facade_src.contains("#[path = \"lib/existing.rs\"]\nmod existing;"),
        "existing lib/ modules should be path-attributed in the facade; got:\n{facade_src}"
    );
    assert!(
        facade_src.contains("pub use existing::*;"),
        "existing lib/ modules should still be re-exported; got:\n{facade_src}"
    );
    compile_existing_lib(tmp.path());
}

#[test]
fn split_mod_rs_force_stale_cleanup_keeps_facade() {
    let src = r#"pub struct Foo;
pub struct Bar;
pub struct Baz;
"#;
    let tmp = tempfile::tempdir().expect("tempdir");
    let module_dir = tmp.path().join("foo");
    fs::create_dir_all(&module_dir).expect("mkdir foo");
    let file = module_dir.join("mod.rs");
    fs::write(&file, src).expect("write mod.rs");
    fs::write(
        module_dir.join("old.rs"),
        "// Auto-generated by r2factor. Manual edits will be overwritten on next split.\n",
    )
    .expect("write stale old.rs");

    run_split(
        &file,
        SplitOptions {
            use_tokensave: false,
            llm: None,
            write: Some(WriteOptions {
                force: true,
                recursive_max_lines: Some(0),
            }),
        },
    )
    .expect("split mod.rs with force");

    assert!(file.exists(), "mod.rs facade must survive force cleanup");
    assert!(
        !module_dir.join("old.rs").exists(),
        "stale generated sibling files should be removed under --force"
    );
    assert!(
        module_dir.join("types.rs").exists(),
        "types.rs should be generated beside mod.rs"
    );
    compile_check(tmp.path(), "foo");
}

#[test]
fn refuses_to_split_a_facade() {
    // Running r2factor on its own output destroys the previous split —
    // the marker guard must fire BEFORE the dry-run, not just before
    // writing.
    let src = "// r2factor:facade — do not pass this file back into r2factor\nmod foo;\n";
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("already_split.rs");
    fs::write(&path, src).expect("write");

    let err = run_split(
        &path,
        SplitOptions {
            use_tokensave: false,
            llm: None,
            write: None,
        },
    )
    .expect_err("should bail");
    let msg = format!("{err}");
    assert!(
        msg.contains("facade"),
        "error message should mention facade, got: {msg}"
    );
}

#[test]
fn split_preserves_existing_subfiles_and_adapts_facade() {
    // If a target directory already contains user-created .rs files, the
    // splitter should preserve them and include them in the generated facade.
    let src = r#"pub struct Foo;
pub struct Bar;
pub struct Baz;
"#;
    let tmp = tempfile::tempdir().expect("tempdir");
    let file = tmp.path().join("demo.rs");
    fs::write(&file, src).expect("write source");

    let sub_dir = tmp.path().join("demo");
    fs::create_dir_all(&sub_dir).expect("mkdir demo");
    fs::write(
        sub_dir.join("existing.rs"),
        "pub fn helper() -> u32 { 42 }\n",
    )
    .expect("write existing.rs");

    let opts = SplitOptions {
        use_tokensave: false,
        llm: None,
        write: Some(WriteOptions {
            force: false,
            recursive_max_lines: Some(0),
        }),
    };
    run_split(&file, opts).expect("run_split succeeds");

    // The user-created file must be untouched.
    let existing_src = fs::read_to_string(sub_dir.join("existing.rs")).expect("read existing.rs");
    assert_eq!(
        existing_src, "pub fn helper() -> u32 { 42 }\n",
        "existing.rs should be preserved"
    );

    // The facade must declare and re-export the existing module.
    let facade_src = fs::read_to_string(&file).expect("read facade");
    assert!(
        facade_src.contains("mod existing;"),
        "facade should declare mod existing; got:\n{facade_src}"
    );
    assert!(
        facade_src.contains("pub use existing::*;"),
        "facade should re-export existing; got:\n{facade_src}"
    );

    // The newly-generated types.rs must also be present.
    assert!(
        sub_dir.join("types.rs").exists(),
        "types.rs should be generated"
    );

    compile_check(tmp.path(), "demo");
}

#[test]
fn split_errors_on_conflicting_file_without_force() {
    // Three plain structs produce a `types.rs` bucket. If that file already
    // exists and --force is not given, the split must bail with a clear error.
    let src = r#"pub struct Foo;
pub struct Bar;
pub struct Baz;
"#;
    let tmp = tempfile::tempdir().expect("tempdir");
    let file = tmp.path().join("demo.rs");
    fs::write(&file, src).expect("write source");

    let sub_dir = tmp.path().join("demo");
    fs::create_dir_all(&sub_dir).expect("mkdir demo");
    fs::write(sub_dir.join("types.rs"), "// old\n").expect("write types.rs");

    let opts = SplitOptions {
        use_tokensave: false,
        llm: None,
        write: Some(WriteOptions {
            force: false,
            recursive_max_lines: Some(0),
        }),
    };
    let err = run_split(&file, opts).expect_err("should bail because types.rs exists");
    let msg = format!("{err}");
    assert!(
        msg.contains("types"),
        "error should mention the conflicting file 'types', got: {msg}"
    );

    // The conflicting file must NOT have been overwritten.
    let types_src = fs::read_to_string(sub_dir.join("types.rs")).expect("read types.rs");
    assert_eq!(
        types_src, "// old\n",
        "types.rs should still be the original"
    );
}

#[test]
fn split_overwrites_conflicting_file_with_force() {
    // With --force, a conflicting existing file is overwritten and the split
    // proceeds normally.
    let src = r#"pub struct Foo;
pub struct Bar;
pub struct Baz;
"#;
    let tmp = tempfile::tempdir().expect("tempdir");
    let file = tmp.path().join("demo.rs");
    fs::write(&file, src).expect("write source");

    let sub_dir = tmp.path().join("demo");
    fs::create_dir_all(&sub_dir).expect("mkdir demo");
    fs::write(sub_dir.join("types.rs"), "// old\n").expect("write types.rs");

    let opts = SplitOptions {
        use_tokensave: false,
        llm: None,
        write: Some(WriteOptions {
            force: true,
            recursive_max_lines: Some(0),
        }),
    };
    run_split(&file, opts).expect("run_split should succeed with force");

    // The conflicting file must have been replaced by the generated content.
    let types_src = fs::read_to_string(sub_dir.join("types.rs")).expect("read types.rs");
    assert!(
        types_src.contains("Auto-generated by r2factor"),
        "types.rs should now be the generated file; got:\n{types_src}"
    );

    compile_check(tmp.path(), "demo");
}

#[test]
fn recursively_splits_generated_files_over_line_threshold() {
    let mut src = String::new();
    for i in 0..12 {
        src.push_str(&format!(
            "pub fn parse_{i}(input: &str) -> usize {{\n    input.len() + {i}\n}}\n\n"
        ));
    }

    let tmp = tempfile::tempdir().expect("tempdir");
    let file = tmp.path().join("recur.rs");
    fs::write(&file, src).expect("write source");

    run_split(
        &file,
        SplitOptions {
            use_tokensave: false,
            llm: None,
            write: Some(WriteOptions {
                force: false,
                recursive_max_lines: Some(20),
            }),
        },
    )
    .expect("recursive split succeeds");

    let parse_file = tmp.path().join("recur").join("parse.rs");
    let parse_src = fs::read_to_string(&parse_file).expect("read recursive facade");
    assert!(
        parse_src.contains("r2factor:facade"),
        "oversized generated parse.rs should be recursively split into a facade; got:\n{parse_src}"
    );
    assert!(
        tmp.path().join("recur").join("parse.rs.bak").exists(),
        "recursive split should back up the oversized generated file"
    );
    compile_check(tmp.path(), "recur");
}
