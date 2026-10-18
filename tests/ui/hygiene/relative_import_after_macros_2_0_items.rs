//@ build
//@ stderr: empty

#![feature(decl_macro)]
#![allow(unused)]

struct Monitor;

mod statics {
    mod tests {
        macro fixture($name:ident) { static $name: usize = 0; }
        fixture!(SAFE);
    }
}

mod after_statics { use super::Monitor; }

mod imports {
    struct Scheduled;

    mod tests {
        struct Fixture;

        macro fixture() { use super::Scheduled; }
        fixture!();

        mod after_import { use super::Fixture; }
    }
}
