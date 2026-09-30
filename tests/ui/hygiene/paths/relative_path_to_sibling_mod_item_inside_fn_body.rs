//@ build
//@ stderr: empty

#![feature(decl_macro)]

macro m() {
    // TEST: Relative path through `super` to an item in a sibling module, where no path from the crate root exists.
    pub fn f() {
        mod types {
            pub mod error {
                pub struct ConversionError;
            }

            pub mod builder {
                pub fn convert() -> super::error::ConversionError {
                    super::error::ConversionError
                }
            }
        }

        let _ = types::builder::convert();
    }
}

m!();
