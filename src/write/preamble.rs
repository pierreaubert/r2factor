/// Pull the leading inner-attribute / `//!` doc block from a source file.
/// Stops at the first non-empty line that isn't `//!` or `#![...]`. Block
/// comments and mixed outer comments aren't handled — the heuristic is
/// intentionally simple because the result is only used to prefix the
/// regenerated facade.
pub fn extract_inner_attrs(src: &str) -> String {
    let mut out = String::new();
    for line in src.lines() {
        let t = line.trim_start();
        if t.is_empty() || t.starts_with("//!") || t.starts_with("#![") {
            out.push_str(line);
            out.push('\n');
        } else {
            break;
        }
    }
    out.trim_end().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_inner_attrs_finds_doc_mod() {
        let src = "//! file-level doc\n//! more docs\n\nuse foo::bar;\nfn x() {}";
        let out = extract_inner_attrs(src);
        assert_eq!(out, "//! file-level doc\n//! more docs");
    }

    #[test]
    fn extract_inner_attrs_handles_inner_attr() {
        let src = "#![allow(dead_code)]\n//! doc\nfn x(){}";
        let out = extract_inner_attrs(src);
        assert_eq!(out, "#![allow(dead_code)]\n//! doc");
    }

    #[test]
    fn extract_inner_attrs_returns_empty_when_none() {
        assert_eq!(extract_inner_attrs("fn x(){}"), "");
    }
}
