//@ build
//@ stderr: empty

#![feature(decl_macro)]

#![allow(unused)]

mod engine {
    // TEST: Imports may name `macro_rules` macros in textual scope, which are not module children.
    macro_rules! all_engines_15407415234486785952 {
        ($test:item) => { $test };
    }
    pub(crate) use all_engines_15407415234486785952 as all_engines;

    all_engines! {
        pub fn encoded_len(len: usize) -> usize { (len + 2) / 3 * 4 }
    }

    macro_rules! template {
        ($name:ident, $name_rand:ident) => {
            macro_rules! $name_rand {
                ($test:item) => { $test };
            }
            pub(crate) use $name_rand as $name;
        };
    }

    // TEST: Such imports may also come from an expansion.
    template!(all_decoders, all_decoders_5126354786);

    all_decoders! {
        pub fn decoded_len(len: usize) -> usize { len / 4 * 3 }
    }

    macro opaque_template($name:ident) {
        macro_rules! name_rand {
            ($test:item) => { $test };
        }
        pub(crate) use name_rand as $name;
    }

    // TEST: The import keeps the sanitized name of a hygienic macro definition.
    opaque_template!(all_configs);

    all_configs! {
        pub fn padding_len(len: usize) -> usize { (3 - len % 3) % 3 }
    }
}

#[test]
fn test() {
    assert_eq!(8, engine::encoded_len(4));
    assert_eq!(6, engine::decoded_len(8));
    assert_eq!(2, engine::padding_len(4));
}
