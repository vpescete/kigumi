//! Macro del framework. Fase 2: `#[model]` genera il `ModelDescriptor` statico + `impl Model`
//! da una struct annotata, eliminando i descrittori scritti a mano.
//!
//! La struct in input è un DSL dichiarativo: i suoi "tipi" di campo (`Text`, `Many2one`, …)
//! sono parole chiave che la macro mappa su `FieldKind`; la struct originale viene SOSTITUITA
//! da un marker type (`pub struct SaleOrder;`) su cui l'utente può scrivere i metodi.
//!
//! Il codice generato usa path assoluti `::meshble::prelude::…`, quindi i moduli devono
//! dipendere dal crate facade `meshble` (è già la convenzione del workspace).

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use syn::{
    parse_macro_input, punctuated::Punctuated, Data, DeriveInput, Expr, ExprLit, Fields, Lit,
    LitStr, Meta, MetaNameValue, Token, Type,
};

/// `#[model(name = "sale.order", table = "sale_order")]`
#[proc_macro_attribute]
pub fn model(attr: TokenStream, item: TokenStream) -> TokenStream {
    let args =
        parse_macro_input!(attr with Punctuated::<Meta, Token![,]>::parse_terminated);
    let args: Vec<Meta> = args.into_iter().collect();
    let input = parse_macro_input!(item as DeriveInput);

    match expand(&args, &input) {
        Ok(ts) => ts.into(),
        Err(e) => e.to_compile_error().into(),
    }
}

fn expand(args: &[Meta], input: &DeriveInput) -> syn::Result<TokenStream2> {
    let model_name = meta_str(args, "name")
        .ok_or_else(|| err(&input.ident, "#[model] richiede `name = \"...\"`"))?;
    let table = meta_str(args, "table").unwrap_or_else(|| model_name.replace('.', "_"));

    let fields = match &input.data {
        Data::Struct(s) => match &s.fields {
            Fields::Named(n) => &n.named,
            _ => return Err(err(&input.ident, "#[model] richiede una struct con campi nominati")),
        },
        _ => return Err(err(&input.ident, "#[model] si applica solo a una struct")),
    };

    let mut field_toks: Vec<TokenStream2> = Vec::new();
    for f in fields {
        field_toks.push(build_field(f)?);
    }

    let vis = &input.vis;
    let ident = &input.ident;
    Ok(quote! {
        #vis struct #ident;

        impl ::meshble::prelude::Model for #ident {
            fn descriptor() -> &'static ::meshble::prelude::ModelDescriptor {
                static D: ::meshble::prelude::ModelDescriptor =
                    ::meshble::prelude::ModelDescriptor {
                        name: #model_name,
                        table: #table,
                        fields: &[ #(#field_toks),* ],
                    };
                &D
            }
        }

        // Auto-registrazione nel catalogo: niente wiring manuale.
        ::meshble::inventory::submit! {
            ::meshble::prelude::ModelRegistration {
                name: #model_name,
                module: ::core::module_path!(),
                descriptor: <#ident as ::meshble::prelude::Model>::descriptor,
            }
        }
    })
}

/// `#[extend("sale.order")]` — aggiunge campi a un modello definito altrove.
/// Le estensioni si auto-registrano e vengono fuse da `resolve_registered`.
#[proc_macro_attribute]
pub fn extend(attr: TokenStream, item: TokenStream) -> TokenStream {
    let target = parse_macro_input!(attr as LitStr);
    let input = parse_macro_input!(item as DeriveInput);
    match expand_extend(&target, &input) {
        Ok(ts) => ts.into(),
        Err(e) => e.to_compile_error().into(),
    }
}

fn expand_extend(target: &LitStr, input: &DeriveInput) -> syn::Result<TokenStream2> {
    let fields = match &input.data {
        Data::Struct(s) => match &s.fields {
            Fields::Named(n) => &n.named,
            _ => return Err(err(&input.ident, "#[extend] richiede una struct con campi nominati")),
        },
        _ => return Err(err(&input.ident, "#[extend] si applica solo a una struct")),
    };

    let mut field_toks: Vec<TokenStream2> = Vec::new();
    for f in fields {
        field_toks.push(build_field(f)?);
    }

    let vis = &input.vis;
    let ident = &input.ident;
    let target_str = target.value();
    Ok(quote! {
        #vis struct #ident;

        impl #ident {
            pub fn fields() -> &'static [::meshble::prelude::FieldDef] {
                static F: &[::meshble::prelude::FieldDef] = &[ #(#field_toks),* ];
                F
            }
        }

        ::meshble::inventory::submit! {
            ::meshble::prelude::FieldExtension {
                target: #target_str,
                module: ::core::module_path!(),
                fields: <#ident>::fields,
            }
        }
    })
}

fn build_field(f: &syn::Field) -> syn::Result<TokenStream2> {
    let fname = f
        .ident
        .as_ref()
        .ok_or_else(|| err(f, "campo senza nome"))?
        .to_string();

    // Raccogli i meta di #[field(...)].
    let mut metas: Vec<Meta> = Vec::new();
    for a in &f.attrs {
        if a.path().is_ident("field") {
            let parsed = a.parse_args_with(Punctuated::<Meta, Token![,]>::parse_terminated)?;
            metas.extend(parsed);
        }
    }

    // Il "tipo" del campo nel DSL → variante FieldKind.
    let kind_name = match &f.ty {
        Type::Path(tp) => tp.path.segments.last().unwrap().ident.to_string(),
        _ => return Err(err(&f.ty, "tipo di campo non riconosciuto")),
    };

    let kind = match kind_name.as_str() {
        "Text" => quote! { ::meshble::prelude::FieldKind::Text },
        "Integer" => quote! { ::meshble::prelude::FieldKind::Integer },
        "Bool" => quote! { ::meshble::prelude::FieldKind::Bool },
        "Decimal" => {
            let cur = match meta_str(&metas, "currency") {
                Some(c) => quote! { Some(#c) },
                None => quote! { None },
            };
            quote! { ::meshble::prelude::FieldKind::Decimal { currency_field: #cur } }
        }
        "Selection" => {
            let sel = meta_str(&metas, "selection")
                .ok_or_else(|| err(&f.ty, "Selection richiede `selection = \"k:Label,...\"`"))?;
            let pairs: Vec<TokenStream2> = sel
                .split(',')
                .filter(|p| !p.trim().is_empty())
                .map(|p| {
                    let mut it = p.splitn(2, ':');
                    let k = it.next().unwrap_or("").trim().to_string();
                    let v = it.next().unwrap_or("").trim().to_string();
                    quote! { (#k, #v) }
                })
                .collect();
            quote! { ::meshble::prelude::FieldKind::Selection(&[ #(#pairs),* ]) }
        }
        "Many2one" => {
            let target = meta_str(&metas, "target")
                .ok_or_else(|| err(&f.ty, "Many2one richiede `target = \"model.name\"`"))?;
            quote! { ::meshble::prelude::FieldKind::Many2one { target: #target } }
        }
        "One2many" => {
            let target = meta_str(&metas, "target")
                .ok_or_else(|| err(&f.ty, "One2many richiede `target = \"...\"`"))?;
            let inverse = meta_str(&metas, "inverse")
                .ok_or_else(|| err(&f.ty, "One2many richiede `inverse = \"...\"`"))?;
            quote! { ::meshble::prelude::FieldKind::One2many { target: #target, inverse: #inverse } }
        }
        other => {
            return Err(err(
                &f.ty,
                &format!("tipo di campo sconosciuto: `{other}` (usa Text/Integer/Decimal/Bool/Selection/Many2one/One2many)"),
            ))
        }
    };

    let label = meta_str(&metas, "label").unwrap_or_else(|| fname.clone());
    let required = meta_flag(&metas, "required");
    let compute = meta_str(&metas, "compute");
    // stored: one2many mai; computed solo con `store`; altrimenti sì.
    let stored = if kind_name == "One2many" {
        false
    } else if compute.is_some() {
        meta_flag(&metas, "store")
    } else {
        true
    };
    let compute_tok = match &compute {
        Some(c) => quote! { Some(#c) },
        None => quote! { None },
    };
    let depends: Vec<String> = match meta_str(&metas, "depends") {
        Some(s) => s
            .split(',')
            .map(|x| x.trim().to_string())
            .filter(|x| !x.is_empty())
            .collect(),
        None => vec![],
    };

    Ok(quote! {
        ::meshble::prelude::FieldDef {
            name: #fname,
            label: #label,
            kind: #kind,
            required: #required,
            stored: #stored,
            compute: #compute_tok,
            depends: &[ #(#depends),* ],
        }
    })
}

fn meta_str(metas: &[Meta], key: &str) -> Option<String> {
    metas.iter().find_map(|m| match m {
        Meta::NameValue(MetaNameValue {
            path,
            value: Expr::Lit(ExprLit { lit: Lit::Str(s), .. }),
            ..
        }) if path.is_ident(key) => Some(s.value()),
        _ => None,
    })
}

fn meta_flag(metas: &[Meta], key: &str) -> bool {
    metas
        .iter()
        .any(|m| matches!(m, Meta::Path(p) if p.is_ident(key)))
}

fn err<T: quote::ToTokens>(tokens: T, msg: &str) -> syn::Error {
    syn::Error::new_spanned(tokens, msg)
}
