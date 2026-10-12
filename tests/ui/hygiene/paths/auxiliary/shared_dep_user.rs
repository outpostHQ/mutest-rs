#![crate_type = "lib"]

extern crate shared_dep;

pub fn doubled() -> u8 {
    shared_dep::value() * 2
}
