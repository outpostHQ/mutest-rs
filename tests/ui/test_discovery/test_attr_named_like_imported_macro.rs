//@ build
//@ stderr: empty
//@ aux-build: async_test_macros.rs

//! A test stays a test where an import gives the name `test` to another attribute macro.

extern crate async_test_macros;

fn mutable_fn() -> u32 {
    1
}

mod tests {
    use async_test_macros::test;

    #[async_test_macros::test]
    async fn test_made_by_imported_macro() {
        assert_eq!(1, super::mutable_fn());
    }

    #[test]
    async fn test_made_by_imported_macro_through_its_short_name() {
        assert_eq!(1, super::mutable_fn());
    }
}
