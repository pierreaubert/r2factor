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
        write: Some(WriteOptions { force: false }),
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
    fs::write(
        &lib_path,
        format!("#![allow(warnings)]\npub mod {stem};\n"),
    )
    .expect("write lib.rs");

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
