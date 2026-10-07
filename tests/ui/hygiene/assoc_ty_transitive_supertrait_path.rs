//@ build
//@ stderr: empty

#![feature(decl_macro)]

#![allow(unused)]

mod private {
    pub trait Sealed { type Buffer: Default + AsRef<[u8]>; }
    impl Sealed for u8 { type Buffer = [u8; 3]; }
}

pub trait Integer: private::Sealed {}
impl Integer for u8 {}

// TEST: Paths to assoc items with implicit Self roots may point to assoc items in supertraits of supertraits.
trait Unsigned: Integer {
    fn fmt(self, buf: &Self::Buffer) -> usize;
}
impl Unsigned for u8 {
    fn fmt(self, buf: &Self::Buffer) -> usize {
        let empty: Self::Buffer = Default::default();
        buf.as_ref().len() - empty.as_ref().len()
    }
}

macro m() {
    trait Signed: Integer {
        fn fmt(self, buf: &Self::Buffer) -> usize;
    }
    impl Signed for u8 {
        fn fmt(self, buf: &Self::Buffer) -> usize { buf.as_ref().len() }
    }
}

m!();
