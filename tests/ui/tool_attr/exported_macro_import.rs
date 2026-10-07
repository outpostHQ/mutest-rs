//@ build
//@ stderr: empty

#![allow(unused)]

mod macros {
    #[macro_export]
    #[clippy::format_args]
    macro_rules! anyhow {
        ($msg:literal) => { $msg.len() };
    }
}

// TEST: The generated code can import a `#[macro_export]` macro with a tool attribute,
//       after it expands the last macro call in the crate root.
pub use anyhow as format_err;

#[derive(Clone)]
pub struct Error;

mod tests {
    #[test]
    fn test() {
        assert_eq!(1, crate::format_err!("x"));
    }
}
