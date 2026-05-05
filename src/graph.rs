use crate::item::{ItemId, ParsedItem};
use crate::tokensave::CrossFileEvidence;
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

fn collect_refs(src: &str, names: &HashSet<String>, self_name: &str) -> Vec<String> {
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
        let s = ident.to_string();
        if s != self.self_name && self.names.contains(&s) {
            self.found.insert(s);
        }
    }
}
