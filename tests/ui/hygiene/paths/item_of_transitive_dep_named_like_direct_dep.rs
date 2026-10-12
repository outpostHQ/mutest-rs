//@ build
//@ stderr: empty
//@ aux-build: older/versioned_dep.rs
//@ aux-build: versioned_dep.rs
//@ aux-build: versioned_dep_proxy.rs
//@ rustc-flags: --extern versioned_dep=target/mutest_test/debug/deps/auxiliary/libversioned_dep.rlib
//@ rustc-flags: --extern versioned_dep_proxy=target/mutest_test/debug/deps/auxiliary/libversioned_dep_proxy.rlib

#![allow(unused)]

use versioned_dep_proxy::Registry;
use versioned_dep_proxy::versioned_dep::Store;

struct NoStores;

impl Registry for NoStores {
    fn store(&self) -> Option<Box<dyn Store>> {
        None
    }
}

#[test]
fn test() {
    assert!(NoStores.store().is_none());
    let _: Option<Box<dyn versioned_dep::Store>> = None;
}
