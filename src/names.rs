//! Module-name normalization shared between the deterministic clusterer and
//! the LLM advisor. Both pipelines produce bucket names that become
//! filenames, and historically each had its own ad-hoc helper — keeping the
//! two in lockstep avoids casing/separator drift between passes.

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
    out
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
        out
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
}
