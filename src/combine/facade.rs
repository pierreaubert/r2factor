use anyhow::Result;
use std::collections::HashSet;
use syn::visit::Visit;

/// Generate the facade AST: mod declarations + pub use re-exports.
pub fn generate_facade(
    _module_name: &str,
    file1_name: &str,
    file2_name: &str,
    file1_ast: &syn::File,
    file2_ast: &syn::File,
    filter: Option<&str>,
) -> Result<syn::File> {
    let mut items: Vec<syn::Item> = Vec::new();

    // mod declarations
    let mod1 = make_mod_decl(file1_name)?;
    let mod2 = make_mod_decl(file2_name)?;
    items.push(mod1);
    items.push(mod2);

    // Collect public items from both files
    let pub_items1 = collect_public_items(file1_ast, file1_name, filter);
    let pub_items2 = collect_public_items(file2_ast, file2_name, filter);

    // Detect collisions
    let names1: HashSet<String> = pub_items1.iter().map(|(name, _)| name.clone()).collect();
    let names2: HashSet<String> = pub_items2.iter().map(|(name, _)| name.clone()).collect();
    let collisions: HashSet<String> = names1.intersection(&names2).cloned().collect();

    // Generate re-exports for file1
    for (name, vis) in &pub_items1 {
        let use_item = make_re_export(file1_name, name, vis, collisions.contains(name));
        items.push(use_item);
    }

    // Generate re-exports for file2
    for (name, vis) in &pub_items2 {
        let use_item = make_re_export(file2_name, name, vis, collisions.contains(name));
        items.push(use_item);
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
        syn::UseTree::Name(syn::UseName {
            ident: name_ident,
        })
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
            self.items.push((node.sig.ident.to_string(), node.vis.clone()));
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
            self.items.push((ident.to_string(), syn::Visibility::Public(syn::token::Pub::default())));
        }
    }
}

fn is_public(vis: &syn::Visibility) -> bool {
    matches!(vis, syn::Visibility::Public(_) | syn::Visibility::Restricted(_))
}

fn collect_public_items(ast: &syn::File, _module_name: &str, filter: Option<&str>) -> Vec<(String, syn::Visibility)> {
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
