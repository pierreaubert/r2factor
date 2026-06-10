use crate::item::{ItemKind, ParsedItem};
use quote::{ToTokens, format_ident};
use syn::parse_quote;

pub fn is_proc_macro_entry(item: &ParsedItem) -> bool {
    matches!(item.kind, ItemKind::Fn { .. })
        && parse_proc_macro_fn(&item.source).is_some_and(|item_fn| has_proc_macro_attr(&item_fn))
}

pub fn implementation_source(source: &str) -> Option<String> {
    let mut item_fn = parse_proc_macro_fn(source)?;
    if !has_proc_macro_attr(&item_fn) {
        return None;
    }
    item_fn.attrs.retain(|attr| !is_proc_macro_attr(attr));
    let original = item_fn.sig.ident.clone();
    item_fn.sig.ident = impl_ident(&original);
    item_fn.vis = parse_quote!(pub(super));
    Some(item_fn.to_token_stream().to_string())
}

pub fn wrapper_source(source: &str, module: &str) -> Option<String> {
    let mut item_fn = parse_proc_macro_fn(source)?;
    if !has_proc_macro_attr(&item_fn) {
        return None;
    }
    let module_ident: syn::Ident = syn::parse_str(module).ok()?;
    let impl_ident = impl_ident(&item_fn.sig.ident);
    let args = argument_idents(&item_fn)?;
    item_fn.block = parse_quote!({
        #module_ident::#impl_ident(#(#args),*)
    });
    Some(item_fn.to_token_stream().to_string())
}

fn parse_proc_macro_fn(source: &str) -> Option<syn::ItemFn> {
    match syn::parse_str::<syn::Item>(source).ok()? {
        syn::Item::Fn(item_fn) => Some(item_fn),
        _ => None,
    }
}

fn has_proc_macro_attr(item_fn: &syn::ItemFn) -> bool {
    item_fn.attrs.iter().any(is_proc_macro_attr)
}

fn is_proc_macro_attr(attr: &syn::Attribute) -> bool {
    attr.path().is_ident("proc_macro")
        || attr.path().is_ident("proc_macro_derive")
        || attr.path().is_ident("proc_macro_attribute")
}

fn impl_ident(original: &syn::Ident) -> syn::Ident {
    format_ident!("__r2factor_{}_impl", original)
}

fn argument_idents(item_fn: &syn::ItemFn) -> Option<Vec<syn::Ident>> {
    item_fn
        .sig
        .inputs
        .iter()
        .map(|arg| match arg {
            syn::FnArg::Typed(pat_type) => match pat_type.pat.as_ref() {
                syn::Pat::Ident(pat_ident) => Some(pat_ident.ident.clone()),
                _ => None,
            },
            syn::FnArg::Receiver(_) => None,
        })
        .collect()
}
