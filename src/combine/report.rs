use super::plan::CombinePlan;

/// Human-readable dry-run report.
pub fn human_report(plan: &CombinePlan, facade_src: &str) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "r2factor combine — {} files into 1 module\n\n",
        plan.files.len()
    ));
    out.push_str(&format!("== {}/mod.rs (facade) ==\n", plan.module_name));
    for line in facade_src.lines() {
        out.push_str(&format!("  {}\n", line));
    }
    out.push('\n');

    for file in &plan.files {
        out.push_str(&format!(
            "[move] {} -> {}\n",
            file.display(),
            plan.target_dir.join(file.file_name().unwrap()).display()
        ));
    }

    if let Some(parent) = &plan.parent_module {
        out.push_str(&format!(
            "[update] {}: add `mod {};`, remove old mod declarations\n",
            parent.display(),
            plan.module_name
        ));
    }

    for file in &plan.files {
        out.push_str(&format!("[backup] {}.bak\n", file.display()));
    }
    if let Some(parent_module) = &plan.parent_module {
        out.push_str(&format!("[backup] {}.bak\n", parent_module.display()));
    }

    out
}
