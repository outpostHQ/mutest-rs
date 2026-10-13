#![crate_type = "lib"]

extern crate enum_behind_alias;

pub mod error {
    pub type SdkError<E, R = ()> = enum_behind_alias::result::SdkError<E, R>;
}
