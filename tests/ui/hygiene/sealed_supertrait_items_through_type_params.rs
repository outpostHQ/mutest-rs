//@ build
//@ stderr: empty

mod encoding {
    mod sealed {
        pub trait Sealed {
            fn from_static(value: &'static str) -> u8;
        }
    }

    pub trait Encoding: self::sealed::Sealed {}

    pub struct Ascii;

    impl sealed::Sealed for Ascii {
        fn from_static(value: &'static str) -> u8 { value.len() as u8 }
    }

    impl Encoding for Ascii {}
}

mod value {
    use std::marker::PhantomData;

    use super::encoding::Encoding;

    pub struct Value<E: Encoding> {
        inner: u8,
        phantom: PhantomData<E>,
    }

    impl<E: Encoding> Value<E> {
        pub fn from_static(src: &'static str) -> Self {
            // TEST: No path names the sealed supertrait here, so the path to its method must stay type-relative.
            Value { inner: E::from_static(src), phantom: PhantomData }
        }

        pub fn inner(&self) -> u8 { self.inner }
    }
}

#[test]
fn test() {
    assert_eq!(value::Value::<encoding::Ascii>::from_static("ab").inner(), 2);
}
