//@ build
//@ stderr: empty

#![feature(decl_macro)]

use std::fmt::Debug;

macro m() {
    // TEST: The associated type of a return-position `impl Trait` in a trait has no name, so it must be inferred.
    trait Build: 'static {
        fn build(self, prefix: &str) -> impl Debug + 'static;
    }

    impl Build for u32 {
        fn build(self, prefix: &str) -> impl Debug + 'static { format!("{prefix}{self}") }
    }

    trait DynBuild {
        fn build_boxed(self: Box<Self>, prefix: &str) -> Box<dyn Debug>;
    }

    impl<T: Build> DynBuild for T {
        fn build_boxed(self: Box<Self>, prefix: &str) -> Box<dyn Debug> {
            let built: Box<dyn Debug> = Box::new(Build::build(*self, prefix));
            built
        }
    }

    assert_eq!(format!("{:?}", DynBuild::build_boxed(Box::new(1_u32), "n")), "\"n1\"");
}

#[test]
fn test() {
    m!();
}
