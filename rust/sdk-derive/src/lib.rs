#![forbid(unsafe_code)]

use proc_macro::TokenStream;
use quote::quote;
use std::collections::BTreeSet;
use syn::{
    parse_macro_input, Attribute, Data, DeriveInput, Expr, ExprLit, Fields, ImplItem, ItemImpl,
    Lit, LitStr, Meta,
};

/// Generate the typed extension dispatch implementation and the factory owned
/// by Axiom's generated WASM wrapper.
///
/// Exported methods must take `&mut self` and one `TypedInvocation`, and return
/// `Result<ExtensionResponse>`. The extension type must implement `Default`.
#[proc_macro_attribute]
pub fn extension(arguments: TokenStream, input: TokenStream) -> TokenStream {
    if !arguments.is_empty() {
        return syn::Error::new(
            proc_macro2::Span::call_site(),
            "extension takes no arguments",
        )
        .into_compile_error()
        .into();
    }
    let implementation = parse_macro_input!(input as ItemImpl);
    expand_extension(implementation)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

/// Export markers are consumed by `#[extension]`. Seeing one independently is
/// always an authoring error rather than a silently ignored annotation.
#[proc_macro_attribute]
pub fn export(_arguments: TokenStream, input: TokenStream) -> TokenStream {
    let item: proc_macro2::TokenStream = input.into();
    quote! {
        compile_error!("#[export] must be nested inside an #[extension] inherent impl");
        #item
    }
    .into()
}

/// Derive canonical Axiom record encoding and strict record decoding.
///
/// DX 4 intentionally limits the derive to non-generic named-field structs.
/// Export dispatch and extension implementation macros remain part of DX 5.
#[proc_macro_derive(AxiomType, attributes(axiom))]
pub fn derive_axiom_type(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    expand(input)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

fn expand(input: DeriveInput) -> syn::Result<proc_macro2::TokenStream> {
    if !input.generics.params.is_empty() {
        return Err(syn::Error::new_spanned(
            input.generics,
            "AxiomType currently supports non-generic records",
        ));
    }
    let name = input.ident;
    let Data::Struct(data) = input.data else {
        return Err(syn::Error::new_spanned(
            name,
            "AxiomType can only be derived for a struct",
        ));
    };
    let Fields::Named(fields) = data.fields else {
        return Err(syn::Error::new_spanned(
            name,
            "AxiomType requires named fields",
        ));
    };

    let mut encoded = Vec::new();
    let mut decoded = Vec::new();
    let mut initialized = Vec::new();
    let mut wire_names = BTreeSet::new();
    for field in fields.named {
        let ident = field.ident.expect("named field");
        let ty = field.ty;
        let mut wire_name = ident.to_string();
        for attribute in field
            .attrs
            .iter()
            .filter(|value| value.path().is_ident("axiom"))
        {
            attribute.parse_nested_meta(|meta| {
                if meta.path.is_ident("rename") {
                    let value: LitStr = meta.value()?.parse()?;
                    wire_name = value.value();
                    Ok(())
                } else {
                    Err(meta.error("supported AxiomType field option: rename = \"...\""))
                }
            })?;
        }
        if wire_name.is_empty() || wire_name.chars().any(char::is_control) {
            return Err(syn::Error::new_spanned(
                &ident,
                "AxiomType field names must be non-empty and contain no control characters",
            ));
        }
        if !wire_names.insert(wire_name.clone()) {
            return Err(syn::Error::new_spanned(
                &ident,
                format!("duplicate AxiomType wire field `{wire_name}`"),
            ));
        }
        let wire_name = LitStr::new(&wire_name, ident.span());
        encoded.push(quote! {
            (#wire_name, ::axiom_extension_sdk::AxiomEncode::encode(self.#ident))
        });
        decoded.push(quote! {
            let #ident: #ty = record.required(#wire_name)?;
        });
        initialized.push(quote! { #ident });
    }

    Ok(quote! {
        impl ::axiom_extension_sdk::AxiomEncode for #name {
            fn encode(self) -> ::axiom_extension_sdk::abi::Value {
                ::axiom_extension_sdk::__private::canonical_record(
                    [#(#encoded),*].into_iter().collect()
                )
            }
        }

        impl ::axiom_extension_sdk::AxiomDecode for #name {
            fn decode(
                value: &::axiom_extension_sdk::abi::Value,
            ) -> ::axiom_extension_sdk::Result<Self> {
                let mut record = ::axiom_extension_sdk::__private::RecordDecoder::new(value)?;
                #(#decoded)*
                record.finish()?;
                Ok(Self { #(#initialized),* })
            }
        }
    })
}

fn expand_extension(mut implementation: ItemImpl) -> syn::Result<proc_macro2::TokenStream> {
    if implementation.trait_.is_some() {
        return Err(syn::Error::new_spanned(
            &implementation,
            "extension must annotate an inherent impl",
        ));
    }
    if !implementation.generics.params.is_empty() {
        return Err(syn::Error::new_spanned(
            &implementation.generics,
            "extension currently supports non-generic implementation types",
        ));
    }
    let extension_type = implementation.self_ty.clone();
    let mut exports = Vec::new();
    for item in &mut implementation.items {
        let ImplItem::Fn(method) = item else {
            continue;
        };
        let Some((index, attribute)) = method
            .attrs
            .iter()
            .enumerate()
            .find(|(_, attribute)| is_export(attribute))
            .map(|(index, attribute)| (index, attribute.clone()))
        else {
            continue;
        };
        method.attrs.remove(index);
        if method.sig.asyncness.is_some() {
            return Err(syn::Error::new_spanned(
                &method.sig,
                "Rust core-WASM exports are continuation-based and cannot be async functions",
            ));
        }
        if method.sig.inputs.len() != 2 {
            return Err(syn::Error::new_spanned(
                &method.sig,
                "an exported method must take &mut self and one TypedInvocation",
            ));
        }
        let receiver = method.sig.receiver().ok_or_else(|| {
            syn::Error::new_spanned(&method.sig, "an exported method requires &mut self")
        })?;
        if receiver.reference.is_none() || receiver.mutability.is_none() {
            return Err(syn::Error::new_spanned(
                receiver,
                "an exported method receiver must be &mut self",
            ));
        }
        let export_name = export_name(&attribute, &method.sig.ident)?;
        exports.push((export_name, method.sig.ident.clone()));
    }
    if exports.is_empty() {
        return Err(syn::Error::new_spanned(
            &implementation,
            "extension requires at least one #[export] method",
        ));
    }
    let mut names = BTreeSet::new();
    for (name, _) in &exports {
        if !names.insert(name.value()) {
            return Err(syn::Error::new_spanned(
                name,
                format!("duplicate extension export `{}`", name.value()),
            ));
        }
    }
    exports.sort_by(|left, right| left.0.value().cmp(&right.0.value()));
    let export_names: Vec<_> = exports.iter().map(|(name, _)| name).collect();
    let dispatch: Vec<_> = exports
        .iter()
        .map(|(name, method)| quote! { #name => self.#method(invocation) })
        .collect();

    Ok(quote! {
        #implementation

        impl ::axiom_extension_sdk::TypedExtension for #extension_type {
            fn invoke(
                &mut self,
                invocation: ::axiom_extension_sdk::TypedInvocation,
            ) -> ::axiom_extension_sdk::Result<::axiom_extension_sdk::ExtensionResponse> {
                match invocation.export() {
                    #(#dispatch,)*
                    unknown => Err(::axiom_extension_sdk::SdkError::invalid_input(
                        ::axiom_extension_sdk::__private::unknown_export(unknown, AXIOM_EXTENSION_EXPORTS),
                    )),
                }
            }
        }

        #[doc(hidden)]
        pub const AXIOM_EXTENSION_EXPORTS: &[&str] = &[#(#export_names),*];

        pub fn axiom_extension() -> ::axiom_extension_sdk::__private::Box<dyn ::axiom_extension_sdk::Extension> {
            ::axiom_extension_sdk::boxed_extension(<#extension_type as ::core::default::Default>::default())
        }
    })
}

fn is_export(attribute: &Attribute) -> bool {
    attribute
        .path()
        .segments
        .last()
        .is_some_and(|segment| segment.ident == "export")
}

fn export_name(attribute: &Attribute, method: &syn::Ident) -> syn::Result<LitStr> {
    match &attribute.meta {
        Meta::Path(_) => Ok(LitStr::new(&method.to_string(), method.span())),
        Meta::List(list) => {
            if let Ok(name) = list.parse_args::<LitStr>() {
                return valid_export_name(name);
            }
            let mut result = None;
            list.parse_nested_meta(|meta| {
                if !meta.path.is_ident("name") {
                    return Err(meta.error("supported export option: name = \"...\""));
                }
                let expression: Expr = meta.value()?.parse()?;
                let Expr::Lit(ExprLit {
                    lit: Lit::Str(name),
                    ..
                }) = expression
                else {
                    return Err(meta.error("export name must be a string literal"));
                };
                result = Some(valid_export_name(name)?);
                Ok(())
            })?;
            result.ok_or_else(|| syn::Error::new_spanned(attribute, "export name is missing"))
        }
        Meta::NameValue(_) => Err(syn::Error::new_spanned(
            attribute,
            "use #[export] or #[export(name = \"...\")]",
        )),
    }
}

fn valid_export_name(name: LitStr) -> syn::Result<LitStr> {
    let value = name.value();
    if value.is_empty()
        || value.len() > 128
        || !value.chars().all(|character| {
            character.is_ascii_alphanumeric() || character == '_' || character == '-'
        })
    {
        return Err(syn::Error::new_spanned(
            name,
            "export names must contain 1-128 ASCII letters, digits, '_' or '-'",
        ));
    }
    Ok(name)
}
