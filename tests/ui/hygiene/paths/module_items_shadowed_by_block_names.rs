//@ build
//@ stderr: empty

#![allow(unused)]

pub enum Close {
    Connection(u8),
    Application(u8),
}

pub enum Frame {
    Padding,
    Close(Close),
}

impl Frame {
    pub fn ty(&self) -> u8 {
        // TEST: The glob import brings the variant `Frame::Close` into the block, which shadows the enum `Close`.
        use Frame::*;
        match *self {
            Padding => 0,
            Close(self::Close::Connection(_)) => 1,
            Close(self::Close::Application(_)) => 2,
        }
    }
}

mod data {
    pub struct Value(pub u8);
}

fn read() -> u8 {
    // TEST: The block item `data` shadows the module `data`.
    mod data {}
    let self::data::Value(value) = self::data::Value(1);
    value
}
