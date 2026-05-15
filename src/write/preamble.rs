use syn::spanned::Spanned;

/// Pull the leading inner-attribute / `//!` doc block from a source file.
/// Driven by `syn::parse_file` rather than a line-prefix heuristic so that
/// multi-line attributes like
///
/// ```ignore
/// #![allow(
///     dead_code,
///     unused_imports,
/// )]
/// ```
///
/// are preserved intact instead of being chopped at the first line that
/// doesn't begin with `#![` or `//!`.
pub fn extract_inner_attrs(src: &str) -> String {
    let file = match syn::parse_file(src) {
        Ok(f) => f,
        Err(_) => return String::new(),
    };
    let inner: Vec<&syn::Attribute> = file
        .attrs
        .iter()
        .filter(|a| matches!(a.style, syn::AttrStyle::Inner(_)))
        .collect();
    if inner.is_empty() {
        return String::new();
    }
    // Slice up to (and including) the last line covered by an inner attr.
    // proc_macro2 spans are 1-indexed.
    let end_line = inner
        .iter()
        .map(|a| a.span().end().line)
        .max()
        .unwrap_or(0);
    if end_line == 0 {
        return String::new();
    }
    src.lines()
        .take(end_line)
        .collect::<Vec<_>>()
        .join("\n")
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

    #[test]
    fn extract_inner_attrs_preserves_multiline() {
        let src = "#![allow(\n    dead_code,\n    unused_imports\n)]\n\nfn x() {}";
        let out = extract_inner_attrs(src);
        assert_eq!(out, "#![allow(\n    dead_code,\n    unused_imports\n)]");
    }
}
