//! End-to-end tests for the combine pipeline.

use r2factor::combine::{
    CombineOptions, SuggestOptions, combine_dry_run, combine_dry_run_many, combine_write,
    combine_write_many, suggest_groups,
};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn combine_in_tempdir(src1: &str, src2: &str) -> (tempfile::TempDir, PathBuf, PathBuf, PathBuf) {
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
    let (tmp, file1, file2, _lib) =
        combine_in_tempdir("pub fn parse() {}\n", "pub fn tokenize() {}\n");

    let opts = CombineOptions {
        module_name: Some("front_end".to_string()),
        write: false,
        force: false,
        json: false,
        preview_impacts: false,
        use_tokensave: false,
        re_export_filter: None,
        rewrite_consumers: false,
        preview_consumer_rewrites: false,
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
    let (tmp, file1, file2, lib) =
        combine_in_tempdir("pub fn parse() {}\n", "pub fn tokenize() {}\n");

    let opts = CombineOptions {
        module_name: Some("front_end".to_string()),
        write: true,
        force: false,
        json: false,
        preview_impacts: false,
        use_tokensave: false,
        re_export_filter: None,
        rewrite_consumers: false,
        preview_consumer_rewrites: false,
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
fn write_combines_three_peer_files() {
    let (tmp, file1, file2, lib) =
        combine_in_tempdir("pub fn parse() {}\n", "pub fn tokenize() {}\n");
    let file3 = tmp.path().join("ast.rs");
    fs::write(&file3, "pub fn build() { crate::parser::parse(); }\n").unwrap();
    fs::write(&lib, "pub mod parser;\npub mod lexer;\npub mod ast;\n").unwrap();

    let opts = CombineOptions {
        module_name: Some("front_end".to_string()),
        write: true,
        force: false,
        json: false,
        preview_impacts: false,
        use_tokensave: false,
        re_export_filter: None,
        rewrite_consumers: false,
        preview_consumer_rewrites: false,
    };

    let mut dry_opts = opts.clone();
    dry_opts.write = false;
    dry_opts.json = true;
    let dry_report =
        combine_dry_run_many(&[file1.clone(), file2.clone(), file3.clone()], &dry_opts)
            .expect("dry-run succeeds");
    let dry_json: serde_json::Value = serde_json::from_str(&dry_report).expect("dry-run JSON");
    assert_eq!(dry_json["moved_files"].as_array().unwrap().len(), 3);

    let report = combine_write_many(&[file1.clone(), file2.clone(), file3.clone()], &opts)
        .expect("write succeeds");
    assert_eq!(report.moved_files.len(), 3);
    assert!(!file1.exists());
    assert!(!file2.exists());
    assert!(!file3.exists());
    assert!(tmp.path().join("front_end/parser.rs").exists());
    assert!(tmp.path().join("front_end/lexer.rs").exists());
    assert!(tmp.path().join("front_end/ast.rs").exists());

    let facade_src = fs::read_to_string(tmp.path().join("front_end/mod.rs")).unwrap();
    assert!(facade_src.contains("mod parser"));
    assert!(facade_src.contains("mod lexer"));
    assert!(facade_src.contains("mod ast"));
    assert!(facade_src.contains("build"));

    let ast_src = fs::read_to_string(tmp.path().join("front_end/ast.rs")).unwrap();
    assert!(ast_src.contains("crate :: front_end :: parser :: parse"));

    let lib_src = fs::read_to_string(&lib).unwrap();
    assert!(lib_src.contains("mod front_end"));
    assert!(!lib_src.contains("mod parser"));
    assert!(!lib_src.contains("mod lexer"));
    assert!(!lib_src.contains("mod ast"));
    rustc_check(tmp.path(), "front_end").expect("three-file combined output must compile");
}

#[test]
fn suggest_groups_ranks_peer_references() {
    let (tmp, file1, file2, lib) = combine_in_tempdir(
        "pub fn parse() {}\n",
        "pub fn tokenize() { crate::parser::parse(); }\n",
    );
    let file3 = tmp.path().join("ast.rs");
    fs::write(&file3, "pub fn build() { crate::parser::parse(); }\n").unwrap();
    fs::write(&lib, "pub mod parser;\npub mod lexer;\npub mod ast;\n").unwrap();

    let report =
        suggest_groups(tmp.path(), &SuggestOptions { min_score: 1 }).expect("suggest groups");
    assert!(!report.suggestions.is_empty());
    let top = &report.suggestions[0];
    assert!(top.score >= 1);
    assert!(
        top.files
            .iter()
            .any(|path| path.file_name().unwrap() == "parser.rs")
    );
    assert!(
        top.reasons
            .iter()
            .any(|reason| reason.contains("references `parser`"))
    );
    assert!(file1.exists());
    assert!(file2.exists());
}

#[test]
fn write_output_compiles() {
    let (tmp, file1, file2, _lib) =
        combine_in_tempdir("pub fn parse() {}\n", "pub fn tokenize() {}\n");

    let opts = CombineOptions {
        module_name: Some("front_end".to_string()),
        write: true,
        force: false,
        json: false,
        preview_impacts: false,
        use_tokensave: false,
        re_export_filter: None,
        rewrite_consumers: false,
        preview_consumer_rewrites: false,
    };

    combine_write(&file1, &file2, &opts).expect("write succeeds");
    rustc_check(tmp.path(), "front_end").expect("combined output must compile");
}

#[test]
fn write_refuses_existing_target_without_force() {
    let (tmp, file1, file2, _lib) =
        combine_in_tempdir("pub fn parse() {}\n", "pub fn tokenize() {}\n");

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
        rewrite_consumers: false,
        preview_consumer_rewrites: false,
    };

    let result = combine_write(&file1, &file2, &opts);
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(err.contains("already exists"));
}

#[test]
fn json_output_is_valid() {
    let (_tmp, file1, file2, _lib) =
        combine_in_tempdir("pub fn parse() {}\n", "pub fn tokenize() {}\n");

    let opts = CombineOptions {
        module_name: Some("front_end".to_string()),
        write: false,
        force: false,
        json: true,
        preview_impacts: false,
        use_tokensave: false,
        re_export_filter: None,
        rewrite_consumers: false,
        preview_consumer_rewrites: false,
    };

    let report = combine_dry_run(&file1, &file2, &opts).expect("dry-run succeeds");
    let parsed: serde_json::Value = serde_json::from_str(&report).expect("valid JSON");
    assert_eq!(parsed["module_name"], "front_end");
    assert!(
        parsed["facade_content"]
            .as_str()
            .unwrap()
            .contains("mod parser")
    );
    assert_eq!(
        parsed["manifest"]["created_dirs"]
            .as_array()
            .expect("created dirs")
            .len(),
        1
    );
    assert_eq!(
        parsed["manifest"]["removed_files"]
            .as_array()
            .expect("removed files")
            .len(),
        2
    );
    assert_eq!(
        parsed["planned_backups"]
            .as_array()
            .expect("planned backups")
            .len(),
        3
    );
}

#[test]
fn collision_renames_with_prefix() {
    let (_tmp, file1, file2, _lib) =
        combine_in_tempdir("pub fn helper() {}\n", "pub fn helper() {}\n");

    let opts = CombineOptions {
        module_name: Some("front_end".to_string()),
        write: true,
        force: false,
        json: false,
        preview_impacts: false,
        use_tokensave: false,
        re_export_filter: None,
        rewrite_consumers: false,
        preview_consumer_rewrites: false,
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
        rewrite_consumers: false,
        preview_consumer_rewrites: false,
    };

    let report = combine_write(&file1, &file2, &opts).expect("write succeeds");
    let facade_src = fs::read_to_string(&report.facade_path).expect("read facade");
    assert!(facade_src.contains("parse"));
    assert!(!facade_src.contains("internal"));
    assert!(facade_src.contains("tokenize"));
}

#[test]
fn force_preserves_unrelated_target_files() {
    let (tmp, file1, file2, _lib) =
        combine_in_tempdir("pub fn parse() {}\n", "pub fn tokenize() {}\n");
    let target = tmp.path().join("front_end");
    fs::create_dir(&target).unwrap();
    fs::write(target.join("notes.rs"), "pub fn keep_me() {}\n").unwrap();

    let opts = CombineOptions {
        module_name: Some("front_end".to_string()),
        write: true,
        force: true,
        json: false,
        preview_impacts: false,
        use_tokensave: false,
        re_export_filter: None,
        rewrite_consumers: false,
        preview_consumer_rewrites: false,
    };

    let report = combine_write(&file1, &file2, &opts).expect("write succeeds");
    assert!(target.join("notes.rs").exists());
    assert!(
        report
            .manifest
            .preserved_files
            .iter()
            .any(|p| p.ends_with("notes.rs"))
    );
}

#[test]
fn dry_run_json_includes_write_manifest_preview() {
    let (tmp, file1, file2, _lib) =
        combine_in_tempdir("pub fn parse() {}\n", "pub fn tokenize() {}\n");
    let target = tmp.path().join("front_end");
    fs::create_dir(&target).unwrap();
    fs::write(target.join("mod.rs"), "mod stale;\n").unwrap();
    fs::write(target.join("notes.rs"), "pub fn keep_me() {}\n").unwrap();

    let opts = CombineOptions {
        module_name: Some("front_end".to_string()),
        write: false,
        force: true,
        json: true,
        preview_impacts: false,
        use_tokensave: false,
        re_export_filter: None,
        rewrite_consumers: false,
        preview_consumer_rewrites: false,
    };

    let report = combine_dry_run(&file1, &file2, &opts).expect("dry-run succeeds");
    let parsed: serde_json::Value = serde_json::from_str(&report).expect("valid JSON");
    let manifest = &parsed["manifest"];
    assert_eq!(
        manifest["created_dirs"]
            .as_array()
            .expect("created dirs")
            .len(),
        0
    );
    assert!(
        manifest["preserved_files"]
            .as_array()
            .expect("preserved files")
            .iter()
            .any(|p| p.as_str().unwrap().ends_with("front_end/notes.rs"))
    );
    assert!(
        manifest["written_files"]
            .as_array()
            .expect("written files")
            .iter()
            .any(|p| p.as_str().unwrap().ends_with("front_end/mod.rs"))
    );
    assert!(
        parsed["planned_backups"]
            .as_array()
            .expect("planned backups")
            .iter()
            .any(|p| p.as_str().unwrap().ends_with("front_end/mod.rs.bak"))
    );
}

#[test]
fn dry_run_includes_rewritten_sources() {
    let (_tmp, file1, file2, _lib) = combine_in_tempdir(
        "pub fn parse() { super::shared(); }\n",
        "pub fn tokenize() { crate::parser::parse(); }\n",
    );
    let opts = CombineOptions {
        module_name: Some("front_end".to_string()),
        write: false,
        force: false,
        json: true,
        preview_impacts: false,
        use_tokensave: false,
        re_export_filter: None,
        rewrite_consumers: false,
        preview_consumer_rewrites: false,
    };

    let report = combine_dry_run(&file1, &file2, &opts).expect("dry-run succeeds");
    let parsed: serde_json::Value = serde_json::from_str(&report).expect("valid JSON");
    let rewrites = parsed["rewrites"].as_array().expect("rewrites");
    assert_eq!(rewrites.len(), 2);
    assert!(
        rewrites
            .iter()
            .any(|r| r["content"].as_str().unwrap().contains("super :: super"))
    );
    assert!(rewrites.iter().any(|r| {
        r["content"]
            .as_str()
            .unwrap()
            .contains("crate :: front_end :: parser :: parse")
    }));
}

#[test]
fn preview_impacts_reports_missing_tokensave() {
    let (_tmp, file1, file2, _lib) =
        combine_in_tempdir("pub fn parse() {}\n", "pub fn tokenize() {}\n");
    let opts = CombineOptions {
        module_name: Some("front_end".to_string()),
        write: false,
        force: false,
        json: true,
        preview_impacts: true,
        use_tokensave: true,
        re_export_filter: None,
        rewrite_consumers: false,
        preview_consumer_rewrites: false,
    };

    let report = combine_dry_run(&file1, &file2, &opts).expect("dry-run succeeds");
    let parsed: serde_json::Value = serde_json::from_str(&report).expect("valid JSON");
    assert_eq!(parsed["impact"]["available"], false);
    assert!(
        parsed["impact"]["message"]
            .as_str()
            .unwrap()
            .contains(".tokensave")
    );
}

#[test]
fn dry_run_previews_consumer_rewrites_without_touching_disk() {
    let (tmp, file1, file2, _lib) =
        combine_in_tempdir("pub fn parse() {}\n", "pub fn tokenize() {}\n");
    let consumer = tmp.path().join("driver.rs");
    fs::write(
        &consumer,
        "use crate::{parser, lexer};\nuse crate::parser; // review\npub fn run() { crate::parser::parse(); lexer::tokenize(); }\n",
    )
    .unwrap();

    let opts = CombineOptions {
        module_name: Some("front_end".to_string()),
        write: false,
        force: false,
        json: true,
        preview_impacts: false,
        use_tokensave: false,
        re_export_filter: None,
        rewrite_consumers: false,
        preview_consumer_rewrites: true,
    };

    let report = combine_dry_run(&file1, &file2, &opts).expect("dry-run succeeds");
    let parsed: serde_json::Value = serde_json::from_str(&report).expect("valid JSON");
    let rewrites = parsed["consumer_rewrites"]
        .as_array()
        .expect("consumer rewrites");
    assert_eq!(rewrites.len(), 1);
    assert_eq!(rewrites[0]["replacements"], 4);
    let hunks = rewrites[0]["hunks"].as_array().expect("rewrite hunks");
    assert_eq!(hunks.len(), 2);
    assert_eq!(hunks[0]["line"], 1);
    assert!(hunks[0]["new"].as_str().unwrap().contains("front_end"));
    assert_eq!(hunks[1]["line"], 3);
    assert!(
        hunks[1]["old"]
            .as_str()
            .unwrap()
            .contains("crate::parser::parse")
    );
    assert!(
        hunks[1]["new"]
            .as_str()
            .unwrap()
            .contains("crate :: front_end :: parser :: parse")
    );
    let new_source = rewrites[0]["new_source"].as_str().unwrap();
    assert!(new_source.contains("front_end"));
    assert!(!new_source.contains("use crate::{parser, lexer};"));
    assert!(new_source.contains("crate :: front_end :: parser :: parse"));
    assert!(new_source.contains("front_end :: lexer :: tokenize"));
    assert!(
        parsed["manifest"]["updated_files"]
            .as_array()
            .expect("updated files")
            .iter()
            .any(|p| p.as_str().unwrap().ends_with("driver.rs"))
    );
    assert!(
        parsed["planned_backups"]
            .as_array()
            .expect("planned backups")
            .iter()
            .any(|p| p.as_str().unwrap().ends_with("driver.rs.bak"))
    );
    let skipped = parsed["skipped_consumer_rewrites"]
        .as_array()
        .expect("skipped consumer rewrites");
    assert_eq!(skipped.len(), 1);
    assert_eq!(skipped[0]["line"], 2);
    assert_eq!(skipped[0]["old"], "crate::parser");
    assert!(
        skipped[0]["reason"]
            .as_str()
            .unwrap()
            .contains("conservative")
    );

    let original = fs::read_to_string(&consumer).unwrap();
    assert!(original.contains("use crate::{parser, lexer};"));
    assert!(original.contains("use crate::parser; // review"));
    assert!(original.contains("crate::parser::parse"));
    assert!(original.contains("lexer::tokenize"));
    assert!(!tmp.path().join("driver.rs.bak").exists());
}

#[test]
fn consumer_rewrites_use_ast_paths_without_touching_strings_or_comments() {
    let (tmp, file1, file2, _lib) =
        combine_in_tempdir("pub fn parse() {}\n", "pub fn tokenize() {}\n");
    let consumer = tmp.path().join("driver.rs");
    fs::write(
        &consumer,
        "const NOTE: &str = \"crate::parser::parse\";\n// crate::parser::parse should stay in comments\npub fn run() { crate :: parser :: parse(); }\n",
    )
    .unwrap();

    let opts = CombineOptions {
        module_name: Some("front_end".to_string()),
        write: false,
        force: false,
        json: true,
        preview_impacts: false,
        use_tokensave: false,
        re_export_filter: None,
        rewrite_consumers: false,
        preview_consumer_rewrites: true,
    };

    let report = combine_dry_run(&file1, &file2, &opts).expect("dry-run succeeds");
    let parsed: serde_json::Value = serde_json::from_str(&report).expect("valid JSON");
    let rewrites = parsed["consumer_rewrites"]
        .as_array()
        .expect("consumer rewrites");
    assert_eq!(rewrites.len(), 1);
    assert_eq!(rewrites[0]["replacements"], 1);
    let new_source = rewrites[0]["new_source"].as_str().unwrap();
    assert!(new_source.contains("\"crate::parser::parse\""));
    assert!(new_source.contains("// crate::parser::parse should stay in comments"));
    assert!(new_source.contains("crate :: front_end :: parser :: parse"));
}

#[test]
fn rewrite_consumers_updates_nested_use_imports_and_compiles() {
    let (tmp, file1, file2, lib) =
        combine_in_tempdir("pub fn parse() {}\n", "pub fn tokenize() {}\n");
    let consumer = tmp.path().join("driver.rs");
    fs::write(
        &consumer,
        "use crate::parser::parse;\nuse crate::parser::{parse as parse_again};\npub fn run() { parse(); parse_again(); }\n",
    )
    .unwrap();
    fs::write(&lib, "pub mod parser;\npub mod lexer;\npub mod driver;\n").unwrap();

    let opts = CombineOptions {
        module_name: Some("front_end".to_string()),
        write: true,
        force: false,
        json: false,
        preview_impacts: false,
        use_tokensave: false,
        re_export_filter: None,
        rewrite_consumers: true,
        preview_consumer_rewrites: false,
    };

    let report = combine_write(&file1, &file2, &opts).expect("write succeeds");
    let consumer_src = fs::read_to_string(&consumer).unwrap();
    assert!(consumer_src.contains("front_end :: parse"));
    assert!(consumer_src.contains("front_end :: { parse as parse_again }"));
    assert!(!consumer_src.contains("crate::parser::parse"));
    assert_eq!(report.consumer_rewrites.len(), 1);
    assert_eq!(report.consumer_rewrites[0].replacements, 2);
    rustc_check(tmp.path(), "front_end").expect("nested use rewrites must compile");
}

#[test]
fn rewrite_consumers_updates_sibling_paths() {
    let (tmp, file1, file2, _lib) =
        combine_in_tempdir("pub fn parse() {}\n", "pub fn tokenize() {}\n");
    let consumer = tmp.path().join("driver.rs");
    fs::write(
        &consumer,
        "use crate::{parser, lexer};\nuse crate::parser; // review\npub fn run() { crate::parser::parse(); lexer::tokenize(); }\n",
    )
    .unwrap();

    let opts = CombineOptions {
        module_name: Some("front_end".to_string()),
        write: true,
        force: false,
        json: false,
        preview_impacts: false,
        use_tokensave: false,
        re_export_filter: None,
        rewrite_consumers: true,
        preview_consumer_rewrites: false,
    };

    let report = combine_write(&file1, &file2, &opts).expect("write succeeds");
    let consumer_src = fs::read_to_string(&consumer).unwrap();
    assert!(consumer_src.contains("front_end"));
    assert!(!consumer_src.contains("use crate::{parser, lexer};"));
    assert!(consumer_src.contains("crate :: front_end :: parser :: parse"));
    assert!(consumer_src.contains("front_end :: lexer :: tokenize"));
    assert!(consumer_src.contains("use crate::parser; // review"));
    assert_eq!(report.consumer_rewrites.len(), 1);
    assert_eq!(report.consumer_rewrites[0].replacements, 4);
    assert_eq!(report.consumer_rewrites[0].hunks.len(), 2);
    assert_eq!(report.skipped_consumer_rewrites.len(), 1);
    assert_eq!(report.skipped_consumer_rewrites[0].line, 2);
    assert_eq!(report.skipped_consumer_rewrites[0].old, "crate::parser");
    assert!(tmp.path().join("driver.rs.bak").exists());
}
