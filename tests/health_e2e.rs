use r2factor::health::{check, human_report};
use std::fs;

#[test]
fn check_reports_actionable_suggestions_without_cargo() {
    let tmp = tempfile::tempdir().expect("tempdir");

    let report = check(tmp.path()).expect("health check");
    let ids: Vec<&str> = report.suggestions.iter().map(|s| s.id.as_str()).collect();
    assert!(ids.contains(&"cargo_root_missing"));
    assert!(ids.contains(&"tokensave_unavailable"));

    let text = human_report(&report);
    assert!(text.contains("suggestion: cargo_root_missing"));
    assert!(text.contains("suggestion: tokensave_unavailable"));
    assert!(text.contains("command: tokensave index"));
}

#[test]
fn check_reports_local_path_dependency_suggestion() {
    let tmp = tempfile::tempdir().expect("tempdir");
    fs::write(
        tmp.path().join("Cargo.toml"),
        "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\n[dependencies]\nhelper = { path = \"../helper\" }\n",
    )
    .unwrap();

    let report = check(tmp.path()).expect("health check");
    assert!(
        report
            .suggestions
            .iter()
            .any(|s| s.id == "local_path_dependencies")
    );
    assert!(
        report
            .local_path_dependencies
            .iter()
            .any(|dep| dep.contains("../helper"))
    );

    let json = serde_json::to_value(&report).expect("serialize report");
    assert!(
        json["suggestions"]
            .as_array()
            .expect("suggestions")
            .iter()
            .any(|s| s["id"] == "local_path_dependencies")
    );
}
