//! Macro del framework. Fase 1: `#[model]` è uno STUB identità che riserva l'API.
//!
//! Fase 2 (roadmap): genererà da `struct SaleOrder { ... }` il `ModelDescriptor` statico
//! + `impl Model` + accessor tipizzati, eliminando la definizione a mano dei descrittori.
//! È volutamente lasciata per ultima: prima si provano i trait runtime, poi la codegen.

use proc_macro::TokenStream;

/// Attribute macro `#[model(name = "...", table = "...")]`.
/// ponytail: identità per ora — passa il token stream invariato. Upgrade alla fase 2.
#[proc_macro_attribute]
pub fn model(_attr: TokenStream, item: TokenStream) -> TokenStream {
    item
}
