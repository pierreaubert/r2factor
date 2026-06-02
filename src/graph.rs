use crate::item::{ItemId, ParsedItem};
use crate::tokensave::CrossFileEvidence;
use quote::ToTokens;
use std::collections::{BTreeMap, HashSet};
use syn::visit::{self, Visit};

/// For each item, populate `refs` with the set of identifiers it references
/// that match other items' names in the same file. When tokensave evidence
/// is available, also fold in items reached via cross-symbol edges — this is
/// strictly better than the syn-ident heuristic, which trips on shadowing.
pub fn annotate_refs(items: &mut [ParsedItem], evidence: Option<&CrossFileEvidence>) {
    let names: HashSet<String> = items
        .iter()
        .filter(|i| !i.name.is_empty())
        .map(|i| i.name.clone())
        .collect();

    let id_to_name: BTreeMap<ItemId, String> =
        items.iter().map(|i| (i.id, i.name.clone())).collect();

    let computed: Vec<Vec<String>> = items
        .iter()
        .map(|it| {
            let mut refs: HashSet<String> = collect_refs(&it.source, &names, &it.name)
                .into_iter()
                .collect();
            if let Some(ev) = evidence
                && let Some(callees) = ev.intra_file_callees.get(&it.id)
            {
                for callee_id in callees {
                    if let Some(name) = id_to_name.get(callee_id)
                        && !name.is_empty()
                        && name != &it.name
                    {
                        refs.insert(name.clone());
                    }
                }
            }
            let mut v: Vec<String> = refs.into_iter().collect();
            v.sort();
            v
        })
        .collect();
    for (it, refs) in items.iter_mut().zip(computed) {
        it.refs = refs;
    }
}

pub fn collect_refs(src: &str, names: &HashSet<String>, self_name: &str) -> Vec<String> {
    let Ok(file) = syn::parse_file(src) else {
        return Vec::new();
    };
    let mut visitor = RefVisitor {
        names,
        self_name,
        found: HashSet::new(),
    };
    visit::visit_file(&mut visitor, &file);
    let mut v: Vec<String> = visitor.found.into_iter().collect();
    v.sort();
    v
}

struct RefVisitor<'a> {
    names: &'a HashSet<String>,
    self_name: &'a str,
    found: HashSet<String>,
}

impl<'ast, 'a> Visit<'ast> for RefVisitor<'a> {
    fn visit_ident(&mut self, ident: &'ast proc_macro2::Ident) {
        self.record_ident(&ident.to_string());
    }

    fn visit_attribute(&mut self, attr: &'ast syn::Attribute) {
        scan_attr_tokens(&attr.meta.to_token_stream(), self);
    }

    fn visit_macro(&mut self, mac: &'ast syn::Macro) {
        // `syn` keeps macro contents as an opaque `TokenStream` since it
        // doesn't expand macros, so the default visitor walks past every
        // ident inside e.g. `vec![normalize(s)]`. Drop in a token-level
        // scan so refs from inside macro calls aren't lost.
        visit::visit_path(self, &mac.path);
        scan_token_stream(&mac.tokens, self);
    }
}

impl RefVisitor<'_> {
    fn record_ident(&mut self, ident: &str) {
        if ident != self.self_name && self.names.contains(ident) {
            self.found.insert(ident.to_string());
        }
    }
}

fn scan_attr_tokens(stream: &proc_macro2::TokenStream, v: &mut RefVisitor) {
    use proc_macro2::TokenTree;
    for tt in stream.clone() {
        match tt {
            TokenTree::Ident(id) => v.record_ident(&id.to_string()),
            TokenTree::Group(g) => scan_attr_tokens(&g.stream(), v),
            TokenTree::Literal(lit) => scan_attr_string_path_literal(&lit, v),
            _ => {}
        }
    }
}

fn scan_attr_string_path_literal(lit: &proc_macro2::Literal, v: &mut RefVisitor) {
    let Ok(lit) = syn::parse_str::<syn::LitStr>(&lit.to_string()) else {
        return;
    };
    let Ok(path) = syn::parse_str::<syn::Path>(&lit.value()) else {
        return;
    };
    for segment in path.segments {
        v.record_ident(&segment.ident.to_string());
    }
}

fn scan_token_stream(stream: &proc_macro2::TokenStream, v: &mut RefVisitor) {
    use proc_macro2::TokenTree;
    for tt in stream.clone() {
        match tt {
            TokenTree::Ident(id) => v.visit_ident(&id),
            TokenTree::Group(g) => scan_token_stream(&g.stream(), v),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::item::parse_file;

    #[test]
    fn refs_from_inside_macro_invocation_are_counted() {
        // The fn body calls `normalize` from inside `vec![..]`. Without a
        // token-stream walk, syn's default `Visit` skips the macro body
        // and `helper` ends up with no refs.
        let src = "fn normalize(s: &str) -> &str { s }\nfn helper() { let _ = vec![normalize(\"x\")]; }\n";
        let mut items = parse_file(src).unwrap();
        annotate_refs(&mut items, None);
        let helper = items.iter().find(|i| i.name == "helper").unwrap();
        assert!(
            helper.refs.iter().any(|r| r == "normalize"),
            "helper should ref normalize through the vec! macro; refs={:?}",
            helper.refs
        );
    }

    #[test]
    fn refs_from_attribute_string_paths_are_counted() {
        let src = r#"
fn default_gain() -> f64 { 1.0 }

#[derive(Deserialize)]
struct Config {
    #[serde(default = "default_gain")]
    gain: f64,
}
"#;
        let mut items = parse_file(src).unwrap();
        annotate_refs(&mut items, None);
        let config = items.iter().find(|i| i.name == "Config").unwrap();
        assert!(
            config.refs.iter().any(|r| r == "default_gain"),
            "Config should ref serde string default; refs={:?}",
            config.refs
        );
    }
}
