//@ build
//@ stderr: empty
//@ aux-build: proc_macro_generated_imports.rs
//@ aux-build: generated_client_lib.rs

//! Imports a procedural macro generates, rooted at an extern crate, `crate`, `self` and `self::super`.
//! Items it writes at call-site (a transparent mark) in another crate keep their names.

extern crate generated_client_lib;
extern crate proc_macro_generated_imports;

pub mod client {
    pub mod inner {
        pub struct Marker;
    }

    proc_macro_generated_imports::generate_client!();
}

pub fn extern_items() -> (generated_client_lib::client::Client, String) {
    (<generated_client_lib::client::Client>::new(), generated_client_lib::client::render())
}
