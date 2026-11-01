#![crate_type = "lib"]
extern crate proc_macro_generated_imports;
pub mod client {
    pub mod inner { pub struct Marker; }
    proc_macro_generated_imports::generate_client!();
}
