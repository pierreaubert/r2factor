use anyhow::{Context, Result};
use quote::ToTokens;
use std::fs;
use std::path::{Path, PathBuf};

/// Update the parent module to declare the new combined module and remove
/// declarations for the moved files.
pub fn update_parent_module(
    parent_path: &Path,
    new_module: &str,
    old_modules: &[&str],
) -> Result<String> {
    let src = fs::read_to_string(parent_path)
        .with_context(|| format!("read parent module {}", parent_path.display()))?;
    let mut file = syn::parse_file(&src)
        .with_context(|| format!("parse parent module {}", parent_path.display()))?;

    // Remove old mod declarations
    file.items.retain(|item| {
        if let syn::Item::Mod(m) = item
            && m.content.is_none()
            && old_modules.contains(&m.ident.to_string().as_str())
        {
            return false;
        }
        true
    });

    // Insert new mod declaration
    let new_mod = make_mod_decl(new_module)?;
    file.items.push(new_mod);

    let rendered = file.to_token_stream().to_string();
    Ok(rendered)
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

/// Build a sibling backup path with `.bak` suffix.
pub fn make_backup_path(original: &Path) -> Result<PathBuf> {
    let mut path = original.to_path_buf();
    let name = original
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| anyhow::anyhow!("invalid filename {}", original.display()))?;
    path.set_file_name(format!("{name}.bak"));
    Ok(path)
}
