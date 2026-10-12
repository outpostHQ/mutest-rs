//@ rustc-flags: --extern versioned_dep=target/mutest_test/debug/deps/auxiliary/libversioned_dep-older.rlib

#![crate_type = "lib"]

pub extern crate versioned_dep;

pub trait Registry {
    fn store(&self) -> Option<Box<dyn versioned_dep::Store>>;
}
