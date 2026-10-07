//@ build
//@ stderr

//! A crate that denies warnings can be mutation tested on a toolchain that adds lints or deprecations.

#![deny(deprecated)]

#[deprecated]
fn old() {}

#[test]
fn test() {
    old();
}
