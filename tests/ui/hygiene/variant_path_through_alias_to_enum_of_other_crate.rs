//@ build
//@ stderr: empty
//@ aux-build: enum_behind_alias.rs
//@ aux-build: enum_alias.rs
//@ rustc-flags: --extern enum_alias

//! This test ensures that a path to an enum variant or an associated item through a type alias keeps the alias
//! when no path names the enum here, because only the crate of the alias depends on the crate of the enum.

#![allow(unused)]

use enum_alias::error::SdkError;

fn code(err: &enum_alias::error::SdkError<u8>) -> Option<u8> {
    match err {
        // TEST: Tuple variant pattern through the full path to the alias.
        enum_alias::error::SdkError::ServiceError(code) => Some(*code),
        // TEST: Struct variant pattern through the imported alias.
        SdkError::Response { raw: () } => None,
        // TEST: Unit variant pattern through the imported alias.
        SdkError::Timeout => None,
    }
}

#[test]
fn test() {
    // TEST: Variant constructor through the alias, with generic args written for the alias.
    let err = enum_alias::error::SdkError::<u8>::ServiceError(7);
    assert_eq!(code(&err), Some(7));

    // TEST: Unit variant through the alias as a qualified self type.
    let _: SdkError<u8> = <SdkError<u8>>::Timeout;

    // TEST: Inherent associated function through the alias, with and without generic args written for the alias.
    assert_eq!(code(&enum_alias::error::SdkError::<u8>::service(8)), Some(8));
    let _: SdkError<u8> = SdkError::service(9);

    // TEST: Trait associated function through the alias, with and without generic args written for the alias.
    assert_eq!(code(&enum_alias::error::SdkError::<u8>::default()), None);
    let _: SdkError<u8> = SdkError::default();
}
