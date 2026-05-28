use anyhow::Result;
use std::collections::HashSet;
use syn::visit::Visit;

pub fn generate_facade_many(
    _module_name: &str,
    modules: &[(&str, &syn::File)],
    filter: Option<&str>,
) -> Result<syn::File> {
    let mut items: Vec<syn::Item> = Vec::new();

    // mod declarations
    for (module, _) in modules {
        items.push(make_mod_decl(module)?);
    }

    let pub_items: Vec<(&str, Vec<(String, syn::Visibility)>)> = modules
        .iter()
        .map(|(module, ast)| (*module, collect_public_items(ast, module, filter)))
        .collect();

    let mut seen = HashSet::new();
    let mut collisions = HashSet::new();
    for (_, items) in &pub_items {
        for (name, _) in items {
            if !seen.insert(name.clone()) {
                collisions.insert(name.clone());
            }
        }
    }

    for (module, module_items) in &pub_items {
        for (name, vis) in module_items {
            let use_item = make_re_export(module, name, vis, collisions.contains(name));
            items.push(use_item);
        }
    }

    Ok(syn::File {
        shebang: None,
        attrs: Vec::new(),
        items,
    })
}

fn make_mod_decl(name: &str) -> Result<syn::Item> {
    let ident = syn::parse_str::<syn::Ident>(name)?;
    let item_mod = syn::ItemMod {
        attrs: Vec::new(),
        vis: syn::Visibility::Inherited,
        unsafety: None,
        mod_token: syn::token::Mod::default(),
        ident,
        content: None,
        semi: Some(syn::token::Semi::default()),
    };
    Ok(syn::Item::Mod(item_mod))
}

fn make_re_export(module: &str, name: &str, vis: &syn::Visibility, collision: bool) -> syn::Item {
    let module_ident = syn::parse_str::<syn::Ident>(module).unwrap();
    let name_ident = syn::parse_str::<syn::Ident>(name).unwrap();

    let tree = if collision {
        let alias = format!("{}_{}", module, name);
        let alias_ident = syn::parse_str::<syn::Ident>(&alias).unwrap();
        syn::UseTree::Rename(syn::UseRename {
            ident: name_ident,
            as_token: syn::token::As::default(),
            rename: alias_ident,
        })
    } else {
        syn::UseTree::Name(syn::UseName { ident: name_ident })
    };

    let use_path = syn::UseTree::Path(syn::UsePath {
        ident: module_ident.clone(),
        colon2_token: syn::token::PathSep::default(),
        tree: Box::new(tree),
    });

    let item_use = syn::ItemUse {
        attrs: Vec::new(),
        vis: vis.clone(),
        use_token: syn::token::Use::default(),
        leading_colon: None,
        tree: use_path,
        semi_token: syn::token::Semi::default(),
    };

    syn::Item::Use(item_use)
}

#[derive(Default)]
struct PublicItemCollector {
    items: Vec<(String, syn::Visibility)>,
}

impl<'ast> Visit<'ast> for PublicItemCollector {
    fn visit_item_fn(&mut self, node: &'ast syn::ItemFn) {
        if is_public(&node.vis) {
            self.items
                .push((node.sig.ident.to_string(), node.vis.clone()));
        }
    }

    fn visit_item_struct(&mut self, node: &'ast syn::ItemStruct) {
        if is_public(&node.vis) {
            self.items.push((node.ident.to_string(), node.vis.clone()));
        }
    }

    fn visit_item_enum(&mut self, node: &'ast syn::ItemEnum) {
        if is_public(&node.vis) {
            self.items.push((node.ident.to_string(), node.vis.clone()));
        }
    }

    fn visit_item_trait(&mut self, node: &'ast syn::ItemTrait) {
        if is_public(&node.vis) {
            self.items.push((node.ident.to_string(), node.vis.clone()));
        }
    }

    fn visit_item_type(&mut self, node: &'ast syn::ItemType) {
        if is_public(&node.vis) {
            self.items.push((node.ident.to_string(), node.vis.clone()));
        }
    }

    fn visit_item_const(&mut self, node: &'ast syn::ItemConst) {
        if is_public(&node.vis) {
            self.items.push((node.ident.to_string(), node.vis.clone()));
        }
    }

    fn visit_item_static(&mut self, node: &'ast syn::ItemStatic) {
        if is_public(&node.vis) {
            self.items.push((node.ident.to_string(), node.vis.clone()));
        }
    }

    fn visit_item_mod(&mut self, node: &'ast syn::ItemMod) {
        if is_public(&node.vis) {
            self.items.push((node.ident.to_string(), node.vis.clone()));
        }
    }

    fn visit_item_macro(&mut self, node: &'ast syn::ItemMacro) {
        if let Some(ref ident) = node.ident {
            // ItemMacro doesn't have visibility; assume pub if at top level
            self.items.push((
                ident.to_string(),
                syn::Visibility::Public(syn::token::Pub::default()),
            ));
        }
    }
}

fn is_public(vis: &syn::Visibility) -> bool {
    matches!(
        vis,
        syn::Visibility::Public(_) | syn::Visibility::Restricted(_)
    )
}

fn collect_public_items(
    ast: &syn::File,
    _module_name: &str,
    filter: Option<&str>,
) -> Vec<(String, syn::Visibility)> {
    let mut collector = PublicItemCollector::default();
    collector.visit_file(ast);

    let re: Option<regex::Regex> = filter.and_then(|f| regex::Regex::new(f).ok());

    collector
        .items
        .into_iter()
        .filter(|(name, _)| {
            if let Some(ref re) = re {
                re.is_match(name)
            } else {
                true
            }
        })
        .collect()
}
