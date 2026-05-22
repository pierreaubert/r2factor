use super::plan::CombinePlan;
use super::write::CombineWriteReport;

/// Human-readable dry-run report.
pub fn human_report(plan: &CombinePlan, facade_src: &str) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "r2factor combine — 2 files into 1 module\n\n"
    ));
    out.push_str(&format!(
        "== {}/mod.rs (facade) ==\n",
        plan.module_name
    ));
    for line in facade_src.lines() {
        out.push_str(&format!("  {}\n", line));
    }
    out.push('\n');

    out.push_str(&format!(
        "[move] {} -> {}\n",
        plan.file1.display(),
        plan.target_dir.join(plan.file1.file_name().unwrap()).display()
    ));
    out.push_str(&format!(
        "[move] {} -> {}\n",
        plan.file2.display(),
        plan.target_dir.join(plan.file2.file_name().unwrap()).display()
    ));

    if let Some(parent) = &plan.parent_module {
        out.push_str(&format!(
            "[update] {}: add `mod {};`, remove old mod declarations\n",
            parent.display(),
            plan.module_name
        ));
    }

    out.push_str(&format!(
        "[backup] {}.bak\n",
        plan.file1.display()
    ));
    out.push_str(&format!(
        "[backup] {}.bak\n",
        plan.file2.display()
    ));
    if plan.parent_module.is_some() {
        out.push_str(&format!(
            "[backup] {}.bak\n",
            plan.parent_module.as_ref().unwrap().display()
        ));
    }

    out
}

/// JSON dry-run report.
pub fn json_report(plan: &CombinePlan, facade_src: &str) -> Result<String, serde_json::Error> {
    let payload = serde_json::json!({
        "module_name": plan.module_name,
        "facade_path": plan.facade_path,
        "facade_content": facade_src,
        "moved_files": [
            {
                "from": plan.file1,
                "to": plan.target_dir.join(plan.file1.file_name().unwrap()),
            },
            {
                "from": plan.file2,
                "to": plan.target_dir.join(plan.file2.file_name().unwrap()),
            }
        ],
        "parent_update": plan.parent_module.as_ref().map(|p| {
            let stem1 = plan.file1.file_stem().unwrap().to_str().unwrap();
            let stem2 = plan.file2.file_stem().unwrap().to_str().unwrap();
            serde_json::json!({
                "path": p,
                "add": format!("mod {};", plan.module_name),
                "remove": [format!("mod {};", stem1), format!("mod {};", stem2)],
            })
        }),
    });
    serde_json::to_string_pretty(&payload)
}

/// JSON write report.
pub fn json_write_report(report: &CombineWriteReport) -> Result<String, serde_json::Error> {
    serde_json::to_string_pretty(report)
}
