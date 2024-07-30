//! Create extensions for types you don't own with [extension traits] but without the boilerplate.
//!
//! Example:
//!
//! ```rust
//! use extend::ext;
//!
//! #[ext]
//! impl<T: Ord> Vec<T> {
//!     fn sorted(mut self) -> Self {
//!         self.sort();
//!         self
//!     }
//! }
//!
//! fn main() {
//!     assert_eq!(
//!         vec![1, 2, 3],
//!         vec![2, 3, 1].sorted(),
//!     );
//! }
//! ```
//!
//! # How does it work?
//!
//! Under the hood it generates a trait with methods in your `impl` and implements those for the
//! type you specify. The code shown above expands roughly to:
//!
//! ```rust
//! trait VecExt<T: Ord> {
//!     fn sorted(self) -> Self;
//! }
//!
//! impl<T: Ord> VecExt<T> for Vec<T> {
//!     fn sorted(mut self) -> Self {
//!         self.sort();
//!         self
//!     }
//! }
//! ```
//!
//! # Configuration
//!
//! You can configure:
//!
//! - The visibility of the trait. The default visibility is private. Example: `#[ext(pub)]`. This
//! must be the first argument to the attribute
//! - The name of the generated extension trait. Example: `#[ext(name = MyExt)]`.
//! - Whether or not the generated trait should be [sealed]. Example: `#[ext(sealed = false)]`. The
//! default is `true`.
//! - Which supertraits the generated extension trait should have. Default is no supertraits.
//! Example: `#[ext(supertraits = Default + Clone)]`.
//!
//! [sealed]: https://rust-lang.github.io/api-guidelines/future-proofing.html#sealed-traits-protect-against-downstream-implementations-c-sealed
//!
//! More examples:
//!
//! ```rust
//! use extend::ext;
//!
//! #[ext(name = SortedVecExt)]
//! impl<T: Ord> Vec<T> {
//!     fn sorted(mut self) -> Self {
//!         self.sort();
//!         self
//!     }
//! }
//!
//! #[ext(pub(crate))]
//! impl i32 {
//!     fn double(self) -> i32 {
//!         self * 2
//!     }
//! }
//!
//! #[ext(pub, name = ResultSafeUnwrapExt)]
//! impl<T> Result<T, std::convert::Infallible> {
//!     fn safe_unwrap(self) -> T {
//!         match self {
//!             Ok(t) => t,
//!             Err(_) => unreachable!(),
//!         }
//!     }
//! }
//!
//! #[ext(supertraits = Default + Clone)]
//! impl String {
//!     fn my_length(self) -> usize {
//!         self.len()
//!     }
//! }
//! ```
//!
//! [extension traits]: https://dev.to/matsimitsu/extending-existing-functionality-in-rust-with-traits-in-rust-3622

#![doc(html_root_url = "https://docs.rs/extend/0.1.1")]
#![allow(clippy::let_and_return)]
#![deny(
    unused_variables,
    mutable_borrow_reservation_conflict,
    dead_code,
    unused_must_use,
    unused_imports
)]

extern crate proc_macro;

use proc_macro2::TokenStream;
use proc_macro_error::*;
use quote::{format_ident, quote, ToTokens};
use syn::{
    parse::{self, Parse, ParseStream},
    parse_macro_input,
    punctuated::Punctuated,
    spanned::Spanned,
    token::{Add, Semi},
    Ident, ImplItem, ItemImpl, LitBool, Token, TraitItemMethod, Type, TypeArray, TypeGroup,
    TypeNever, TypeParamBound, TypeParen, TypePath, TypePtr, TypeReference, TypeSlice, TypeTuple,
    Visibility,
};

/// See crate docs for more info.
#[proc_macro_attribute]
#[proc_macro_error]
#[allow(clippy::unneeded_field_pattern)]
pub fn ext(
    attr: proc_macro::TokenStream,
    item: proc_macro::TokenStream,
) -> proc_macro::TokenStream {
    let item = parse_macro_input!(item as ItemImpl);
    let config = parse_macro_input!(attr as Config);

    let ItemImpl {
        attrs,
        unsafety,
        generics,
        trait_,
        self_ty,
        items,
        // What is defaultness?
        defaultness: _,
        impl_token: _,
        brace_token: _,
    } = item;

    if let Some((_, path, _)) = trait_ {
        abort!(path.span(), "Trait impls cannot be used for #[ext]");
    }

    let self_ty = parse_self_ty(&self_ty);

    let ext_trait_name = config
        .ext_trait_name
        .unwrap_or_else(|| ext_trait_name(&self_ty));

    let trait_methods = items
        .iter()
        .map(|item| trait_method(item))
        .collect::<Vec<_>>();

    let (impl_generics, ty_generics, where_clause) = generics.split_for_impl();

    let private_mod_name = format_ident!("private_{}", ext_trait_name);

    let visibility = &config.visibility;

    let mut all_supertraits = Vec::<TypeParamBound>::new();

    let sealed_mod = if config.sealed {
        all_supertraits.push(syn::parse_str(&format!("{}::Sealed", private_mod_name)).unwrap());

        quote! {
            #[allow(non_snake_case)]
            /// Generated by `#[ext]`.
            ///
            /// Contains stuff required to make the extension trait sealed.
            mod #private_mod_name {
                use super::*;

                pub trait Sealed {}

                impl #impl_generics Sealed for #self_ty #where_clause {}
            }
        }
    } else {
        quote! {}
    };

    if let Some(supertraits_from_config) = config.supertraits {
        all_supertraits.extend(supertraits_from_config);
    }

    let supertraits_quoted = if all_supertraits.is_empty() {
        quote! {}
    } else {
        let supertraits_quoted = punctuated_from_iter::<_, _, Add>(all_supertraits);
        quote! { : #supertraits_quoted }
    };

    let code = (quote! {
        #[allow(non_camel_case_types)]
        #(#attrs)*
        #visibility
        #unsafety
        trait #ext_trait_name #impl_generics #supertraits_quoted #where_clause {
            #(
                #[allow(
                    patterns_in_fns_without_body,
                    clippy::inline_fn_without_body,
                    unused_attributes
                )]
                #trait_methods
            )*
        }

        impl #impl_generics #ext_trait_name #ty_generics for #self_ty #where_clause {
            #(#items)*
        }

        #sealed_mod
    })
    .into();

    code
}

#[derive(Debug, Clone)]
enum ExtType<'a> {
    Array(&'a TypeArray),
    Group(&'a TypeGroup),
    Never(&'a TypeNever),
    Paren(&'a TypeParen),
    Path(&'a TypePath),
    Ptr(&'a TypePtr),
    Reference(&'a TypeReference),
    Slice(&'a TypeSlice),
    Tuple(&'a TypeTuple),
}

fn parse_self_ty(self_ty: &Type) -> ExtType {
    match self_ty {
        Type::Array(inner) => ExtType::Array(inner),
        Type::Group(inner) => ExtType::Group(inner),
        Type::Never(inner) => ExtType::Never(inner),
        Type::Paren(inner) => ExtType::Paren(inner),
        Type::Path(inner) => ExtType::Path(inner),
        Type::Ptr(inner) => ExtType::Ptr(inner),
        Type::Reference(inner) => ExtType::Reference(inner),
        Type::Slice(inner) => ExtType::Slice(inner),
        Type::Tuple(inner) => ExtType::Tuple(inner),

        Type::BareFn(_)
        | Type::ImplTrait(_)
        | Type::Infer(_)
        | Type::Macro(_)
        | Type::Verbatim(_)
        | Type::TraitObject(_)
        | _ => abort!(
            self_ty.span(),
            "#[ext] is not supported for this kind of type"
        ),
    }
}

impl<'a> From<&'a Type> for ExtType<'a> {
    fn from(inner: &'a Type) -> ExtType<'a> {
        parse_self_ty(inner)
    }
}

impl<'a> ToTokens for ExtType<'a> {
    fn to_tokens(&self, tokens: &mut TokenStream) {
        match self {
            ExtType::Array(inner) => inner.to_tokens(tokens),
            ExtType::Group(inner) => inner.to_tokens(tokens),
            ExtType::Never(inner) => inner.to_tokens(tokens),
            ExtType::Paren(inner) => inner.to_tokens(tokens),
            ExtType::Path(inner) => inner.to_tokens(tokens),
            ExtType::Ptr(inner) => inner.to_tokens(tokens),
            ExtType::Reference(inner) => inner.to_tokens(tokens),
            ExtType::Slice(inner) => inner.to_tokens(tokens),
            ExtType::Tuple(inner) => inner.to_tokens(tokens),
        }
    }
}

fn ext_trait_name(self_ty: &ExtType) -> Ident {
    fn inner_self_ty(self_ty: &ExtType) -> Ident {
        match self_ty {
            ExtType::Path(inner) => inner
                .path
                .segments
                .last()
                .unwrap_or_else(|| abort!(inner.span(), "Empty type path"))
                .ident
                .clone(),
            ExtType::Reference(inner) => {
                let name = inner_self_ty(&(&*inner.elem).into());
                format_ident!("Ref{}", name)
            }
            ExtType::Array(inner) => {
                let name = inner_self_ty(&(&*inner.elem).into());
                format_ident!("ListOf{}", name)
            }
            ExtType::Group(inner) => {
                let name = inner_self_ty(&(&*inner.elem).into());
                format_ident!("Group{}", name)
            }
            ExtType::Paren(inner) => {
                let name = inner_self_ty(&(&*inner.elem).into());
                format_ident!("Paren{}", name)
            }
            ExtType::Ptr(inner) => {
                let name = inner_self_ty(&(&*inner.elem).into());
                format_ident!("PointerTo{}", name)
            }
            ExtType::Slice(inner) => {
                let name = inner_self_ty(&(&*inner.elem).into());
                format_ident!("SliceOf{}", name)
            }
            ExtType::Tuple(inner) => {
                let mut name = format_ident!("TupleOf");
                for elem in &inner.elems {
                    name = format_ident!("{}{}", name, inner_self_ty(&elem.into()));
                }
                name
            }
            ExtType::Never(_) => format_ident!("Never"),
        }
    }

    format_ident!("{}Ext", inner_self_ty(self_ty))
}

fn trait_method(item: &ImplItem) -> TraitItemMethod {
    let method = match item {
        ImplItem::Method(method) => method,
        _ => abort!(item.span(), "Only methods are allowed in #[ext] impls"),
    };

    TraitItemMethod {
        attrs: method.attrs.clone(),
        sig: method.sig.clone(),
        default: None,
        semi_token: Some(Semi::default()),
    }
}

#[derive(Debug)]
struct Config {
    ext_trait_name: Option<Ident>,
    visibility: Visibility,
    sealed: bool,
    supertraits: Option<Punctuated<TypeParamBound, Add>>,
}

impl Parse for Config {
    fn parse(input: ParseStream) -> parse::Result<Self> {
        let mut config = Config::default();

        if let Ok(visibility) = input.parse::<Visibility>() {
            config.visibility = visibility;
        }

        input.parse::<Token![,]>().ok();

        while !input.is_empty() {
            let ident = input.parse::<Ident>()?;
            input.parse::<Token![=]>()?;

            match &*ident.to_string() {
                "name" => {
                    config.ext_trait_name = Some(input.parse()?);
                }
                "sealed" => {
                    config.sealed = input.parse::<LitBool>()?.value;
                }
                "supertraits" => {
                    config.supertraits =
                        Some(Punctuated::<TypeParamBound, Add>::parse_terminated(input)?);
                }
                _ => abort!(ident.span(), "Unknown configuration name"),
            }

            input.parse::<Token![,]>().ok();
        }

        Ok(config)
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            ext_trait_name: None,
            visibility: Visibility::Inherited,
            sealed: true,
            supertraits: None,
        }
    }
}

fn punctuated_from_iter<I, T, P>(i: I) -> Punctuated<T, P>
where
    P: Default,
    I: IntoIterator<Item = T>,
{
    let mut iter = i.into_iter().peekable();
    let mut acc = Punctuated::default();

    while let Some(item) = iter.next() {
        acc.push_value(item);

        if iter.peek().is_some() {
            acc.push_punct(P::default());
        }
    }

    acc
}

#[cfg(test)]
mod test {
    #[allow(unused_imports)]
    use super::*;

    #[test]
    fn test_ui() {
        let t = trybuild::TestCases::new();
        t.pass("tests/compile_pass/*.rs");
        t.compile_fail("tests/compile_fail/*.rs");
    }
}