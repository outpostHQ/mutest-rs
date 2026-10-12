//@ edition: 2024

#![crate_type = "proc-macro"]

#![feature(proc_macro_quote)]

extern crate proc_macro;

use proc_macro::{TokenStream, TokenTree, quote};

/// Like the test attribute of an async runtime: makes a plain test of an `async` function, and refuses any other.
#[proc_macro_attribute]
pub fn test(_args: TokenStream, input: TokenStream) -> TokenStream {
    let mut tokens = input.into_iter();
    match tokens.next() {
        Some(TokenTree::Ident(ident)) if ident.to_string() == "async" => {}
        _ => panic!("the async keyword is missing from the function declaration"),
    }
    let plain_fn = tokens.collect::<TokenStream>();

    quote! {
        #[::core::prelude::v1::test]
        $plain_fn
    }
}
