//@ build
//@ stderr: empty

//! A `use` item whose `cfg` attribute holds keeps a synthetic trace attribute of the compiler.

#[cfg(test)]
use std::fmt::Write;

#[test]
fn test() {
    let mut text = String::new();
    write!(text, "{}", 1).unwrap();
    assert_eq!("1", text);
}
