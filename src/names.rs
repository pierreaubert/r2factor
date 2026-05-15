//! Module-name normalization shared between the deterministic clusterer and
//! the LLM advisor. Both pipelines produce bucket names that become
//! filenames, and historically each had its own ad-hoc helper — keeping the
//! two in lockstep avoids casing/separator drift between passes.
//!
//! Bucket names also become Rust identifiers (`mod foo;`), so we have to
//! avoid reserved keywords. Code that anchors on a fn prefix like
//! `try_each`, `try_apply`, … would otherwise produce `mod try;` which is
//! a parse error. [`escape_keyword`] handles the rename.

/// Rust reserved / strict-mode keywords that cannot appear as plain
/// identifiers in `mod NAME;` declarations. Source: Rust Reference,
/// "Keywords" — includes both currently-used and reserved-for-future-use
/// keywords across all editions. We err on the side of escaping more
/// names than strictly necessary; the cost is one trailing underscore on
/// rare-but-real cases like `try_*` / `yield_*` / `match_*` clusters.
const RESERVED: &[&str] = &[
    "as", "break", "const", "continue", "crate", "dyn", "else", "enum", "extern", "false", "fn",
    "for", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub", "ref",
    "return", "self", "Self", "static", "struct", "super", "trait", "true", "type", "unsafe",
    "use", "where", "while", "async", "await", "abstract", "become", "box", "do", "final",
    "macro", "override", "priv", "typeof", "unsized", "virtual", "yield", "try", "gen",
];

/// If `name` would collide with a Rust keyword, append `_` to make it a
/// valid identifier. Otherwise return it unchanged. We don't use the
/// raw-identifier (`r#name`) form because that would require sprinkling
/// `r#` through every emit site (mod decl, pub use, cross-imports) and
/// readers would have to remember which buckets are raw.
pub fn escape_keyword(name: &str) -> String {
    if RESERVED.iter().any(|kw| *kw == name) {
        format!("{name}_")
    } else {
        name.to_string()
    }
}

/// Lower a Rust type name (`HttpClient`, `FooBar`) into a snake-cased module
/// stem (`http_client`, `foo_bar`). Runs of capitals collapse without
/// splitting (`HTTPClient` -> `httpclient`) — this matches what most Rust
/// codebases ship for acronym-heavy type names. The literal `"misc"` is
/// passed through unchanged so the bucket stays recognizable.
pub fn type_to_module_name(ty: &str) -> String {
    if ty == "misc" {
        return "misc".to_string();
    }
    let mut out = String::with_capacity(ty.len() + 4);
    let mut prev_lower = false;
    for ch in ty.chars() {
        if ch.is_ascii_uppercase() {
            if prev_lower {
                out.push('_');
            }
            out.push(ch.to_ascii_lowercase());
            prev_lower = false;
        } else {
            out.push(ch);
            prev_lower = ch.is_ascii_lowercase() || ch.is_ascii_digit();
        }
    }
    escape_keyword(&out)
}

/// Sanitize a free-form module name (typically coming from the LLM) into a
/// valid Rust file stem. Strips a trailing `.rs`, lowercases ASCII letters,
/// keeps `_` and digits, and turns whitespace / `-` / `/` into `_`. Empty
/// results collapse to `"misc"` so we always have a valid bucket key.
pub fn sanitize_module(name: &str) -> String {
    let trimmed = name.trim().trim_end_matches(".rs");
    let mut out = String::with_capacity(trimmed.len());
    for ch in trimmed.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' {
            out.push(ch.to_ascii_lowercase());
        } else if ch.is_whitespace() || ch == '-' || ch == '/' {
            out.push('_');
        }
    }
    if out.is_empty() {
        "misc".to_string()
    } else {
        escape_keyword(&out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snake_cases_camel() {
        assert_eq!(type_to_module_name("FooBar"), "foo_bar");
        assert_eq!(type_to_module_name("HTTPClient"), "httpclient");
        assert_eq!(type_to_module_name("Token"), "token");
    }

    #[test]
    fn sanitize_strips_ext_and_specials() {
        assert_eq!(sanitize_module("Eval Helpers.rs"), "eval_helpers");
        assert_eq!(sanitize_module("foo-bar/baz"), "foo_bar_baz");
        assert_eq!(sanitize_module(""), "misc");
    }

    #[test]
    fn escapes_keyword_buckets() {
        // Real failure observed on `meshlang-compiler`: cluster picked
        // the prefix `try` from `try_*` fns, then `mod try;` failed to
        // parse. We rename to `try_` instead.
        assert_eq!(type_to_module_name("try"), "try_");
        assert_eq!(type_to_module_name("Try"), "try_");
        assert_eq!(sanitize_module("match"), "match_");
        assert_eq!(sanitize_module("yield"), "yield_");
    }

    #[test]
    fn non_keywords_pass_through() {
        assert_eq!(type_to_module_name("tries"), "tries");
        assert_eq!(type_to_module_name("matcher"), "matcher");
        assert_eq!(sanitize_module("trying"), "trying");
    }
}
