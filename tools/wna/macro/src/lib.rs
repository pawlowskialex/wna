//! `wna::generate!`: native WNA bindings for Rust, shaped like wit-bindgen's.
//! On `wasm32` targets the same invocation expands to `wit_bindgen::generate!`,
//! so one source builds as a native component or as a Wasm component.
//!
//! ```ignore
//! wna::generate!({
//!     world: "acme:feature/rules-engine",
//!     path: "../../wit",          // a directory of WIT package directories
//!     name: "acme:promo-rules",   // component name (default: the crate name)
//!     additional_derives: [PartialEq],
//! });
//! ```

mod compose;

use proc_macro::TokenStream;
use proc_macro2::Span;
use quote::quote;
use std::path::PathBuf;
use syn::parse::{Parse, ParseStream};
use syn::punctuated::Punctuated;
use syn::{Ident, LitStr, Path, Token, braced, bracketed};

struct Input {
    world: LitStr,
    path: LitStr,
    name: Option<LitStr>,
    additional_derives: Vec<Path>,
}

impl Parse for Input {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let content;
        braced!(content in input);
        let (mut world, mut path, mut name, mut derives) = (None, None, None, Vec::new());
        while !content.is_empty() {
            let key: Ident = content.parse()?;
            content.parse::<Token![:]>()?;
            match key.to_string().as_str() {
                "world" => world = Some(content.parse()?),
                "path" => path = Some(content.parse()?),
                "name" => name = Some(content.parse()?),
                "additional_derives" => {
                    let list;
                    bracketed!(list in content);
                    derives = Punctuated::<Path, Token![,]>::parse_terminated(&list)?.into_iter().collect();
                }
                k => return Err(syn::Error::new(key.span(), format!("unknown option `{k}`"))),
            }
            if !content.is_empty() {
                content.parse::<Token![,]>()?;
            }
        }
        Ok(Input {
            world: world.ok_or_else(|| input.error("missing `world`"))?,
            path: path.ok_or_else(|| input.error("missing `path`"))?,
            name,
            additional_derives: derives,
        })
    }
}

#[proc_macro]
pub fn generate(input: TokenStream) -> TokenStream {
    let input = syn::parse_macro_input!(input as Input);
    match expand(&input) {
        Ok(t) => t.into(),
        Err(e) => syn::Error::new(Span::call_site(), format!("{e:#}")).to_compile_error().into(),
    }
}

fn expand(input: &Input) -> anyhow::Result<proc_macro2::TokenStream> {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR")?);
    let root = manifest.join(input.path.value());
    let wit = wna_abi::Wit::load(&root)?;
    let world = wit.world(&input.world.value())?;
    let name = input.name.as_ref().map(|n| n.value()).unwrap_or_else(|| std::env::var("CARGO_PKG_NAME").unwrap_or_default());
    let derives: Vec<String> = input.additional_derives.iter().map(|p| quote!(#p).to_string().replace(' ', "")).collect();
    let src = wna_gen::rust::generate(&wit, world, &wna_gen::rust::Opts { name, additional_derives: derives })?;
    let mut file: syn::File = syn::parse_str(&src).map_err(|e| anyhow::anyhow!("generated code does not parse: {e}"))?;
    // Native bindings exist only off-wasm; on wasm, wit-bindgen's take their place.
    for item in &mut file.items {
        let attrs = match item {
            syn::Item::Mod(m) => &mut m.attrs,
            syn::Item::Macro(m) => &mut m.attrs,
            syn::Item::Use(u) => &mut u.attrs,
            _ => continue,
        };
        attrs.push(syn::parse_quote!(#[cfg(not(target_family = "wasm"))]));
    }
    let native = quote!(#file);

    // wit-bindgen wants package directories in dependency order.
    let dirs: Vec<String> = wna_abi::package_dirs_in_order(&root)?.iter().map(|d| d.display().to_string()).collect();
    let world_name = &input.world;
    let derives = &input.additional_derives;
    let tracked = wit.sources.iter().map(|p| p.display().to_string());
    Ok(quote! {
        #[cfg(not(target_family = "wasm"))]
        const _: () = { #( let _ = include_bytes!(#tracked); )* };

        #native

        #[cfg(target_family = "wasm")]
        ::wna::__private::wit_bindgen::generate!({
            world: #world_name,
            path: [#(#dirs),*],
            generate_all,
            runtime_path: "::wna::__private::wit_bindgen::rt",
            bitflags_path: "::wna::__private::bitflags",
            additional_derives: [#(#derives),*],
        });
    })
}

/// The composition root: `wna::compose!("wna.compose.toml");`. Validates the
/// wiring at build time and defines `compose()` and the C `wna_compose`.
#[proc_macro]
pub fn compose(input: TokenStream) -> TokenStream {
    let path = syn::parse_macro_input!(input as LitStr);
    match compose::expand(&path.value()) {
        Ok(t) => t.into(),
        Err(e) => syn::Error::new(path.span(), format!("{e:#}")).to_compile_error().into(),
    }
}
