#![crate_type = "proc-macro"]

extern crate proc_macro;
use proc_macro::TokenStream;

#[proc_macro]
pub fn generate_client(_input: TokenStream) -> TokenStream {
    r#"
        #[allow(unused_imports)]
        use std::fmt::{Debug, Display};
        #[allow(unused_imports)]
        pub use ::std::fmt::Write;
        pub use crate::client::inner::Marker as Replaced;
        #[allow(unused_imports)]
        use self::inner::Marker;

        pub struct Client;

        pub mod prelude {
            pub use self::super::Client;
        }

        pub fn render() -> String {
            use ::std::fmt::Write as _;
            let mut rendered = String::new();
            write!(rendered, "{}", 1).unwrap();
            rendered
        }
    "#.parse().unwrap()
}
