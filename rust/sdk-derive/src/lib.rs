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
///
/// An exported `async fn name(cx: &Cx, input: In) -> Result<Out>` (no
/// receiver: no guest state survives between ABI 1.1 rounds) is an async
/// export: it runs on the SDK's replaying executor (`axiom_extension_sdk::
/// async_rt`), `In: AxiomDecode`, `Out: AxiomEncode`. An impl with at least
/// one async export makes `axiom_extension()` return an `AsyncExtension`, so
/// the module exports and negotiates ABI 1.1; its sync exports keep working
/// (inside 1.1 rounds). An impl with only sync exports is unchanged (ABI 1.0).
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
    let mut async_exports = Vec::new();
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
            check_async_export(&method.sig)?;
            let export_name = export_name(&attribute, &method.sig.ident)?;
            async_exports.push(AsyncExport {
                name: export_name,
                method: method.sig.ident.clone(),
                value_input: value_input_type(&method.sig),
            });
            continue;
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
    if exports.is_empty() && async_exports.is_empty() {
        return Err(syn::Error::new_spanned(
            &implementation,
            "extension requires at least one #[export] method",
        ));
    }
    let mut names = BTreeSet::new();
    for name in exports.iter().map(|(name, _)| name).chain(async_exports.iter().map(|export| &export.name)) {
        if !names.insert(name.value()) {
            return Err(syn::Error::new_spanned(
                name,
                format!("duplicate extension export `{}`", name.value()),
            ));
        }
    }
    exports.sort_by_key(|export| export.0.value());
    if !async_exports.is_empty() {
        return Ok(expand_async_extension(implementation, exports, async_exports));
    }
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

/// One `#[export] async fn`.
struct AsyncExport {
    name: LitStr,
    method: syn::Ident,
    /// The input parameter's type when it is spelled `Value` (any path ending
    /// in `Value`): the executor moves the input into the argument instead of
    /// cloning it every round. Whether it really is `abi::Value` is checked
    /// with `TypeId` in the generated code; other types decode as before.
    value_input: Option<syn::Type>,
}

/// The type of the input parameter if its last path segment is `Value`.
fn value_input_type(signature: &syn::Signature) -> Option<syn::Type> {
    let Some(syn::FnArg::Typed(argument)) = signature.inputs.iter().nth(1) else {
        return None;
    };
    match &*argument.ty {
        syn::Type::Path(path)
            if path.qself.is_none()
                && path.path.segments.last().is_some_and(|segment| {
                    segment.ident == "Value" && segment.arguments.is_empty()
                }) =>
        {
            Some((*argument.ty).clone())
        }
        _ => None,
    }
}

/// `async fn name(cx: &Cx, input: In) -> Result<Out>`: no receiver, a
/// reference context and exactly one input.
fn check_async_export(signature: &syn::Signature) -> syn::Result<()> {
    if let Some(receiver) = signature.receiver() {
        return Err(syn::Error::new_spanned(
            receiver,
            "an async export takes no self: no guest state survives between ABI 1.1 rounds; write `async fn name(cx: &Cx, input: In) -> Result<Out>`",
        ));
    }
    if !signature.generics.params.is_empty() {
        return Err(syn::Error::new_spanned(
            &signature.generics,
            "an async export cannot be generic",
        ));
    }
    let shape = "an async export must be `async fn name(cx: &Cx, input: In) -> Result<Out>`";
    if signature.inputs.len() != 2 {
        return Err(syn::Error::new_spanned(signature, shape));
    }
    match signature.inputs.first() {
        Some(syn::FnArg::Typed(argument)) if matches!(*argument.ty, syn::Type::Reference(ref r) if r.mutability.is_none()) => {}
        _ => return Err(syn::Error::new_spanned(&signature.inputs, shape)),
    }
    if matches!(signature.output, syn::ReturnType::Default) {
        return Err(syn::Error::new_spanned(signature, shape));
    }
    Ok(())
}

fn expand_async_extension(
    implementation: ItemImpl,
    mut exports: Vec<(LitStr, syn::Ident)>,
    mut async_exports: Vec<AsyncExport>,
) -> proc_macro2::TokenStream {
    let extension_type = implementation.self_ty.clone();
    exports.sort_by_key(|export| export.0.value());
    async_exports.sort_by_key(|export| export.name.value());
    let mut all: Vec<_> = exports
        .iter()
        .map(|(name, _)| name.clone())
        .chain(async_exports.iter().map(|export| export.name.clone()))
        .collect();
    all.sort_by_key(LitStr::value);
    let async_names: Vec<_> = async_exports.iter().map(|export| &export.name).collect();
    let moves: Vec<_> = async_exports
        .iter()
        .filter_map(|export| {
            let name = &export.name;
            export.value_input.as_ref().map(|ty| {
                quote! {
                    #name => ::core::any::TypeId::of::<#ty>()
                        == ::core::any::TypeId::of::<::axiom_extension_sdk::abi::Value>(),
                }
            })
        })
        .collect();
    let dispatch: Vec<_> = exports
        .iter()
        .map(|(name, method)| quote! { #name => self.#method(invocation) })
        .collect();
    let async_dispatch: Vec<_> = async_exports
        .iter()
        .map(|AsyncExport { name, method, value_input }| {
            let input = match value_input {
                Some(ty) => quote! { cx.__export_input::<#ty>()? },
                None => quote! { cx.input()? },
            };
            quote! {
                #name => {
                    let future: ::axiom_extension_sdk::LocalBoxFuture<'cx, ::axiom_extension_sdk::Result<::axiom_extension_sdk::abi::Value>> =
                        ::axiom_extension_sdk::__private::Box::pin(async move {
                            let output = <#extension_type>::#method(cx, #input).await?;
                            ::core::result::Result::<::axiom_extension_sdk::abi::Value, ::axiom_extension_sdk::SdkError>::Ok(::axiom_extension_sdk::AxiomEncode::encode(output))
                        });
                    ::core::option::Option::Some(future)
                }
            }
        })
        .collect();
    quote! {
        #implementation

        impl ::axiom_extension_sdk::TypedExtension for #extension_type {
            fn invoke(
                &mut self,
                invocation: ::axiom_extension_sdk::TypedInvocation,
            ) -> ::axiom_extension_sdk::Result<::axiom_extension_sdk::ExtensionResponse> {
                match invocation.export() {
                    #(#dispatch,)*
                    #(#async_names)|* => Err(::axiom_extension_sdk::SdkError::invalid_input(
                        "this export is async; it runs only through the ABI 1.1 executor",
                    )),
                    unknown => Err(::axiom_extension_sdk::SdkError::invalid_input(
                        ::axiom_extension_sdk::__private::unknown_export(unknown, AXIOM_EXTENSION_EXPORTS),
                    )),
                }
            }
        }

        impl ::axiom_extension_sdk::__private::AsyncExports for #extension_type {
            const ASYNC_EXPORTS: &'static [&'static str] = &[#(#async_names),*];

            fn dispatch_async<'cx>(
                cx: &'cx ::axiom_extension_sdk::Cx,
                export: &str,
            ) -> ::core::option::Option<::axiom_extension_sdk::LocalBoxFuture<'cx, ::axiom_extension_sdk::Result<::axiom_extension_sdk::abi::Value>>> {
                match export {
                    #(#async_dispatch)*
                    _ => ::core::option::Option::None,
                }
            }

            fn input_moves(export: &str) -> bool {
                match export {
                    #(#moves)*
                    _ => false,
                }
            }
        }

        #[doc(hidden)]
        pub const AXIOM_EXTENSION_EXPORTS: &[&str] = &[#(#all),*];

        pub fn axiom_extension() -> ::axiom_extension_sdk::AsyncExtension {
            ::axiom_extension_sdk::AsyncExtension::new(<#extension_type as ::core::default::Default>::default())
        }
    }
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
                if result.is_some() {
                    // w2-d4: a repeated `name` used to keep the last one silently; the
                    // X9a analyser follows the macro (last wins) but a reader takes the first.
                    return Err(meta.error("duplicate export option `name`"));
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

#[cfg(test)]
mod tests {
    use super::*;

    fn name_of(attribute: Attribute) -> syn::Result<String> {
        let method: syn::Ident = syn::parse_quote!(run);
        export_name(&attribute, &method).map(|name| name.value())
    }

    #[test]
    fn an_export_takes_one_name() {
        assert_eq!(name_of(syn::parse_quote!(#[export])).unwrap(), "run");
        assert_eq!(name_of(syn::parse_quote!(#[export("quote")])).unwrap(), "quote");
        assert_eq!(name_of(syn::parse_quote!(#[export(name = "quote")])).unwrap(), "quote");
    }

    #[test]
    fn a_repeated_export_name_is_refused() {
        // w2-d4 / w2-i: `name` given twice used to keep the last one silently.
        for attribute in [
            syn::parse_quote!(#[export(name = "a", name = "b")]),
            syn::parse_quote!(#[export(name = "a", name = "a")]),
            syn::parse_quote!(#[export(name = "a", name = "b", name = "c")]),
        ] {
            let error = name_of(attribute).unwrap_err();
            assert!(error.to_string().contains("duplicate export option `name`"), "{error}");
        }
    }
}
