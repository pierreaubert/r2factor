use r2factor::backups::{list_backups, restore_backup};
use std::fs;
use std::process::Command;

#[test]
fn list_backups_reports_restore_targets() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let nested = tmp.path().join("src");
    fs::create_dir(&nested).unwrap();
    fs::write(tmp.path().join("foo.rs.bak"), "old foo").unwrap();
    fs::write(nested.join("bar.rs.bak"), "old bar").unwrap();
    fs::write(nested.join("bar.rs"), "new bar").unwrap();

    let backups = list_backups(tmp.path()).expect("list backups");
    assert_eq!(backups.len(), 2);
    assert!(
        backups
            .iter()
            .any(|b| b.backup.ends_with("foo.rs.bak") && b.restore_target.ends_with("foo.rs"))
    );
    assert!(
        backups
            .iter()
            .any(|b| b.backup.ends_with("bar.rs.bak") && b.restore_target.ends_with("bar.rs"))
    );
}

#[test]
fn restore_refuses_existing_target_without_force() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let target = tmp.path().join("foo.rs");
    let backup = tmp.path().join("foo.rs.bak");
    fs::write(&target, "new").unwrap();
    fs::write(&backup, "old").unwrap();

    let err = restore_backup(&backup, false).expect_err("restore should refuse overwrite");
    assert!(err.to_string().contains("already exists"));
    assert_eq!(fs::read_to_string(&target).unwrap(), "new");

    let report = restore_backup(&backup, true).expect("restore with force");
    assert!(report.replaced_existing);
    assert_eq!(fs::read_to_string(&target).unwrap(), "old");
    assert!(backup.exists());
}

#[test]
fn backups_cli_json_lists_backups() {
    let tmp = tempfile::tempdir().expect("tempdir");
    fs::write(tmp.path().join("foo.rs.bak"), "old foo").unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_r2factor"))
        .arg("backups")
        .arg(tmp.path())
        .arg("--json")
        .output()
        .expect("run r2factor backups");
    assert!(output.status.success());

    let parsed: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("backup JSON output");
    let backups = parsed.as_array().expect("array output");
    assert_eq!(backups.len(), 1);
    assert!(
        backups[0]["restore_target"]
            .as_str()
            .unwrap()
            .ends_with("foo.rs")
    );
}
