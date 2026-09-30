//@ build
//@ stderr: empty

//! Underscore imports share no binding, so both traits must remain in scope.

use std::io::{Read as _, Write as _};

pub fn copy_some(mut from: &[u8], to: &mut Vec<u8>) -> usize {
    let mut buf = [0; 8];
    let read = from.read(&mut buf).unwrap();
    to.write_all(&buf[..read]).unwrap();
    read
}

#[test]
fn test() {
    let mut to = vec![];
    assert_eq!(3, copy_some(b"abc", &mut to));
}
