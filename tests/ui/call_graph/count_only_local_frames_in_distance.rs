//@ print-targets
//@ stdout
//@ aux-build: crate_with_calls.rs
//@ mutest-flags: --call-graph-depth-limit=3

extern crate crate_with_calls;

use std::io::{self, Read};

struct Counted(usize);

fn feed(counted: &mut Counted, len: usize) -> usize {
    counted.0 += len;
    len
}

impl Read for Counted {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let len = buf.len().min(4 - self.0);
        Ok(feed(self, len))
    }
}

// TEST: The frames of `io::copy` in the standard library do not add to the distance of the local `read` impl.
#[test]
fn test_local_callee_through_std_io_copy() {
    let mut counted = Counted(0);
    assert_eq!(4, io::copy(&mut counted, &mut io::sink()).unwrap());
}

struct ImplsExternTrait;

fn extern_trait_fn_impl() {}
impl crate_with_calls::ExternTrait for ImplsExternTrait {
    fn extern_trait_fn() {
        extern_trait_fn_impl();
    }
}

// TEST: Three frames of an extern crate do not add to the distance of the local trait impl.
#[test]
fn test_local_callee_through_nested_extern_calls() {
    crate_with_calls::extern_fn_calling_trait_fn_through_nested_calls::<ImplsExternTrait>();
}
