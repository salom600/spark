//! Procedural macros for spark.
//!
//! The centerpiece is `#[derive(ComponentDef)]`: from a single struct or enum
//! definition it generates the egui inspector body (per-field widgets, variant
//! switching for enums) that the editor needs, plus the matching `Inspect`
//! impl so the type can be edited *nested inside* other components. Combined
//! with the generic component registry in `spark::ecs`, one declaration is the
//! *only* thing a component author writes — no serialization glue, no
//! inspector UI, no registration boilerplate.
//!
//! Generated code is ordinary, readable Rust — inspect it with `cargo expand`.

use proc_macro::TokenStream;
use quote::quote;
use syn::{Data, DeriveInput, Fields, parse_macro_input};

/// Humanize a field/variant ident for display: `base_color` -> `Base color`.
fn humanize(ident: &proc_macro2::Ident) -> String {
    let raw = ident.to_string();
    let mut out = String::with_capacity(raw.len() + 4);
    for (i, part) in raw.split('_').enumerate() {
        if i > 0 {
            out.push(' ');
        }
        let mut chars = part.chars();
        if let Some(first) = chars.next() {
            out.extend(first.to_uppercase());
            out.push_str(chars.as_str());
        }
    }
    out
}

/// True when the field carries `#[inspector(skip)]`.
fn skipped(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|a| {
        a.path().is_ident("inspector")
            && a.meta
                .require_list()
                .is_ok_and(|l| l.tokens.to_string() == "skip")
    })
}

/// Derive `spark::ecs::ComponentDef` (+ `Inspect`) for a struct or enum.
///
/// * On **structs**: generates `NAME` and an `inspect` that lays fields out in
///   a two-column egui grid, editing each field via [`spark::ecs::Inspect`].
/// * On **enums**: additionally generates `variant_name` and an `inspect` with
///   a variant switcher (constructing the newly selected variant with
///   `Default::default()` per field) followed by the fields of the active
///   variant.
///
/// Fields annotated `#[inspector(skip)]` are omitted from the generated UI.
/// Tuple variants are rejected — use struct variants so fields stay named.
#[proc_macro_derive(ComponentDef, attributes(inspector))]
pub fn derive_component_def(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    let name = &input.ident;
    let type_name = name.to_string();

    let (inspect_body, variant_name_impl) = match &input.data {
        Data::Struct(data) => {
            let fields = named_fields(&data.fields, &type_name);
            let rows = fields.iter().filter(|f| !skipped(&f.attrs)).map(|f| {
                let label = humanize(f.ident.as_ref().unwrap());
                let field = f.ident.as_ref().unwrap();
                quote! {
                    ui.strong(#label);
                    changed |= ::spark::ecs::Inspect::inspect(&mut self.#field, ui);
                    ui.end_row();
                }
            });
            let body = quote! {
                let mut changed = false;
                ::spark::reexport::egui::Grid::new(#type_name)
                    .num_columns(2)
                    .show(ui, |ui| { #(#rows)* });
                changed
            };
            (body, quote! {})
        }
        Data::Enum(data) => {
            // Variant switcher: selectable labels; selecting a different variant
            // constructs it with per-field `Default::default()`.
            let switch_items = data.variants.iter().map(|v| {
                let vname = &v.ident;
                let label = humanize(&v.ident);
                let construct = default_construct(v);
                quote! {
                    if ui.selectable_label(matches!(self, #name::#vname { .. }), #label).clicked() {
                        *self = #name::#construct;
                        changed = true;
                    }
                }
            });
            // Field editors for the active variant (binds &mut via match
            // ergonomics), mutating the outer `changed`.
            let arms = data.variants.iter().map(|v| {
                let vname = &v.ident;
                let bind = variant_bindings(v);
                let rows = variant_field_rows(v, &type_name);
                quote! { #name::#vname #bind => { #rows } }
            });
            let vn_name = data.variants.iter().map(|v| {
                let vname = &v.ident;
                let label = vname.to_string();
                quote! { #name::#vname { .. } => #label }
            });
            let body = quote! {
                let mut changed = false;
                ::spark::reexport::egui::ComboBox::from_id_salt(concat!(#type_name, "::variant"))
                    .selected_text(self.variant_name())
                    .show_ui(ui, |ui| { #(#switch_items)* });
                match self { #(#arms)* }
                changed
            };
            let variant_impl = quote! {
                fn variant_name(&self) -> &'static str {
                    match self { #(#vn_name),* }
                }
            };
            (body, variant_impl)
        }
        Data::Union(_) => {
            return syn::Error::new_spanned(&input.ident, "ComponentDef does not support unions")
                .to_compile_error()
                .into();
        }
    };

    let expanded = quote! {
        impl ::spark::ecs::ComponentDef for #name {
            const NAME: &'static str = #type_name;
            #variant_name_impl

            fn inspect(&mut self, ui: &mut ::spark::reexport::egui::Ui) -> bool {
                #inspect_body
            }
        }

        // Nested usability: any ComponentDef type can be edited inside another
        // component's generated inspector.
        impl ::spark::ecs::Inspect for #name {
            fn inspect(&mut self, ui: &mut ::spark::reexport::egui::Ui) -> bool {
                <Self as ::spark::ecs::ComponentDef>::inspect(self, ui)
            }
        }
    };
    TokenStream::from(expanded)
}

/// Validate that fields are named; tuple/unit structs are rejected (components
/// need named fields for readable scene files).
fn named_fields<'a>(fields: &'a Fields, type_name: &str) -> Vec<&'a syn::Field> {
    match fields {
        Fields::Named(f) => f.named.iter().collect(),
        _ => panic!(
            "ComponentDef on `{type_name}` requires named fields \
             (scene files and the inspector rely on field names)"
        ),
    }
}

/// Build `Variant { a: Default::default(), .. }` for enum variant switching.
fn default_construct(variant: &syn::Variant) -> proc_macro2::TokenStream {
    let vname = &variant.ident;
    match &variant.fields {
        Fields::Named(f) => {
            let inits = f.named.iter().map(|fl| {
                let id = fl.ident.as_ref().unwrap();
                quote! { #id: ::std::default::Default::default() }
            });
            quote! { #vname { #(#inits),* } }
        }
        Fields::Unit => quote! { #vname },
        Fields::Unnamed(_) => {
            panic!("ComponentDef enums must use struct variants (found tuple variant)")
        }
    }
}

/// Binding pattern for a variant in a `match` arm: `Variant { a, b }`.
fn variant_bindings(variant: &syn::Variant) -> proc_macro2::TokenStream {
    match &variant.fields {
        Fields::Named(f) => {
            let ids = f.named.iter().map(|fl| fl.ident.as_ref().unwrap());
            quote! { { #(#ids),* } }
        }
        Fields::Unit => quote! {},
        Fields::Unnamed(_) => {
            panic!("ComponentDef enums must use struct variants (found tuple variant)")
        }
    }
}

/// Per-field edit rows for the active enum variant. Mutates the outer `changed`.
fn variant_field_rows(variant: &syn::Variant, type_name: &str) -> proc_macro2::TokenStream {
    if let Fields::Named(f) = &variant.fields {
        let rows = f.named.iter().filter(|fl| !skipped(&fl.attrs)).map(|fl| {
            let label = humanize(fl.ident.as_ref().unwrap());
            let field = fl.ident.as_ref().unwrap();
            quote! {
                ui.strong(#label);
                changed |= ::spark::ecs::Inspect::inspect(#field, ui);
                ui.end_row();
            }
        });
        quote! {
            ::spark::reexport::egui::Grid::new(concat!(#type_name, "::fields"))
                .num_columns(2)
                .show(ui, |ui| { #(#rows)* });
        }
    } else {
        quote! {}
    }
}
