//@ build
//@ stderr: empty

// NOTE: The test harness injects `extern crate test` into the crate root, next to this module.
mod test {
    pub fn one() -> u32 { 1 }
}

mod math {
    pub fn add(a: u32, b: u32) -> u32 { a + b }

    #[test]
    fn adds() {
        assert_eq!(add(super::test::one(), 1), 2);
    }
}
