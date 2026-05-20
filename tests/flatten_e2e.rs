//! End-to-end tests for the flatten post-pass. These cover the contained
//! single-file mode: inline modules are unwrapped, named items get
//! bucket-prefixed, and the resulting module compiles.

use r2factor::{SplitOptions, consolidate, flatten, run_split, write::WriteOptions};
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
            write: Some(WriteOptions {
                force: false,
                recursive_max_lines: Some(0),
            }),
        },
    )
    .expect("split");
    (tmp, f)
}

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
fn flatten_after_split_and_consolidate_compiles() {
    let original = fs::read_to_string("fixtures/sample.rs").expect("fixture");
    let (tmp, facade) = split_in_tempdir("sample", &original);
    consolidate::consolidate_write(&facade, &consolidate::ConsolidateOptions { write: true })
        .expect("consolidate write");

    let report = flatten::flatten_write(&facade, &flatten::FlattenOptions { write: true })
        .expect("flatten write");

    assert!(report.rewrites > 0);
    let flattened = fs::read_to_string(&facade).expect("read flattened");
    assert!(!flattened.contains("use super::*;"));
    rustc_check(tmp.path(), "sample").expect("flattened round-trip module must compile");
}

#[test]
fn flatten_dry_run_rewrites_inline_modules_and_compiles() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let foo = tmp.path().join("foo.rs");
    fs::write(
        &foo,
        r#"
pub struct Parent;

mod math {
    use super::Parent;

    pub struct Helper;

    impl Helper {
        pub fn value() -> u32 {
            40
        }
    }

    fn helper() -> u32 {
        self::Helper::value() + 2
    }

    pub fn entry(parent: Parent) -> u32 {
        let _ = parent;
        helper()
    }
}

mod eval {
    use super::math::entry;

    pub fn run() -> u32 {
        entry(Parent) + math::entry(Parent)
    }
}

pub use math::entry;
pub use eval::*;

pub fn top() -> u32 {
    math::entry(Parent) + eval::run()
}
"#,
    )
    .unwrap();

    let flattened = flatten::flatten_dry_run(&foo).expect("flatten dry-run");
    assert!(flattened.contains("pub struct math_Helper"));
    assert!(flattened.contains("impl math_Helper"));
    assert!(flattened.contains("fn math_helper()"));
    assert!(flattened.contains("pub fn math_entry(parent: Parent)"));
    assert!(flattened.contains("pub use self::math_entry as entry;"));
    assert!(flattened.contains("pub use self::{eval_run as run};"));
    assert!(!flattened.contains("mod math"));
    assert!(!flattened.contains("mod eval"));

    fs::write(&foo, flattened).unwrap();
    rustc_check(tmp.path(), "foo").expect("flattened module must compile");
}

#[test]
fn flatten_write_backs_up_and_reports() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let foo = tmp.path().join("foo.rs");
    fs::write(
        &foo,
        "mod bucket {\n    const N: u8 = 1;\n    pub fn get() -> u8 { N }\n}\n",
    )
    .unwrap();

    let report = flatten::flatten_write(&foo, &flatten::FlattenOptions { write: true })
        .expect("flatten write");

    assert_eq!(report.target, foo);
    assert!(report.backup.as_ref().unwrap().exists());
    assert!(report.rewrites > 0);
    assert!(report.warnings.iter().any(|w| w.contains("widens private")));

    let flattened = fs::read_to_string(&foo).unwrap();
    assert!(flattened.contains("const bucket_N"));
    assert!(flattened.contains("pub fn bucket_get"));
    rustc_check(tmp.path(), "foo").expect("written flattened module must compile");
}

#[test]
fn flatten_preserves_local_shadowing_and_parent_super_refs() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let foo = tmp.path().join("foo.rs");
    fs::write(
        &foo,
        r#"
pub fn helper() -> u32 {
    2
}

mod bucket {
    pub fn helper() -> u32 {
        1
    }

    pub fn shadowed() -> u32 {
        let helper = 40;
        helper + self::helper()
    }

    pub fn parent_ref() -> u32 {
        super::helper()
    }
}
"#,
    )
    .unwrap();

    let flattened = flatten::flatten_dry_run(&foo).expect("flatten dry-run");
    assert!(flattened.contains("helper + bucket_helper()"));
    assert!(flattened.contains("pub fn bucket_parent_ref() -> u32 {\n    helper()\n}"));
    assert!(!flattened.contains("pub fn bucket_parent_ref() -> u32 {\n    bucket_helper()\n}"));

    fs::write(&foo, flattened).unwrap();
    rustc_check(tmp.path(), "foo").expect("flattened shadowing module must compile");
}

#[test]
fn flatten_rewrites_grouped_imports_and_reexports() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let foo = tmp.path().join("foo.rs");
    fs::write(
        &foo,
        r#"
pub struct Parent;
pub struct Thing;

mod math {
    pub struct Helper;

    pub fn entry() -> u32 {
        42
    }
}

mod eval {
    use super::{Parent, Parent as P, Thing};
    use super::math::{entry, Helper as H};

    pub fn run(_: Parent, _: P, _: Thing, _: H) -> u32 {
        entry()
    }
}

pub use math::{entry, Helper};
"#,
    )
    .unwrap();

    let flattened = flatten::flatten_dry_run(&foo).expect("flatten dry-run");
    assert!(flattened.contains("use self::"));
    assert!(flattened.contains("Parent as P"));
    assert!(flattened.contains("Thing"));
    assert!(flattened.contains("pub use self::{math_entry as entry, math_Helper as Helper};"));
    assert!(!flattened.contains("use super::{Parent"));
    assert!(!flattened.contains("use super::math"));
    assert!(flattened.contains("pub fn eval_run(_: Parent, _: P, _: Thing, _: math_Helper)"));
    assert!(flattened.contains("math_entry()"));

    fs::write(&foo, flattened).unwrap();
    rustc_check(tmp.path(), "foo").expect("flattened grouped imports module must compile");
}
