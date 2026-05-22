use quote::ToTokens;
use syn::visit_mut::VisitMut;

/// Rewrite paths inside a file that is moving one module level deeper.
pub fn rewrite_paths(ast: &mut syn::File, other_file_stem: &str, new_module: &str) {
    let mut rewriter = PathRewriter {
        other_file_stem,
        new_module,
    };
    rewriter.visit_file_mut(ast);
}

struct PathRewriter<'a> {
    other_file_stem: &'a str,
    new_module: &'a str,
}

impl VisitMut for PathRewriter<'_> {
    fn visit_path_mut(&mut self, path: &mut syn::Path) {
        // Check conditions first, then mutate
        let is_super = path.segments.first().map(|s| s.ident == "super").unwrap_or(false);
        let is_crate_with_other = path.segments.first().map(|s| s.ident == "crate").unwrap_or(false)
            && path.segments.get(1).map(|s| s.ident == self.other_file_stem).unwrap_or(false);

        if is_super {
            let new_super = syn::PathSegment {
                ident: syn::Ident::new("super", proc_macro2::Span::call_site()),
                arguments: syn::PathArguments::None,
            };
            path.segments.insert(0, new_super);
        }

        if is_crate_with_other {
            let module_seg = syn::PathSegment {
                ident: syn::Ident::new(self.new_module, proc_macro2::Span::call_site()),
                arguments: syn::PathArguments::None,
            };
            path.segments.insert(1, module_seg);
        }

        syn::visit_mut::visit_path_mut(self, path);
    }
}

/// Render the modified AST back to source string.
pub fn render_ast(ast: &syn::File) -> String {
    ast.to_token_stream().to_string()
}
