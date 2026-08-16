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
    fs::write(tmp.path().join("README.md"), "fixture sibling\n").expect("write sibling");

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

fn compile_lib_root(lib_path: &Path) {
    let tmp_root = lib_path.parent().expect("lib parent");
    let out_dir = tmp_root.join("rustc-out");
    fs::create_dir_all(&out_dir).expect("mkdir rustc-out");

    let output = Command::new("rustc")
        .arg("--edition=2024")
        .arg("--crate-type=lib")
        .arg("--emit=metadata")
        .arg("-Awarnings")
        .arg("--out-dir")
        .arg(&out_dir)
        .arg(lib_path)
        .output()
        .expect("invoke rustc");

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let facade = fs::read_to_string(lib_path).unwrap_or_else(|_| "<facade unreadable>".into());
        panic!(
            "split lib root failed to compile.\n\n--- rustc stderr ---\n{stderr}\n--- rustc stdout ---\n{stdout}\n--- facade ---\n{facade}"
        );
    }
}

fn compile_test_check(tmp_root: &Path, stem: &str) {
    let lib_path = tmp_root.join("lib.rs");
    fs::write(&lib_path, format!("#![allow(warnings)]\npub mod {stem};\n")).expect("write lib.rs");

    let out_dir = tmp_root.join("rustc-test-out");
    fs::create_dir_all(&out_dir).expect("mkdir rustc-test-out");

    let output = Command::new("rustc")
        .arg("--edition=2024")
        .arg("--test")
        .arg("-Awarnings")
        .arg("--out-dir")
        .arg(&out_dir)
        .arg(&lib_path)
        .output()
        .expect("invoke rustc --test");

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let facade = fs::read_to_string(tmp_root.join(format!("{stem}.rs")))
            .unwrap_or_else(|_| "<facade unreadable>".into());
        panic!(
            "split test output for stem `{stem}` failed to compile.\n\n--- rustc stderr ---\n{stderr}\n--- rustc stdout ---\n{stdout}\n--- facade ---\n{facade}"
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
fn sidecar_module_rs_keeps_its_facade_file() {
    let src = r#"use std::fmt;

pub struct Runner {
    value: u32,
}

impl Runner {
    pub fn new() -> Self {
        Self { value: adjust(1) }
    }
}

impl fmt::Display for Runner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.value)
    }
}

fn adjust(value: u32) -> u32 {
    value + 1
}
"#;
    let (tmp, facade, sub_dir) = split_in_tempdir("helper", src);

    assert!(
        facade.exists(),
        "splitting helper.rs must leave helper.rs as the module facade"
    );
    assert!(
        sub_dir.join("runner.rs").exists(),
        "sidecar split should place generated buckets under helper/"
    );
    assert!(
        !sub_dir.join("mod.rs").exists(),
        "sidecar modules should not require helper/mod.rs when helper.rs is the declared module"
    );

    let facade_src = fs::read_to_string(&facade).expect("read facade");
    assert!(
        facade_src.contains("mod runner;"),
        "helper.rs should declare generated child modules directly; got:\n{facade_src}"
    );
    compile_check(tmp.path(), "helper");
}

fn compile_binary_root(path: &Path) {
    let out_dir = path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("rustc-out");
    fs::create_dir_all(&out_dir).expect("mkdir rustc-out");

    let output = Command::new("rustc")
        .arg("--edition=2024")
        .arg("--emit=metadata")
        .arg("-Awarnings")
        .arg("--out-dir")
        .arg(&out_dir)
        .arg(path)
        .output()
        .expect("invoke rustc");

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let facade = fs::read_to_string(path).unwrap_or_else(|_| "<facade unreadable>".into());
        panic!(
            "split binary root {} failed to compile.\n\n--- rustc stderr ---\n{stderr}\n--- rustc stdout ---\n{stdout}\n--- facade ---\n{facade}",
            path.display()
        );
    }
}

fn compile_proc_macro_root(path: &Path) {
    let out_dir = path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("rustc-proc-macro-out");
    fs::create_dir_all(&out_dir).expect("mkdir rustc-proc-macro-out");

    let output = Command::new("rustc")
        .arg("--edition=2024")
        .arg("--crate-type=proc-macro")
        .arg("--emit=metadata")
        .arg("-Awarnings")
        .arg("--out-dir")
        .arg(&out_dir)
        .arg(path)
        .output()
        .expect("invoke rustc proc macro");

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let facade = fs::read_to_string(path).unwrap_or_else(|_| "<facade unreadable>".into());
        panic!(
            "split proc-macro root {} failed to compile.\n\n--- rustc stderr ---\n{stderr}\n--- rustc stdout ---\n{stdout}\n--- facade ---\n{facade}",
            path.display()
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
    // `macro_rules!` definitions have textual scope. Keeping them in the
    // facade before child module declarations lets generated siblings use
    // those macros without a separate `#[macro_use]` bucket.
    let macro_pos = facade_src
        .find("macro_rules! tok")
        .expect("macro_rules definition in facade");
    let module_pos = facade_src.find("mod lexer;").expect("module declarations");
    assert!(
        macro_pos < module_pos,
        "macro_rules definition should render before child modules; got:\n{facade_src}"
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
fn promoted_helper_private_return_type_gets_promoted_too() {
    let src = r#"pub struct Caller;

impl Caller {
    pub fn value(&self) -> u32 { make_hidden().value() }
}

struct Hidden {
    value: u32,
}

impl Hidden {
    fn value(&self) -> u32 { self.value }
}

fn make_hidden() -> Hidden {
    Hidden { value: 7 }
}
"#;
    let (tmp, _facade, _sub_dir) = split_in_tempdir("private_return", src);
    compile_check(tmp.path(), "private_return");
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
fn cfg_test_shared_declaration_and_unwrapped_tests_module_compile() {
    let tmp = tempfile::tempdir().expect("create tempdir");
    let file = tmp.path().join("tree.rs");
    let target = tmp.path().join("tree");
    fs::write(
        &file,
        r#"#[cfg(test)]
pub const TREE_BASE: usize = 2;

#[cfg(not(test))]
pub const TREE_BASE: usize = 6;

pub struct Tree;

pub fn capacity() -> usize {
    TREE_BASE * 2
}

#[cfg(test)]
mod tests {
    #[test]
    fn capacity_uses_test_tree_base() {
        assert_eq!(super::capacity(), 4);
        let _ = super::Tree;
    }
}
"#,
    )
    .expect("write source");
    fs::create_dir_all(&target).expect("mkdir target");
    fs::write(target.join("cursor.rs"), "use super::*;\n").expect("write sibling");

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
    .expect("split tree.rs");

    let facade = fs::read_to_string(&file).expect("read facade");
    assert!(facade.contains("mod tests;"), "facade={facade}");
    assert!(
        !facade.contains("tests::tests"),
        "unwrapped inline test modules must not become self-imports: {facade}"
    );
    compile_check(tmp.path(), "tree");
    compile_test_check(tmp.path(), "tree");
}

#[test]
fn external_module_crate_root_private_refs_survive_lib_split() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let lib = tmp.path().join("lib.rs");
    fs::write(
        tmp.path().join("denoise.rs"),
        r#"use crate::{Complex, NB_DELTA_CEPS};

pub fn denoise(buf: &mut [f32]) -> Complex {
    crate::apply_window(buf);
    [NB_DELTA_CEPS as f32, buf.len() as f32]
}
"#,
    )
    .expect("write denoise.rs");
    fs::write(
        &lib,
        r#"mod denoise;

type Complex = [f32; 2];

const NB_DELTA_CEPS: usize = 2;

fn apply_window(buf: &mut [f32]) {
    for sample in buf {
        *sample *= 0.5;
    }
}

pub struct Denoiser;

impl Denoiser {
    pub fn process(buf: &mut [f32]) -> Complex {
        denoise::denoise(buf)
    }
}

pub struct OtherApi;
"#,
    )
    .expect("write lib.rs");

    run_split(
        &lib,
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

    let facade_src = fs::read_to_string(&lib).expect("read facade");
    assert!(
        facade_src.contains("pub(crate) use") && facade_src.contains("Complex"),
        "facade should preserve crate::Complex for existing modules:\n{facade_src}"
    );
    assert!(
        facade_src.contains("pub(crate) use") && facade_src.contains("NB_DELTA_CEPS"),
        "facade should preserve crate::NB_DELTA_CEPS for existing modules:\n{facade_src}"
    );
    assert!(
        facade_src.contains("pub(crate) use") && facade_src.contains("apply_window"),
        "facade should preserve crate::apply_window for existing modules:\n{facade_src}"
    );
    let generated =
        fs::read_to_string(tmp.path().join("lib").join("misc.rs")).expect("read generated misc.rs");
    assert!(
        generated.contains("pub(crate) fn apply_window"),
        "externally referenced private fn should be lifted to pub(crate):\n{generated}"
    );
    compile_lib_root(&lib);
}

#[test]
fn inline_test_module_is_unwrapped_and_relative_includes_rebased() {
    let tmp = tempfile::tempdir().expect("create tempdir");
    let stem = "nested_tests";
    let file = tmp.path().join(format!("{stem}.rs"));
    fs::write(tmp.path().join("fixture.txt"), "ok").expect("write fixture");
    fs::write(
        &file,
        r#"pub struct Parser;

impl Parser {
    pub fn parse(&self) -> &'static str { include_str!("fixture.txt") }
}

pub struct Lexer;

impl Lexer {
    pub fn lex(&self) -> usize { 2 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parser_reads_fixture() {
        assert_eq!(Parser.parse(), "ok");
        assert_eq!(Lexer.lex(), 2);
    }
}
"#,
    )
    .expect("write source");

    let opts = SplitOptions {
        use_tokensave: false,
        llm: None,
        write: Some(WriteOptions {
            force: false,
            recursive_max_lines: Some(0),
        }),
    };
    run_split(&file, opts).expect("run_split succeeds");

    let sub_dir = tmp.path().join(stem);
    let tests_src = fs::read_to_string(sub_dir.join("tests.rs")).expect("tests.rs");
    assert!(
        !tests_src.contains("mod tests"),
        "tests.rs should contain the inline test body, not a nested module; got:\n{tests_src}"
    );
    assert!(
        tests_src.contains("fn parser_reads_fixture"),
        "tests.rs should keep the original test body; got:\n{tests_src}"
    );

    let parser_src = fs::read_to_string(sub_dir.join("parser.rs")).expect("parser.rs");
    assert!(
        parser_src.contains("include_str!(\"../fixture.txt\")"),
        "include_str! path should be rebased from subdir to original dir; got:\n{parser_src}"
    );

    compile_check(tmp.path(), stem);
    compile_test_check(tmp.path(), stem);
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
fn original_pub_super_impl_method_is_rebased_for_sibling_bucket_calls() {
    let src = r#"pub struct Profile {
    value: u32,
}

impl Profile {
    pub fn new(value: u32) -> Self { Self { value } }
    pub(super) fn matching_band(&self) -> u32 { self.value }
}

pub struct Report;

impl Report {
    pub fn render(profile: &Profile) -> u32 {
        profile.matching_band()
    }
}
"#;
    let (tmp, _facade, sub_dir) = split_in_tempdir("scope", src);

    let profile_src = fs::read_to_string(sub_dir.join("profile.rs")).expect("profile.rs");
    assert!(
        profile_src.contains("pub(in super::super) fn matching_band"),
        "original pub(super) impl method should preserve its pre-split horizon; got:\n{profile_src}"
    );
    compile_check(tmp.path(), "scope");
}

#[test]
fn original_pub_super_fields_and_methods_remain_parent_visible() {
    let src = r#"pub(super) struct External {
    pub(super) value: u32,
}

impl External {
    pub(super) fn new(value: u32) -> Self {
        Self { value }
    }
}

pub struct Vis;

impl Vis {
    pub fn local() -> u32 {
        External::new(1).value
    }
}
"#;
    let (tmp, _facade, sub_dir) = split_in_tempdir("vis", src);
    let external_src = fs::read_to_string(sub_dir.join("external.rs")).expect("external.rs");
    assert!(
        external_src.contains("pub(in super::super) value")
            && external_src.contains("pub(in super::super) fn new"),
        "existing pub(super) visibilities should rebase to the original parent scope; got:\n{external_src}"
    );

    fs::write(
        tmp.path().join("lib.rs"),
        r#"#![allow(warnings)]
pub mod vis;

pub fn parent_can_still_access_original_pub_super() -> u32 {
    vis::External::new(2).value
}
"#,
    )
    .expect("write lib.rs");
    compile_existing_lib(tmp.path());
}

#[test]
fn body_super_paths_inside_macros_are_rebased_for_subfiles() {
    let src = r#"pub fn collect() -> Vec<u32> {
    vec![super::outer(), super::outer()]
}
"#;
    let (tmp, _facade, sub_dir) = split_in_tempdir("macro_super", src);
    let collect_src = fs::read_dir(&sub_dir)
        .expect("read subdir")
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "rs"))
        .find_map(|path| {
            let src = fs::read_to_string(path).ok()?;
            src.contains("fn collect").then_some(src)
        })
        .expect("generated file containing collect");
    assert!(
        collect_src.contains("vec![super::super::outer(), super::super::outer()]"),
        "macro token paths should be rebased from the generated subfile; got:\n{collect_src}"
    );

    fs::write(
        tmp.path().join("lib.rs"),
        r#"#![allow(warnings)]
fn outer() -> u32 { 1 }
pub mod macro_super;
"#,
    )
    .expect("write lib.rs");
    compile_existing_lib(tmp.path());
}

#[test]
fn repr_c_data_items_stay_in_facade_for_source_scanning_codegen() {
    let src = r#"#[repr(C)]
enum ShaderInputIndex {
    Vertices = 0,
    ViewportSize = 1,
}

#[repr(C)]
pub struct ShaderVertex {
    pub x: f32,
    pub y: f32,
}

pub struct Renderer;

impl Renderer {
    pub fn binding() -> u32 { ShaderInputIndex::Vertices as u32 }
}

pub struct Widget;
"#;
    let (tmp, facade, sub_dir) = split_in_tempdir("shader", src);

    let facade_src = fs::read_to_string(&facade).expect("read facade");
    assert!(
        facade_src.contains("enum ShaderInputIndex")
            && facade_src.contains("pub struct ShaderVertex"),
        "repr(C) data declarations should stay in the facade for codegen tools; got:\n{facade_src}"
    );
    let renderer_src = fs::read_to_string(sub_dir.join("renderer.rs")).expect("renderer.rs");
    assert!(
        renderer_src.contains("use super::ShaderInputIndex;"),
        "generated buckets should import facade-pinned repr(C) items; got:\n{renderer_src}"
    );
    compile_check(tmp.path(), "shader");
}

#[test]
fn recursive_child_split_imports_private_helpers_from_parent_generated_siblings() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let workflows = tmp.path().join("workflows.rs");
    let workflows_dir = tmp.path().join("workflows");
    fs::create_dir_all(&workflows_dir).expect("mkdir workflows");
    fs::write(
        workflows_dir.join("child.rs"),
        r#"use super::*;

pub struct Runner;

impl Runner {
    pub fn run() -> u32 { shared_value() }
}

pub struct Widget;
"#,
    )
    .expect("write child.rs");
    fs::write(
        &workflows,
        r#"mod child;

pub struct Api;

impl Api {
    pub fn value() -> u32 { shared_value() }
}

fn shared_value() -> u32 { 7 }
"#,
    )
    .expect("write workflows.rs");

    let opts = SplitOptions {
        use_tokensave: false,
        llm: None,
        write: Some(WriteOptions {
            force: false,
            recursive_max_lines: Some(0),
        }),
    };
    run_split(&workflows, opts.clone()).expect("split parent workflows.rs");
    run_split(&workflows_dir.join("child.rs"), opts).expect("split child.rs");

    let runner_src =
        fs::read_to_string(workflows_dir.join("child").join("runner.rs")).expect("runner.rs");
    assert!(
        runner_src.contains("use super::super::misc::shared_value;"),
        "child subfile should import the parent generated helper explicitly; got:\n{runner_src}"
    );
    compile_check(tmp.path(), "workflows");
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
fn splits_only_file_in_directory_in_place() {
    let src = r#"#![allow(dead_code)]

pub struct Alpha;
pub struct Beta;
pub struct Gamma;

pub fn value() -> u32 {
    42
}
"#;
    let tmp = tempfile::tempdir().expect("tempdir");
    let file = tmp.path().join("widget.rs");
    fs::write(&file, src).expect("write widget.rs");

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
    .expect("split widget.rs");

    assert!(
        tmp.path().join("types.rs").exists(),
        "types.rs should be generated beside the only file in the directory"
    );
    assert!(
        !tmp.path().join("widget").exists(),
        "single-file directory split should not create a widget/ subdirectory"
    );
    let facade_src = fs::read_to_string(&file).expect("read generated widget.rs");
    assert!(
        facade_src.contains("#[path = \"types.rs\"]\nmod types;"),
        "non-root in-place facade should path-attribute sibling modules; got:\n{facade_src}"
    );

    assert_parses(&file);
    assert_parses(&tmp.path().join("types.rs"));
    compile_check(tmp.path(), "widget");
}

#[test]
fn proc_macro_entrypoints_stay_at_crate_root_with_child_impls() {
    let src = r#"extern crate proc_macro;

use proc_macro::TokenStream;

#[proc_macro_derive(Hello, attributes(hello))]
pub fn derive_hello(input: TokenStream) -> TokenStream {
    expand_hello(input)
}

fn expand_hello(input: TokenStream) -> TokenStream {
    input
}

struct Helper;
"#;
    let tmp = tempfile::tempdir().expect("tempdir");
    let file = tmp.path().join("lib.rs");
    fs::write(&file, src).expect("write proc macro lib.rs");

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
    .expect("split proc macro lib.rs");

    let facade_src = fs::read_to_string(&file).expect("read generated lib.rs");
    assert!(
        facade_src.contains("proc_macro_derive")
            && facade_src.contains("pub fn derive_hello")
            && facade_src.contains("__r2factor_derive_hello_impl"),
        "proc macro export should remain as a crate-root wrapper; got:\n{facade_src}"
    );

    let generated_children = fs::read_dir(tmp.path())
        .expect("read tempdir")
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| {
            path.extension().and_then(|ext| ext.to_str()) == Some("rs")
                && path.file_name().and_then(|name| name.to_str()) != Some("lib.rs")
        })
        .collect::<Vec<_>>();
    assert!(
        generated_children.iter().any(|path| {
            let src = fs::read_to_string(path).expect("read generated child");
            src.contains("__r2factor_derive_hello_impl") && !src.contains("proc_macro_derive")
        }),
        "a generated child should contain the plain proc macro implementation; children: {generated_children:?}"
    );

    assert_parses(&file);
    for path in generated_children {
        assert_parses(&path);
    }
    compile_proc_macro_root(&file);
}

#[test]
fn existing_same_stem_directory_keeps_subdirectory_split() {
    let src = r#"#![allow(dead_code)]

pub struct Alpha;
pub struct Beta;
pub struct Gamma;
"#;
    let tmp = tempfile::tempdir().expect("tempdir");
    let file = tmp.path().join("widget.rs");
    fs::write(&file, src).expect("write widget.rs");
    fs::create_dir_all(tmp.path().join("widget")).expect("mkdir widget");

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
    .expect("split widget.rs");

    assert!(
        tmp.path().join("widget").join("types.rs").exists(),
        "existing same-stem directory should remain the split target"
    );
    let facade_src = fs::read_to_string(&file).expect("read generated widget.rs");
    assert!(
        facade_src.contains("mod types;") && !facade_src.contains("#[path = \"types.rs\"]"),
        "facade should use normal child-module lookup for existing widget/ target dir; got:\n{facade_src}"
    );
}

#[test]
fn isolated_lib_subfile_imports_external_module_root_paths() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let file = tmp.path().join("lib.rs");
    fs::write(
        tmp.path().join("constants.rs"),
        "pub const MAX: usize = 8;\n",
    )
    .expect("write constants.rs");
    fs::write(
        &file,
        r#"pub mod constants;

pub struct Plugin;

impl Plugin {
    pub fn max() -> usize {
        constants::MAX
    }
}

pub struct Other;
"#,
    )
    .expect("write lib.rs");

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

    let plugin_src =
        fs::read_to_string(tmp.path().join("lib").join("plugin.rs")).expect("read plugin.rs");
    assert!(
        plugin_src.contains("use super::constants;")
            || plugin_src.contains("super::constants::MAX"),
        "subfile should resolve external module-root path; got:\n{plugin_src}"
    );
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
fn splits_main_rs_with_path_attributed_child_modules() {
    let src = r#"
pub struct Widget {
    value: u32,
}

impl Widget {
    pub fn new() -> Self {
        Self { value: 7 }
    }
}

fn describe(widget: &Widget) -> u32 {
    widget.value
}

fn main() {
    let widget = Widget::new();
    assert_eq!(describe(&widget), 7);
}
"#;

    let tmp = tempfile::tempdir().expect("tempdir");
    let file = tmp.path().join("main.rs");
    fs::write(&file, src).expect("write main.rs");

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
    .expect("split main.rs");

    let facade_src = fs::read_to_string(&file).expect("read generated main.rs");
    assert!(
        facade_src.contains("mod widget;") && !facade_src.contains("#[path = \"main/"),
        "single-file main.rs split should use sibling modules; got:\n{facade_src}"
    );
    compile_binary_root(&file);
}

#[test]
fn splits_bin_crate_root_with_path_attributed_child_modules() {
    let src = r#"
pub struct Widget {
    value: u32,
}

fn describe(widget: &Widget) -> u32 {
    widget.value
}

fn main() {
    let widget = Widget { value: 7 };
    assert_eq!(describe(&widget), 7);
}
"#;

    let tmp = tempfile::tempdir().expect("tempdir");
    let bin_dir = tmp.path().join("bin");
    fs::create_dir_all(&bin_dir).expect("mkdir bin");
    let file = bin_dir.join("tool.rs");
    fs::write(&file, src).expect("write bin/tool.rs");

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
    .expect("split bin/tool.rs");

    let facade_src = fs::read_to_string(&file).expect("read generated bin/tool.rs");
    assert!(
        facade_src.contains("mod types;")
            && facade_src.contains("mod widget;")
            && !facade_src.contains("#[path = \"tool/"),
        "single-file bin crate root split should use sibling modules; got:\n{facade_src}"
    );
    compile_binary_root(&file);
}

#[test]
fn splits_bench_crate_root_keeps_main_in_facade() {
    let src = r#"
pub struct BenchmarkCase {
    value: u32,
}

fn run_case(case: &BenchmarkCase) -> u32 {
    case.value + helper()
}

fn helper() -> u32 {
    1
}

fn main() {
    let case = BenchmarkCase { value: 41 };
    assert_eq!(run_case(&case), 42);
}
"#;

    let tmp = tempfile::tempdir().expect("tempdir");
    let bench_dir = tmp.path().join("bench");
    fs::create_dir_all(&bench_dir).expect("mkdir bench");
    let file = bench_dir.join("benchmark.rs");
    fs::write(&file, src).expect("write bench/benchmark.rs");

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
    .expect("split bench/benchmark.rs");

    let facade_src = fs::read_to_string(&file).expect("read generated bench/benchmark.rs");
    assert!(
        facade_src.contains("fn main()"),
        "bench root main must remain at crate root; got:\n{facade_src}"
    );
    compile_binary_root(&file);
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
fn external_mod_decls_stay_at_facade_scope() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let module_dir = tmp.path().join("demo");
    fs::create_dir_all(&module_dir).expect("mkdir demo");
    let file = module_dir.join("mod.rs");
    fs::write(
        module_dir.join("functions.rs"),
        "pub fn answer() -> u32 { 42 }\n",
    )
    .expect("write functions.rs");
    fs::write(
        &file,
        r#"pub mod functions;
pub use functions::answer;

pub struct Foo;
pub struct Bar;
pub struct Baz;
"#,
    )
    .expect("write mod.rs");

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

    let facade_src = fs::read_to_string(&file).expect("read generated mod.rs");
    assert!(
        facade_src.contains("pub mod functions;"),
        "external module declaration should be preserved; got:\n{facade_src}"
    );
    assert!(
        !facade_src.contains("\nmod functions;"),
        "existing functions.rs should not be redeclared as a generated sibling; got:\n{facade_src}"
    );
    compile_check(tmp.path(), "demo");
}

#[test]
fn external_mod_decl_name_collision_preserves_existing_module_file() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let module_dir = tmp.path().join("demo");
    fs::create_dir_all(&module_dir).expect("mkdir demo");
    let file = module_dir.join("mod.rs");
    fs::write(
        module_dir.join("functions.rs"),
        "pub struct External;\npub fn value() -> usize { 41 }\n",
    )
    .expect("write functions.rs");
    fs::write(
        &file,
        r#"pub mod functions;
pub use functions::External;

pub struct FunctionsConfig {
    pub value: usize,
}

pub fn functions_value() -> usize {
    functions::value() + FunctionsConfig { value: 1 }.value
}

pub struct Other;
"#,
    )
    .expect("write mod.rs");

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
    .expect("split mod.rs");

    assert!(
        module_dir.join("functions.rs").exists(),
        "existing external module file must not be removed"
    );
    let facade_src = fs::read_to_string(&file).expect("read generated mod.rs");
    assert!(
        facade_src.contains("pub mod functions;"),
        "external module declaration should be preserved; got:\n{facade_src}"
    );
    assert!(
        !facade_src.contains("\nmod functions;\n"),
        "facade must not redeclare the external module as a generated bucket; got:\n{facade_src}"
    );
    let generated = fs::read_to_string(module_dir.join("types.rs")).expect("read generated bucket");
    assert!(
        generated.contains("super::functions::value()"),
        "moved bare module path should be rebased; got:\n{generated}"
    );
    compile_check(tmp.path(), "demo");
}

#[test]
fn local_module_use_roots_rebase_for_generated_subfiles() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let module_dir = tmp.path().join("demo");
    fs::create_dir_all(&module_dir).expect("mkdir demo");
    let file = module_dir.join("mod.rs");
    fs::write(
        module_dir.join("filters.rs"),
        "pub(crate) struct XtcFilters;\nimpl XtcFilters { pub(crate) fn new() -> Self { Self } }\n",
    )
    .expect("write filters.rs");
    fs::write(
        &file,
        r#"mod filters;
use filters::XtcFilters;

pub struct Plugin;

impl Plugin {
    pub fn build() -> XtcFilters {
        use filters::XtcFilters as LocalFilters;
        let _ = LocalFilters::new();
        XtcFilters::new()
    }
}

pub struct Widget;
"#,
    )
    .expect("write mod.rs");

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

    let plugin_src = fs::read_to_string(module_dir.join("plugin.rs")).expect("read plugin.rs");
    assert!(
        plugin_src.contains("use super::filters::XtcFilters;"),
        "local module-root imports must be rebased for subfiles; got:\n{plugin_src}"
    );
    assert!(
        plugin_src.contains("use super::filters::XtcFilters as LocalFilters;"),
        "nested local module-root imports must be rebased for subfiles; got:\n{plugin_src}"
    );
    compile_check(tmp.path(), "demo");
}

#[test]
fn local_module_root_import_skips_existing_same_name_item_import() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let module_dir = tmp.path().join("demo");
    fs::create_dir_all(&module_dir).expect("mkdir demo");
    let file = module_dir.join("mod.rs");
    fs::write(
        module_dir.join("argmin.rs"),
        "pub(crate) fn argmin(values: &[u32]) -> usize { values.iter().position(|v| *v == 0).unwrap_or(0) }\n",
    )
    .expect("write argmin.rs");
    fs::write(
        &file,
        r#"mod argmin;
pub(crate) use argmin::argmin;

pub struct DifferentialEvolution;

impl DifferentialEvolution {
    pub fn best(values: &[u32]) -> usize {
        argmin(values)
    }
}

pub struct Other;
"#,
    )
    .expect("write mod.rs");

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

    let generated =
        fs::read_to_string(module_dir.join("differential_evolution.rs")).expect("read generated");
    assert!(
        generated.contains("use super::argmin::argmin;")
            || generated.contains("pub(crate) use super::argmin::argmin;"),
        "generated bucket should import the argmin function; got:\n{generated}"
    );
    assert!(
        !generated.contains("\nuse super::argmin;\n"),
        "generated bucket must not also import the argmin module under the same value name; got:\n{generated}"
    );
    compile_check(tmp.path(), "demo");
}

#[test]
fn cfg_attrs_stay_attached_to_external_mod_decls() {
    let src = r#"#[cfg(feature = "onnx")]
mod ml_inference;

pub struct Plugin;
pub struct Widget;
"#;
    let (tmp, facade, _sub_dir) = split_in_tempdir("demo", src);

    let facade_src = fs::read_to_string(&facade).expect("read facade");
    assert!(
        facade_src.contains("#[cfg(feature = \"onnx\")]\nmod ml_inference;"),
        "cfg attr should stay attached to the external mod declaration; got:\n{facade_src}"
    );
    assert!(
        !facade_src.contains("\nmod ml_inference;\nmod"),
        "external cfg-gated module should not also be emitted as an unconditional generated module; got:\n{facade_src}"
    );
    compile_check(tmp.path(), "demo");
}

#[test]
fn cfg_attrs_stay_attached_to_external_module_root_imports() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let module_dir = tmp.path().join("demo");
    fs::create_dir_all(&module_dir).expect("mkdir demo");
    let file = module_dir.join("mod.rs");
    fs::write(module_dir.join("ml_inference.rs"), "pub fn run() {}\n")
        .expect("write ml_inference.rs");
    fs::write(
        &file,
        r#"#[cfg(feature = "onnx")]
mod ml_inference;

pub struct Plugin;

impl Plugin {
    #[cfg(feature = "onnx")]
    pub fn run() {
        ml_inference::run();
    }
}

pub struct Widget;
"#,
    )
    .expect("write mod.rs");

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

    let plugin_src = fs::read_to_string(module_dir.join("plugin.rs")).expect("read plugin.rs");
    let cfg_pos = plugin_src.find("cfg").expect("generated cfg attr");
    let import_pos = plugin_src
        .find("use super::ml_inference;")
        .expect("generated ml_inference import");
    assert!(
        cfg_pos < import_pos,
        "cfg-gated external module import should stay cfg-gated; got:\n{plugin_src}"
    );
    compile_check(tmp.path(), "demo");
}

#[test]
fn generated_bucket_renames_around_external_mod_file() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let module_dir = tmp.path().join("demo");
    fs::create_dir_all(&module_dir).expect("mkdir demo");
    let file = module_dir.join("mod.rs");
    fs::write(module_dir.join("types.rs"), "pub struct ExternalType;\n")
        .expect("write existing types.rs");
    fs::write(
        &file,
        r#"mod types;
pub use types::ExternalType;

pub struct Types;

pub struct Widget;
"#,
    )
    .expect("write mod.rs");

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

    assert_eq!(
        fs::read_to_string(module_dir.join("types.rs")).expect("read existing types.rs"),
        "pub struct ExternalType;\n",
        "external types.rs must not be overwritten by a generated types bucket"
    );
    assert!(
        module_dir.join("types_mod.rs").exists(),
        "generated bucket should be renamed away from the external module file"
    );
    compile_check(tmp.path(), "demo");
}

#[test]
fn generated_bucket_renames_around_external_use_root() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let module_dir = tmp.path().join("demo");
    fs::create_dir_all(&module_dir).expect("mkdir demo");
    let file = module_dir.join("mod.rs");
    fs::write(
        &file,
        r#"pub struct Coreaudio;

impl Coreaudio {
    pub fn output_device_id() -> u32 {
        coreaudio::audio_unit::macos_helpers::get_device_id_from_name()
    }
}

pub struct Widget;
"#,
    )
    .expect("write mod.rs");

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

    let generated_path = module_dir.join("coreaudio_mod.rs");
    assert!(
        generated_path.exists(),
        "bucket should be renamed away from external use root"
    );
    let generated = fs::read_to_string(generated_path).expect("read generated");
    assert!(
        generated.contains("coreaudio::audio_unit::macos_helpers"),
        "external crate path must not be rebased to super::coreaudio; got:\n{generated}"
    );
    assert!(
        !generated.contains("super::coreaudio::audio_unit"),
        "external crate path was incorrectly treated as a local module root; got:\n{generated}"
    );
}

#[test]
fn macro_defined_function_names_import_for_attribute_string_refs() {
    let src = r#"macro_rules! define_defaults {
    ($($body:tt)*) => { $($body)* };
}

define_defaults! {
    fn default_gain() -> u32 { 1 }
}

pub struct Settings {
    #[doc = "default_gain"]
    pub value: u32,
}

pub struct Widget;
"#;
    let (_tmp, _facade, sub_dir) = split_in_tempdir("macro_defaults", src);
    let settings = fs::read_to_string(sub_dir.join("types.rs")).expect("types.rs");
    assert!(
        settings.contains("use super::default_gain;"),
        "attribute-only macro-defined function ref should import facade function; got:\n{settings}"
    );
}

#[test]
fn macro_rules_definition_stays_in_facade_with_invocation() {
    let src = r#"macro_rules! impl_widget {
    () => {
        impl Widget {
            pub fn generated(&self) -> u32 { 7 }
        }
    };
}

pub struct Widget;

impl_widget!();

pub fn render_widget(widget: &Widget) -> u32 {
    widget.generated()
}

pub fn render_other(widget: &Widget) -> u32 {
    render_widget(widget)
}
"#;
    let (tmp, facade, sub_dir) = split_in_tempdir("demo", src);
    let facade_src = fs::read_to_string(&facade).expect("read facade");
    assert!(
        facade_src.contains("macro_rules! impl_widget"),
        "macro_rules definition should stay before facade invocation; got:\n{facade_src}"
    );
    let generated = fs::read_to_string(sub_dir.join("render.rs")).expect("read render.rs");
    assert!(
        !generated.contains("super::impl_widget"),
        "child should not import macro internals as items; got:\n{generated}"
    );
    compile_check(tmp.path(), "demo");
}

#[test]
fn inline_module_with_inner_attr_keeps_wrapper_when_moved_to_misc() {
    let src = r#"mod security {
    #![allow(non_upper_case_globals)]

    pub const errSecSuccess: i32 = 0;
}

pub fn read_security() -> i32 {
    security::errSecSuccess
}

pub fn read_other() -> i32 {
    read_security()
}
"#;
    let (tmp, _facade, sub_dir) = split_in_tempdir("demo", src);
    let misc = fs::read_to_string(sub_dir.join("misc.rs")).expect("read misc.rs");
    assert!(
        misc.contains("mod security {"),
        "moved inline module should keep its wrapper; got:\n{misc}"
    );
    assert!(
        misc.contains("#![allow(non_upper_case_globals)]"),
        "moved inline module should keep inner attrs; got:\n{misc}"
    );
    compile_check(tmp.path(), "demo");
}

#[test]
fn foreign_mod_bucket_is_reexported_from_facade() {
    let src = r#"#[link(name = "c")]
unsafe extern "C" {
    pub(in crate::demo) fn abs(input: i32) -> i32;
}

pub fn call_abs(input: i32) -> i32 {
    unsafe { abs(input) }
}
"#;
    let (tmp, facade, sub_dir) = split_in_tempdir("demo", src);
    let facade_src = fs::read_to_string(&facade).expect("read facade");
    assert!(
        facade_src.contains("pub use misc::*;"),
        "foreign mod bucket should be re-exported; got:\n{facade_src}"
    );
    let misc = fs::read_to_string(sub_dir.join("misc.rs")).expect("read misc.rs");
    assert!(
        misc.contains("unsafe extern"),
        "foreign block should stay in generated bucket; got:\n{misc}"
    );
    compile_check(tmp.path(), "demo");
}

#[test]
fn custom_crate_root_preserves_existing_super_glob_trait_scope() {
    let tmp = tempfile::tempdir().expect("create tempdir");
    let src_dir = tmp.path().join("src");
    fs::create_dir_all(&src_dir).expect("mkdir src");
    fs::write(
        tmp.path().join("Cargo.toml"),
        "[package]\nname = \"custom-root-glob\"\nversion = \"0.1.0\"\n\n[lib]\npath = \"src/custom.rs\"\n",
    )
    .expect("write manifest");
    fs::write(
        src_dir.join("sibling.rs"),
        "use super::*;\n\npub fn exercise() {\n    let result: Result<(), ()> = Ok(());\n    result.unwrap_oob();\n}\n",
    )
    .expect("write sibling.rs");
    let original = src_dir.join("custom.rs");
    fs::write(
        &original,
        "mod sibling;\n\ntrait CapacityResultExt {\n    fn unwrap_oob(self);\n}\n\nimpl<T> CapacityResultExt for Result<(), T> {\n    fn unwrap_oob(self) {}\n}\n",
    )
    .expect("write custom.rs");

    run_split(
        &original,
        SplitOptions {
            use_tokensave: false,
            llm: None,
            write: Some(WriteOptions {
                force: false,
                recursive_max_lines: Some(0),
            }),
        },
    )
    .expect("split custom crate root");

    let facade = fs::read_to_string(&original).expect("read custom facade");
    assert!(
        facade.contains("use capacity_result_ext::CapacityResultExt;"),
        "existing sibling glob should keep the moved trait in facade scope; got:\n{facade}"
    );
    compile_lib_root(&original);
}

#[test]
fn extracted_platform_child_keeps_unix_traits_and_foreign_items() {
    let src = r#"
use std::ffi::OsString;
use std::fs::File;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::{OsStrExt, OsStringExt};

unsafe extern "C" {
    fn abs(input: i32) -> i32;
}

pub struct Child;

impl Child {
    pub fn run(&self, program: OsString, file: &File) -> usize {
        let path_len = program.as_os_str().as_bytes().len();
        let owned_len = program.into_vec().len();
        let _ = unsafe { File::from_raw_fd(file.as_raw_fd()) };
        path_len + owned_len + unsafe { abs(1) as usize }
    }
}
"#;
    let (tmp, _facade, sub_dir) = split_in_tempdir("darwin", src);
    let child = fs::read_to_string(sub_dir.join("child.rs")).expect("read child.rs");
    assert!(
        child.contains("use super::misc::abs;"),
        "child should import the foreign declaration from its sibling bucket; got:\n{child}"
    );
    assert!(
        child.contains("use std::os::fd::{AsRawFd, FromRawFd};")
            && child.contains("use std::os::unix::ffi::{OsStrExt, OsStringExt};"),
        "child should retain Unix extension-trait imports; got:\n{child}"
    );
    compile_check(tmp.path(), "darwin");
}

#[test]
fn nested_tests_directory_facade_uses_path_for_in_place_children() {
    let tmp = tempfile::tempdir().expect("create tempdir");
    let source_dir = tmp.path().join("src/line_break/tests");
    fs::create_dir_all(&source_dir).expect("mkdir nested tests directory");
    let original = source_dir.join("misc.rs");
    fs::write(
        &original,
        r#"pub struct Misc;

pub fn first() -> usize { 1 }
pub fn second() -> usize { 2 }

#[cfg(test)]
mod tests {
    #[test]
    fn smoke() {
        assert_eq!(super::first() + super::second(), 3);
        let _ = super::Misc;
    }
}
"#,
    )
    .expect("write nested source");

    run_split(
        &original,
        SplitOptions {
            use_tokensave: false,
            llm: None,
            write: Some(WriteOptions {
                force: false,
                recursive_max_lines: Some(0),
            }),
        },
    )
    .expect("split nested tests source");

    let facade = fs::read_to_string(&original).expect("read nested facade");
    assert!(
        facade.contains("#[path = \"tests.rs\"]\nmod tests;"),
        "nested in-place children need an explicit path from misc.rs: {facade}"
    );
    assert!(source_dir.join("tests.rs").exists());

    let lib = tmp.path().join("src/lib.rs");
    fs::write(
        &lib,
        "#![allow(warnings)]\n#[path = \"line_break/tests/misc.rs\"]\npub mod misc;\n",
    )
    .expect("write test crate root");
    let out_dir = tmp.path().join("rustc-test-out");
    fs::create_dir_all(&out_dir).expect("mkdir rustc test output");
    let output = Command::new("rustc")
        .arg("--edition=2024")
        .arg("--test")
        .arg("-Awarnings")
        .arg("--out-dir")
        .arg(&out_dir)
        .arg(&lib)
        .output()
        .expect("invoke rustc --test");
    assert!(
        output.status.success(),
        "nested in-place split failed to compile as a test crate: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn item_macro_invocations_stay_in_facade() {
    let src = r#"use std::cell::Cell;

thread_local! {
    static HIT: Cell<u32> = Cell::new(7);
}

pub fn read_hit() -> u32 {
    HIT.with(|v| v.get())
}

pub struct Widget;

impl Widget {
    pub fn read(&self) -> u32 {
        read_hit()
    }
}
"#;
    let (tmp, facade, sub_dir) = split_in_tempdir("demo", src);

    let facade_src = fs::read_to_string(&facade).expect("read facade");
    assert!(
        facade_src.contains("thread_local!"),
        "thread_local! invocation should remain in the facade; got:\n{facade_src}"
    );
    let generated = fs::read_to_string(sub_dir.join("misc.rs"))
        .or_else(|_| fs::read_to_string(sub_dir.join("widget.rs")))
        .expect("read generated subfile");
    assert!(
        generated.contains("use super::HIT;"),
        "subfile using macro-defined static should import it from the facade; got:\n{generated}"
    );
    compile_check(tmp.path(), "demo");
}

#[test]
fn include_macros_stay_in_facade_scope() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let file = tmp.path().join("demo.rs");
    fs::write(
        tmp.path().join("generated_impl.inc.rs"),
        "impl Demo { fn generated(&self) -> u32 { 9 } }\n",
    )
    .expect("write include");
    fs::write(
        &file,
        r#"include!("generated_impl.inc.rs");

pub struct Demo;

impl Demo {
    pub fn value(&self) -> u32 {
        self.generated()
    }
}

pub struct Foo;
pub struct Bar;
"#,
    )
    .expect("write source");

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
    .expect("split demo.rs");

    let facade_src = fs::read_to_string(&file).expect("read facade");
    assert!(
        facade_src.contains("include!(\"generated_impl.inc.rs\");"),
        "include! should remain in the facade's scope; got:\n{facade_src}"
    );
    compile_check(tmp.path(), "demo");
}

#[test]
fn included_existing_rs_file_is_not_also_preserved_as_module() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let file = tmp.path().join("demo.rs");
    fs::write(
        tmp.path().join("handle.rs"),
        "impl Demo { pub fn handled(&self) -> u32 { 7 } }\n",
    )
    .expect("write include target");
    fs::write(
        &file,
        r#"include!("handle.rs");

pub struct Demo;
pub struct Widget;
"#,
    )
    .expect("write source");

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
    .expect("split demo.rs");

    let facade_src = fs::read_to_string(&file).expect("read facade");
    assert!(
        facade_src.contains("include!(\"handle.rs\");"),
        "include! for existing handle.rs should remain; got:\n{facade_src}"
    );
    assert!(
        !facade_src.contains("mod handle;"),
        "included existing handle.rs should not also be declared as a module; got:\n{facade_src}"
    );
    compile_check(tmp.path(), "demo");
}

#[test]
fn generated_child_imports_item_from_included_facade_file() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let file = tmp.path().join("demo.rs");
    fs::write(
        tmp.path().join("render.rs"),
        "struct PendingGeometrySave;\n",
    )
    .expect("write include target");
    fs::write(
        &file,
        r#"include!("render.rs");

pub struct PlayerView {
    pending: Option<PendingGeometrySave>,
}

pub fn helper() -> u32 { 1 }
pub fn helper_two() -> u32 { helper() + 1 }
"#,
    )
    .expect("write source");

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
    .expect("split demo.rs");

    let generated =
        fs::read_to_string(tmp.path().join("demo").join("types.rs")).expect("read types.rs");
    assert!(
        generated.contains("use super::PendingGeometrySave;"),
        "child should import type defined by include! in facade; got:\n{generated}"
    );
    compile_check(tmp.path(), "demo");
}

#[test]
fn generated_child_does_not_import_name_it_defines() {
    let src = r#"pub fn duplicate() -> u32 { 1 }

pub fn caller() -> u32 { duplicate() }

pub struct Widget;

impl Widget {
    pub fn read(&self) -> u32 { duplicate() }
}
"#;
    let (_tmp, _facade, sub_dir) = split_in_tempdir("demo", src);
    let generated = fs::read_to_string(sub_dir.join("duplicate.rs"))
        .or_else(|_| fs::read_to_string(sub_dir.join("misc.rs")))
        .expect("read generated subfile");
    assert!(
        !generated.contains("use super::duplicate;"),
        "child defining duplicate() should not import duplicate; got:\n{generated}"
    );
}

#[test]
fn generated_child_imports_sibling_extension_trait_for_method_call() {
    let src = r#"pub struct Widget;

pub trait WidgetExt {
    fn tweak(&self) -> u32;
}

impl WidgetExt for Widget {
    fn tweak(&self) -> u32 { 7 }
}

pub fn render_widget(widget: &Widget) -> u32 {
    widget.tweak()
}

pub fn render_other(widget: &Widget) -> u32 {
    render_widget(widget)
}
"#;
    let (tmp, _facade, sub_dir) = split_in_tempdir("demo", src);
    let generated = fs::read_to_string(sub_dir.join("render.rs")).expect("read render.rs");
    assert!(
        generated.contains("WidgetExt"),
        "child calling extension method should import the trait; got:\n{generated}"
    );
    compile_check(tmp.path(), "demo");
}

#[test]
fn generated_child_promotes_private_sibling_extension_trait_for_method_call() {
    let src = r#"pub struct Widget;

trait WidgetExt {
    fn tweak(&self) -> u32;
}

impl WidgetExt for Widget {
    fn tweak(&self) -> u32 { 7 }
}

pub fn render_widget(widget: &Widget) -> u32 {
    widget.tweak()
}

pub fn render_other(widget: &Widget) -> u32 {
    render_widget(widget)
}
"#;
    let (tmp, _facade, sub_dir) = split_in_tempdir("demo", src);
    let generated = fs::read_to_string(sub_dir.join("render.rs")).expect("read render.rs");
    assert!(
        generated.contains("WidgetExt"),
        "child calling extension method should import the trait; got:\n{generated}"
    );
    let promoted = fs::read_to_string(sub_dir.join("widget_ext.rs")).expect("read widget_ext.rs");
    assert!(
        promoted.contains("pub(super) trait WidgetExt"),
        "private extension trait should be promoted for sibling imports; got:\n{promoted}"
    );
    compile_check(tmp.path(), "demo");
}

#[test]
fn include_macro_targeting_generated_bucket_is_not_duplicated_in_facade() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let file = tmp.path().join("demo.rs");
    fs::write(
        &file,
        r#"include!("misc.rs");

pub fn misc_value() -> u32 { 1 }
pub struct Widget;
"#,
    )
    .expect("write source");

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
    .expect("split demo.rs");

    let facade_src = fs::read_to_string(&file).expect("read facade");
    assert!(
        !facade_src.contains("include!(\"misc.rs\");"),
        "include! targeting generated misc.rs should not be preserved; got:\n{facade_src}"
    );
    assert!(
        facade_src.contains("mod misc;"),
        "generated misc module should still be declared; got:\n{facade_src}"
    );
    compile_check(tmp.path(), "demo");
}

#[test]
fn mod_rs_include_macro_targeting_generated_bucket_is_not_duplicated_in_facade() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let module_dir = tmp.path().join("ui");
    fs::create_dir_all(&module_dir).expect("mkdir ui");
    let file = module_dir.join("mod.rs");
    fs::write(
        &file,
        r#"include!("handle.rs");

pub struct Ui;

impl Ui {
    pub fn handle(&self) -> u32 { handle_value() }
}

pub fn handle_value() -> u32 { 1 }
pub fn handle_other() -> u32 { handle_value() }
pub struct Widget;
"#,
    )
    .expect("write source");

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
    .expect("split ui/mod.rs");

    let facade_src = fs::read_to_string(&file).expect("read facade");
    assert!(
        !facade_src.contains("include!(\"handle.rs\");"),
        "include! targeting generated handle.rs should not be preserved; got:\n{facade_src}"
    );
    assert!(
        facade_src.contains("mod handle;"),
        "generated handle module should still be declared; got:\n{facade_src}"
    );
    let lib_path = tmp.path().join("lib.rs");
    fs::write(&lib_path, "#![allow(warnings)]\npub mod ui;\n").expect("write lib.rs");
    compile_check(tmp.path(), "ui");
}

#[test]
fn crate_visible_items_are_reexported_through_facade() {
    let (tmp, _facade, _sub_dir) = split_in_tempdir(
        "demo",
        r#"pub(crate) fn helper() -> u32 {
    3
}

pub fn call() -> u32 {
    helper()
}

pub struct Foo;
pub struct Bar;
"#,
    );

    let lib_path = tmp.path().join("lib.rs");
    fs::write(
        &lib_path,
        "#![allow(warnings)]\npub mod demo;\npub fn outside() -> u32 { demo::helper() }\n",
    )
    .expect("write lib.rs");
    let out_dir = tmp.path().join("rustc-out-crate-visible");
    fs::create_dir_all(&out_dir).expect("mkdir out");
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
    assert!(
        output.status.success(),
        "crate-visible helper should remain reachable through demo::helper; stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
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
fn split_mod_rs_force_preserves_generated_external_module_facade() {
    let src = r#"mod child;

pub struct Parent;
pub struct Other;
"#;
    let tmp = tempfile::tempdir().expect("tempdir");
    let module_dir = tmp.path().join("foo");
    fs::create_dir_all(&module_dir).expect("mkdir foo");
    let file = module_dir.join("mod.rs");
    fs::write(&file, src).expect("write mod.rs");
    fs::write(
        module_dir.join("child.rs"),
        "// r2factor:facade - do not pass this file back into r2factor\n// Auto-generated by r2factor. Manual edits will be overwritten on next split.\n\npub fn child() -> u32 { 1 }\n",
    )
    .expect("write generated child facade");

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

    assert!(
        module_dir.join("child.rs").exists(),
        "explicit external module child.rs must survive stale generated cleanup"
    );
    let facade_src = fs::read_to_string(&file).expect("read generated mod.rs");
    assert!(
        facade_src.contains("mod child;"),
        "parent facade must keep external child declaration; got:\n{facade_src}"
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

    let parse_file = if tmp.path().join("parse.rs").exists() {
        tmp.path().join("parse.rs")
    } else {
        tmp.path().join("recur").join("parse.rs")
    };
    let parse_src = fs::read_to_string(&parse_file).expect("read recursive facade");
    assert!(
        parse_src.contains("r2factor:facade"),
        "oversized generated parse.rs should be recursively split into a facade; got:\n{parse_src}"
    );
    assert!(
        parse_file.with_extension("rs.bak").exists(),
        "recursive split should back up the oversized generated file"
    );
    compile_check(tmp.path(), "recur");
}

#[test]
fn split_write_directory_splits_all_files_over_line_threshold() {
    let tmp = tempfile::tempdir().expect("tempdir");

    let mut long_a = String::new();
    for i in 0..8 {
        long_a.push_str(&format!(
            "pub fn parse_a_{i}(input: &str) -> usize {{\n    input.len() + {i}\n}}\n\n"
        ));
    }
    let long_a_path = tmp.path().join("long_a.rs");
    fs::write(&long_a_path, long_a).expect("write long_a");

    fs::create_dir_all(tmp.path().join("nested")).expect("mkdir nested");
    let mut long_b = String::new();
    for i in 0..8 {
        long_b.push_str(&format!(
            "pub fn render_b_{i}(input: &str) -> usize {{\n    input.len() + {i}\n}}\n\n"
        ));
    }
    let long_b_path = tmp.path().join("nested").join("long_b.rs");
    fs::write(&long_b_path, long_b).expect("write long_b");

    let short_path = tmp.path().join("short.rs");
    fs::write(&short_path, "pub fn short() -> usize { 1 }\n").expect("write short");

    let output = Command::new(env!("CARGO_BIN_EXE_r2factor"))
        .arg("split")
        .arg("--no-tokensave")
        .arg("--write")
        .arg("--max-lines")
        .arg("10")
        .arg(tmp.path())
        .output()
        .expect("run r2factor split directory");

    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let long_a_src = fs::read_to_string(&long_a_path).expect("read long_a facade");
    assert!(
        long_a_src.contains("r2factor:facade"),
        "long_a.rs should become a facade; got:\n{long_a_src}"
    );
    assert!(
        tmp.path().join("long_a").join("parse.rs").exists()
            || tmp.path().join("long_a").join("parse_a.rs").exists()
    );

    let long_b_src = fs::read_to_string(&long_b_path).expect("read long_b facade");
    assert!(
        long_b_src.contains("r2factor:facade"),
        "nested/long_b.rs should become a facade; got:\n{long_b_src}"
    );
    assert!(
        tmp.path().join("nested").join("render.rs").exists()
            || tmp.path().join("nested").join("render_b.rs").exists()
            || tmp
                .path()
                .join("nested")
                .join("long_b")
                .join("render.rs")
                .exists()
            || tmp
                .path()
                .join("nested")
                .join("long_b")
                .join("render_b.rs")
                .exists()
    );

    let short_src = fs::read_to_string(&short_path).expect("read short");
    assert_eq!(short_src, "pub fn short() -> usize { 1 }\n");
}

#[test]
fn split_write_directory_skips_oversized_non_standalone_fragments() {
    let tmp = tempfile::tempdir().expect("tempdir");

    let mut fragment = String::from("{\n");
    for i in 0..12 {
        fragment.push_str(&format!("    let value_{i} = {i};\n"));
    }
    fragment.push_str("    value_0\n}\n");
    let fragment_path = tmp.path().join("fragment.rs");
    fs::write(&fragment_path, fragment).expect("write fragment");

    let mut long = String::new();
    for i in 0..8 {
        long.push_str(&format!(
            "pub fn parse_ok_{i}(input: &str) -> usize {{\n    input.len() + {i}\n}}\n\n"
        ));
    }
    let long_path = tmp.path().join("long.rs");
    fs::write(&long_path, long).expect("write long");

    let output = Command::new(env!("CARGO_BIN_EXE_r2factor"))
        .arg("split")
        .arg("--no-tokensave")
        .arg("--write")
        .arg("--max-lines")
        .arg("10")
        .arg(tmp.path())
        .output()
        .expect("run r2factor split directory");

    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("cannot split non-standalone Rust source"),
        "stderr should explain skipped fragment:\n{stderr}"
    );
    let fragment_src = fs::read_to_string(&fragment_path).expect("read fragment");
    assert!(
        fragment_src.starts_with("{\n"),
        "fragment should be left untouched; got:\n{fragment_src}"
    );
    let long_src = fs::read_to_string(&long_path).expect("read long");
    assert!(
        long_src.contains("r2factor:facade"),
        "standalone oversized file should still be split; got:\n{long_src}"
    );
}

#[test]
fn split_write_directory_skips_include_targets() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let src_dir = tmp.path().join("src");
    let ui_dir = src_dir.join("ui");
    fs::create_dir_all(&ui_dir).expect("mkdir ui");
    fs::write(
        tmp.path().join("Cargo.toml"),
        r#"[package]
name = "demo"
version = "0.1.0"
edition = "2024"
"#,
    )
    .expect("write Cargo.toml");
    fs::write(src_dir.join("lib.rs"), "pub mod ui;\n").expect("write lib.rs");
    fs::write(
        ui_dir.join("mod.rs"),
        r#"include!("render.rs");

pub struct PlayerView;
"#,
    )
    .expect("write ui mod");

    let mut render = String::new();
    render.push_str("impl PlayerView {\n");
    for i in 0..12 {
        render.push_str(&format!(
            "    pub fn render_part_{i}(&self) -> usize {{ {i} }}\n"
        ));
    }
    render.push_str("}\n");
    fs::write(ui_dir.join("render.rs"), render).expect("write include target");

    let output = Command::new(env!("CARGO_BIN_EXE_r2factor"))
        .arg("split")
        .arg("--no-tokensave")
        .arg("--write")
        .arg("--max-lines")
        .arg("10")
        .arg(&src_dir)
        .output()
        .expect("run r2factor split directory");

    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("referenced by include!"),
        "stderr should explain skipped include target:\n{stderr}"
    );
    let render_src = fs::read_to_string(ui_dir.join("render.rs")).expect("read render.rs");
    assert!(
        !render_src.contains("r2factor:facade"),
        "include! target should not become a module facade; got:\n{render_src}"
    );
    let check = Command::new("cargo")
        .arg("check")
        .current_dir(tmp.path())
        .output()
        .expect("cargo check demo");
    assert!(
        check.status.success(),
        "cargo check failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&check.stdout),
        String::from_utf8_lossy(&check.stderr)
    );
}

#[test]
fn split_write_directory_processes_parent_modules_before_nested_children() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let workflows = tmp.path().join("workflows.rs");
    let workflows_dir = tmp.path().join("workflows");
    fs::create_dir_all(&workflows_dir).expect("mkdir workflows");
    fs::write(
        workflows_dir.join("child.rs"),
        r#"use super::*;

pub struct Runner;

impl Runner {
    pub fn run() -> u32 {
        shared_value()
    }
}

pub struct Widget;

impl Widget {
    pub fn value() -> u32 { 2 }
}
"#,
    )
    .expect("write child.rs");
    fs::write(
        &workflows,
        r#"mod child;

pub struct Api;

impl Api {
    pub fn value() -> u32 {
        shared_value()
    }
}

fn shared_value() -> u32 {
    7
}
"#,
    )
    .expect("write workflows.rs");

    let output = Command::new(env!("CARGO_BIN_EXE_r2factor"))
        .arg("split")
        .arg("--no-tokensave")
        .arg("--write")
        .arg("--max-lines")
        .arg("10")
        .arg(tmp.path())
        .output()
        .expect("run r2factor split directory");

    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let runner_src =
        fs::read_to_string(workflows_dir.join("child").join("runner.rs")).expect("runner.rs");
    assert!(
        runner_src.contains("use super::super::misc::shared_value;"),
        "directory splitting must process workflows.rs before workflows/child.rs; got:\n{runner_src}"
    );
    compile_check(tmp.path(), "workflows");
}

#[test]
fn split_write_directory_reports_cargo_check_for_affected_package() {
    let tmp = tempfile::tempdir().expect("tempdir");
    fs::write(
        tmp.path().join("Cargo.toml"),
        r#"[package]
name = "demo"
version = "0.1.0"
edition = "2024"
"#,
    )
    .expect("write Cargo.toml");
    let src_dir = tmp.path().join("src");
    fs::create_dir_all(&src_dir).expect("mkdir src");
    fs::write(src_dir.join("lib.rs"), "pub mod long_a;\n").expect("write lib.rs");

    let mut long_a = String::new();
    for i in 0..8 {
        long_a.push_str(&format!(
            "pub fn parse_a_{i}(input: &str) -> usize {{\n    input.len() + {i}\n}}\n\n"
        ));
    }
    fs::write(src_dir.join("long_a.rs"), long_a).expect("write long_a");

    let output = Command::new(env!("CARGO_BIN_EXE_r2factor"))
        .arg("split")
        .arg("--no-tokensave")
        .arg("--write")
        .arg("--max-lines")
        .arg("10")
        .arg(&src_dir)
        .output()
        .expect("run r2factor split directory");

    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("[check] cargo check -p demo ok"),
        "stderr:\n{stderr}"
    );
    assert!(
        stderr.contains("[test] cargo test -p demo ok"),
        "stderr should report the test verification:\n{stderr}"
    );
    assert!(
        stderr.contains("[doc] cargo doc --no-deps -p demo ok"),
        "stderr should report the documentation verification:\n{stderr}"
    );
}

#[test]
fn split_write_directory_reports_test_command_when_cargo_check_fails() {
    let tmp = tempfile::tempdir().expect("tempdir");
    fs::write(
        tmp.path().join("Cargo.toml"),
        r#"[package]
name = "demo"
version = "0.1.0"
edition = "2024"
"#,
    )
    .expect("write Cargo.toml");
    let src_dir = tmp.path().join("src");
    fs::create_dir_all(&src_dir).expect("mkdir src");
    fs::write(src_dir.join("lib.rs"), "pub mod long_a;\n").expect("write lib.rs");

    let mut long_a = String::new();
    for i in 0..8 {
        long_a.push_str(&format!(
            "pub fn parse_a_{i}(input: &str) -> usize {{\n    input.len() + missing_symbol_{i}()\n}}\n\n"
        ));
    }
    fs::write(src_dir.join("long_a.rs"), long_a).expect("write long_a");

    let output = Command::new(env!("CARGO_BIN_EXE_r2factor"))
        .arg("split")
        .arg("--no-tokensave")
        .arg("--write")
        .arg("--max-lines")
        .arg("10")
        .arg(&src_dir)
        .output()
        .expect("run r2factor split directory");

    assert!(
        !output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("[check] cargo check -p demo failed"),
        "stderr:\n{stderr}"
    );
    assert!(
        stderr.contains("[check] cargo check -p demo failed"),
        "stderr:\n{stderr}"
    );
    assert!(
        stderr.contains("[verify] bug report ->"),
        "stderr should include rollback bug report path:\n{stderr}"
    );
    assert!(
        !src_dir.join("long_a").exists(),
        "generated split directory should be removed on verification failure"
    );
    let restored = fs::read_to_string(src_dir.join("long_a.rs")).expect("read restored long_a");
    assert!(
        restored.contains("missing_symbol_0()"),
        "facade should be restored to the original source:\n{restored}"
    );
    assert!(
        !restored.contains("pub mod misc;"),
        "restored source should not remain an r2factor facade:\n{restored}"
    );
    let report_dir = tmp.path().join(".r2factor/bug-reports");
    let reports = fs::read_dir(&report_dir)
        .expect("bug report dir")
        .collect::<std::result::Result<Vec<_>, _>>()
        .expect("bug report entries");
    assert_eq!(reports.len(), 1);
    let report = fs::read_to_string(reports[0].path()).expect("read bug report");
    assert!(report.contains("cargo check -p demo"), "{report}");
    assert!(report.contains("cannot find function"), "{report}");
    assert!(report.contains("## Split Tree"), "{report}");
}

#[test]
fn split_write_directory_continues_package_checks_after_failure() {
    let tmp = tempfile::tempdir().expect("tempdir");
    fs::write(
        tmp.path().join("Cargo.toml"),
        r#"[workspace]
members = ["a", "b"]
resolver = "3"
"#,
    )
    .expect("write workspace Cargo.toml");
    for package in ["a", "b"] {
        let package_dir = tmp.path().join(package);
        let src_dir = package_dir.join("src");
        fs::create_dir_all(&src_dir).expect("mkdir package src");
        fs::write(
            package_dir.join("Cargo.toml"),
            format!("[package]\nname = \"{package}\"\nversion = \"0.1.0\"\nedition = \"2024\"\n"),
        )
        .expect("write package manifest");
        fs::write(src_dir.join("lib.rs"), "pub mod long;\n").expect("write lib.rs");
        let mut long = String::new();
        for i in 0..8 {
            long.push_str(&format!(
                "pub fn parse_{package}_{i}(input: &str) -> usize {{\n    input.len() + missing_{package}_{i}()\n}}\n\n"
            ));
        }
        fs::write(src_dir.join("long.rs"), long).expect("write long.rs");
    }

    let output = Command::new(env!("CARGO_BIN_EXE_r2factor"))
        .arg("split")
        .arg("--no-tokensave")
        .arg("--write")
        .arg("--max-lines")
        .arg("10")
        .arg(tmp.path())
        .output()
        .expect("run r2factor split directory");

    assert!(
        !output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("[check] cargo check -p a failed"),
        "stderr:\n{stderr}"
    );
    assert!(
        stderr.contains("[check] cargo check -p b failed"),
        "directory verification should continue after the first package failure:\n{stderr}"
    );
    let report_dir = tmp.path().join(".r2factor/bug-reports");
    let reports = fs::read_dir(&report_dir)
        .expect("bug report dir")
        .collect::<std::result::Result<Vec<_>, _>>()
        .expect("bug report entries");
    assert_eq!(reports.len(), 2);
    assert!(
        !tmp.path().join("a/src/long").exists(),
        "generated split dir for package a should be removed"
    );
    assert!(
        !tmp.path().join("b/src/long").exists(),
        "generated split dir for package b should be removed"
    );
}
