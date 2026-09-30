//@ build
//@ stderr: empty
//@ mutation-operators: fn_return_default

#![feature(allocator_ext)]

use std::alloc::System;

fn allocated() -> Vec<u8, System> {
    Vec::new_in(System)
}

// A shared allocator reference has no Default implementation.
fn borrowed_allocator() -> Vec<u8, &'static System> {
    static ALLOCATOR: System = System;
    Vec::new_in(&ALLOCATOR)
}

fn ordinary() -> Vec<u8> {
    vec![7]
}

#[test]
fn returned_vectors_keep_their_allocator_type() {
    assert!(allocated().is_empty());
    assert!(borrowed_allocator().is_empty());
    assert_eq!(ordinary(), [7]);
}
