//@ build
//@ stderr: empty
//@ aux-build: shared_dep.rs
//@ aux-build: shared_dep_user.rs
//@ rustc-flags: --extern shared_dep=target/mutest_test/debug/deps/auxiliary/libshared_dep.rlib
//@ rustc-flags: --extern shared_dep_user=target/mutest_test/debug/deps/auxiliary/libshared_dep_user.rlib

//! A direct dependency that another dependency loads first, from a search path, is still named by its own name.

#![allow(unused)]

use shared_dep_user::doubled;

#[test]
fn test() {
    assert_eq!(2, doubled());
    assert_eq!(1, shared_dep::value());
}
