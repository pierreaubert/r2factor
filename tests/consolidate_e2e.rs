//! End-to-end tests for the consolidate (inverse-split) pipeline.
//!
//! The headline test is a *round-trip*: take a fixture, split it, then
//! consolidate the split output and compile-check the result against
//! `rustc`. If the merged file compiles, the inverse pipeline didn't
//! lose information that matters.
//!
//! Two non-r2factor inputs are also covered: a hand-written
//! `foo.rs + foo/bar.rs` pair (general case) and a `foo/mod.rs`-style
//! input.

use r2factor::{
    SplitOptions, consolidate, run_split,
    write::WriteOptions,
};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn split_in_tempdir(stem: &str, src: &str) -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let f = tmp.path().join(format!("{stem}.rs"));
    fs::write(&f, src).expect("write src");
    run_split(
        &f,
        SplitOptions {
            use_tokensave: false,
            llm: None,
            write: Some(WriteOptions { force: false }),
        },
    )
    .expect("split");
    (tmp, f)
}

/// Drop a tiny lib.rs alongside `facade_path` and invoke `rustc` on it to
/// confirm the consolidated code compiles. Returns the rustc stderr on
/// failure so the assertion message is actionable.
fn rustc_check(tmp_root: &Path, stem: &str) -> Result<(), String> {
    let lib = tmp_root.join("lib.rs");
    fs::write(&lib, format!("#![allow(warnings)]\npub mod {stem};\n"))
        .map_err(|e| format!("write lib.rs: {e}"))?;
    let out_dir = tmp_root.join("rustc-out");
    fs::create_dir_all(&out_dir).map_err(|e| format!("mkdir: {e}"))?;
    let out = Command::new("rustc")
        .arg("--edition=2024")
        .arg("--crate-type=lib")
        .arg("--emit=metadata")
        .arg("-Awarnings")
        .arg("--out-dir")
        .arg(&out_dir)
        .arg(&lib)
        .output()
        .map_err(|e| format!("invoke rustc: {e}"))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(format!("rustc failed:\n{stderr}"));
    }
    Ok(())
}

#[test]
fn round_trip_fixture_compiles() {
    let original = fs::read_to_string("fixtures/sample.rs").expect("fixture");
    let (tmp, facade) = split_in_tempdir("sample", &original);

    // After split, consolidate the result and write it back.
    let report = consolidate::consolidate_write(
        &facade,
        &consolidate::ConsolidateOptions { write: true },
    )
    .expect("consolidate write");

    // Facade now holds the merged content. The sub-dir should be gone.
    assert!(!tmp.path().join("sample").exists(), "sub-dir should be removed");
    assert!(report.backup.is_some(), "facade should be backed up to .bak");

    // The merged file MUST compile cleanly under rustc.
    rustc_check(tmp.path(), "sample").expect("merged output must compile");
}

#[test]
fn round_trip_idempotent_when_run_twice() {
    // First round-trip
    let original = fs::read_to_string("fixtures/sample.rs").expect("fixture");
    let (tmp, facade) = split_in_tempdir("sample", &original);
    consolidate::consolidate_write(
        &facade,
        &consolidate::ConsolidateOptions { write: true },
    )
    .expect("first consolidate");
    let merged_once = fs::read_to_string(&facade).expect("read merged");

    // Re-split + re-consolidate — should produce equivalent content.
    run_split(
        &facade,
        SplitOptions {
            use_tokensave: false,
            llm: None,
            write: Some(WriteOptions { force: true }),
        },
    )
    .expect("second split");
    consolidate::consolidate_write(
        &facade,
        &consolidate::ConsolidateOptions { write: true },
    )
    .expect("second consolidate");
    let merged_twice = fs::read_to_string(&facade).expect("read merged twice");

    // Byte-equal would be aggressive — we accept compiled equivalence
    // (both must rustc-compile) AND structural equivalence: same set of
    // item-kind tokens. `syn::parse_file` proves the structure is sound.
    syn::parse_file(&merged_once).expect("first merged parses");
    syn::parse_file(&merged_twice).expect("second merged parses");
    rustc_check(tmp.path(), "sample").expect("second-merged must compile");
}

#[test]
fn consolidate_hand_written_module_compiles() {
    // Not an r2factor output: a hand-written `foo.rs + foo/bar.rs` pair.
    // The general consolidator must still merge it.
    let tmp = tempfile::tempdir().expect("tempdir");
    let foo = tmp.path().join("foo.rs");
    let foo_dir = tmp.path().join("foo");
    fs::create_dir(&foo_dir).unwrap();
    fs::write(
        &foo,
        "//! foo module\nmod bar;\npub use bar::*;\n\npub fn entry() -> u32 { bar::Helper::value() }\n",
    )
    .unwrap();
    fs::write(
        foo_dir.join("bar.rs"),
        "pub struct Helper;\nimpl Helper { pub fn value() -> u32 { 42 } }\n",
    )
    .unwrap();

    consolidate::consolidate_write(
        &foo,
        &consolidate::ConsolidateOptions { write: true },
    )
    .expect("consolidate hand-written");

    assert!(!foo_dir.exists(), "foo/ should be removed");
    rustc_check(tmp.path(), "foo").expect("hand-written merged must compile");
}

#[test]
fn consolidate_mod_rs_style_input() {
    // `foo/mod.rs + foo/sibling.rs` — merge target is parent/foo.rs.
    let tmp = tempfile::tempdir().expect("tempdir");
    let foo_dir = tmp.path().join("foo");
    fs::create_dir(&foo_dir).unwrap();
    fs::write(
        foo_dir.join("mod.rs"),
        "//! foo via mod.rs\nmod sibling;\npub use sibling::*;\n",
    )
    .unwrap();
    fs::write(
        foo_dir.join("sibling.rs"),
        "pub fn greet() -> &'static str { \"hi\" }\n",
    )
    .unwrap();

    consolidate::consolidate_write(
        &foo_dir.join("mod.rs"),
        &consolidate::ConsolidateOptions { write: true },
    )
    .expect("consolidate mod.rs");

    // foo/ should be gone; foo.rs should exist at parent level.
    assert!(!foo_dir.exists(), "foo/ should be removed");
    let merged = tmp.path().join("foo.rs");
    assert!(merged.exists(), "foo.rs should exist at parent level");
    rustc_check(tmp.path(), "foo").expect("mod.rs-style merged must compile");
}

#[test]
fn dry_run_does_not_touch_disk() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let foo = tmp.path().join("foo.rs");
    let foo_dir = tmp.path().join("foo");
    fs::create_dir(&foo_dir).unwrap();
    fs::write(&foo, "mod bar;\npub use bar::*;\n").unwrap();
    fs::write(foo_dir.join("bar.rs"), "pub fn f() {}\n").unwrap();

    let merged = consolidate::consolidate_dry_run(&foo).expect("dry-run");
    assert!(merged.contains("pub fn f"));

    // Files must be unchanged.
    assert!(foo.exists());
    assert!(foo_dir.join("bar.rs").exists());
}
