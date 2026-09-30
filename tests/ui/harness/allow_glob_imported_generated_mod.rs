//@ build
//@ stderr: empty
//@ aux-build: crate_with_generated_mod.rs

//! The injected module shadows a glob-imported one, such as another crate's injected module.

extern crate crate_with_generated_mod;

pub use crate_with_generated_mod::*;
