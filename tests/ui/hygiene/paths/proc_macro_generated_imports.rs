//@ build
//@ stderr: empty
//@ aux-build: proc_macro_generated_imports.rs

//! Imports a procedural macro generates, rooted at an extern crate, `crate`, `self` and `self::super`.

extern crate proc_macro_generated_imports;

pub mod client {
    pub mod inner {
        pub struct Marker;
    }

    proc_macro_generated_imports::generate_client!();
}
