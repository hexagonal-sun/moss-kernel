use proc_macro::TokenStream;
use quote::quote;
use syn::{ItemFn, parse_macro_input};

#[proc_macro_attribute]
pub fn ktest(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let item = parse_macro_input!(item as ItemFn);

    // Parse the list of variables the user wanted to print.

    TokenStream::from(quote! {
        crate::ktest_impl! {
            #item
        }
    })
}
