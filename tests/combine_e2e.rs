//! End-to-end tests for the combine pipeline.

use r2factor::combine::{CombineOptions, combine_dry_run, combine_write};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn combine_in_tempdir(
    src1: &str,
    src2: &str,
) -> (tempfile::TempDir, PathBuf, PathBuf, PathBuf) {
    let tmp = tempfile::tempdir().expect("create tempdir");
    let file1 = tmp.path().join("parser.rs");
    let file2 = tmp.path().join("lexer.rs");
    let lib_rs = tmp.path().join("lib.rs");
    fs::write(&file1, src1).expect("write parser.rs");
    fs::write(&file2, src2).expect("write lexer.rs");
    fs::write(&lib_rs, "pub mod parser;\npub mod lexer;\n").expect("write lib.rs");
    (tmp, file1, file2, lib_rs)
}

fn rustc_check(tmp_root: &Path, _stem: &str) -> Result<(), String> {
    let lib = tmp_root.join("lib.rs");
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
fn dry_run_does_not_modify_files() {
    let (tmp, file1, file2, _lib) = combine_in_tempdir(
        "pub fn parse() {}\n",
        "pub fn tokenize() {}\n",
    );

    let opts = CombineOptions {
        module_name: Some("front_end".to_string()),
        write: false,
        force: false,
        json: false,
        preview_impacts: false,
        use_tokensave: false,
        re_export_filter: None,
    };

    let report = combine_dry_run(&file1, &file2, &opts).expect("dry-run succeeds");
    assert!(report.contains("front_end/mod.rs"));
    assert!(report.contains("mod parser"));
    assert!(report.contains("mod lexer"));

    // Files should be untouched
    assert!(file1.exists());
    assert!(file2.exists());
    assert!(!tmp.path().join("front_end").exists());
}

#[test]
fn write_creates_facade_and_moves_files() {
    let (tmp, file1, file2, lib) = combine_in_tempdir(
        "pub fn parse() {}\n",
        "pub fn tokenize() {}\n",
    );

    let opts = CombineOptions {
        module_name: Some("front_end".to_string()),
        write: true,
        force: false,
        json: false,
        preview_impacts: false,
        use_tokensave: false,
        re_export_filter: None,
    };

    let report = combine_write(&file1, &file2, &opts).expect("write succeeds");

    // Facade exists
    assert!(report.facade_path.exists());
    let facade_src = fs::read_to_string(&report.facade_path).expect("read facade");
    assert!(facade_src.contains("mod parser"));
    assert!(facade_src.contains("mod lexer"));

    // Files moved
    assert!(!file1.exists());
    assert!(!file2.exists());
    assert!(tmp.path().join("front_end/parser.rs").exists());
    assert!(tmp.path().join("front_end/lexer.rs").exists());

    // Backups created
    assert!(tmp.path().join("parser.rs.bak").exists());
    assert!(tmp.path().join("lexer.rs.bak").exists());

    // Parent module updated
    let lib_src = fs::read_to_string(&lib).expect("read lib.rs");
    if !lib_src.contains("mod front_end") {
        panic!("Expected 'mod front_end' in lib.rs:\n{}", lib_src);
    }
    assert!(!lib_src.contains("mod parser"));
    assert!(!lib_src.contains("mod lexer"));
}

#[test]
fn write_output_compiles() {
    let (tmp, file1, file2, _lib) = combine_in_tempdir(
        "pub fn parse() {}\n",
        "pub fn tokenize() {}\n",
    );

    let opts = CombineOptions {
        module_name: Some("front_end".to_string()),
        write: true,
        force: false,
        json: false,
        preview_impacts: false,
        use_tokensave: false,
        re_export_filter: None,
    };

    combine_write(&file1, &file2, &opts).expect("write succeeds");
    rustc_check(tmp.path(), "front_end").expect("combined output must compile");
}

#[test]
fn write_refuses_existing_target_without_force() {
    let (tmp, file1, file2, _lib) = combine_in_tempdir(
        "pub fn parse() {}\n",
        "pub fn tokenize() {}\n",
    );

    // Pre-create target directory
    fs::create_dir(tmp.path().join("front_end")).unwrap();

    let opts = CombineOptions {
        module_name: Some("front_end".to_string()),
        write: true,
        force: false,
        json: false,
        preview_impacts: false,
        use_tokensave: false,
        re_export_filter: None,
    };

    let result = combine_write(&file1, &file2, &opts);
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(err.contains("already exists"));
}

#[test]
fn json_output_is_valid() {
    let (tmp, file1, file2, _lib) = combine_in_tempdir(
        "pub fn parse() {}\n",
        "pub fn tokenize() {}\n",
    );

    let opts = CombineOptions {
        module_name: Some("front_end".to_string()),
        write: false,
        force: false,
        json: true,
        preview_impacts: false,
        use_tokensave: false,
        re_export_filter: None,
    };

    let report = combine_dry_run(&file1, &file2, &opts).expect("dry-run succeeds");
    let parsed: serde_json::Value = serde_json::from_str(&report).expect("valid JSON");
    assert_eq!(parsed["module_name"], "front_end");
    assert!(parsed["facade_content"].as_str().unwrap().contains("mod parser"));
}

#[test]
fn collision_renames_with_prefix() {
    let (tmp, file1, file2, _lib) = combine_in_tempdir(
        "pub fn helper() {}\n",
        "pub fn helper() {}\n",
    );

    let opts = CombineOptions {
        module_name: Some("front_end".to_string()),
        write: true,
        force: false,
        json: false,
        preview_impacts: false,
        use_tokensave: false,
        re_export_filter: None,
    };

    let report = combine_write(&file1, &file2, &opts).expect("write succeeds");
    let facade_src = fs::read_to_string(&report.facade_path).expect("read facade");
    assert!(facade_src.contains("parser_helper"));
    assert!(facade_src.contains("lexer_helper"));
}

#[test]
fn re_export_filter_works() {
    let (_tmp, file1, file2, _lib) = combine_in_tempdir(
        "pub fn parse() {}\npub fn internal() {}\n",
        "pub fn tokenize() {}\n",
    );

    let opts = CombineOptions {
        module_name: Some("front_end".to_string()),
        write: true,
        force: false,
        json: false,
        preview_impacts: false,
        use_tokensave: false,
        re_export_filter: Some("parse|tokenize".to_string()),
    };

    let report = combine_write(&file1, &file2, &opts).expect("write succeeds");
    let facade_src = fs::read_to_string(&report.facade_path).expect("read facade");
    assert!(facade_src.contains("parse"));
    assert!(!facade_src.contains("internal"));
    assert!(facade_src.contains("tokenize"));
}
