//@ build
//@ stderr: empty
//@ mutation-operators: call_value_default_shadow
// TODO: //@ run (without mutest test harness)

#![feature(decl_macro)]

use std::fmt::{self, Debug};
use std::marker::PhantomData;

enum AssertMutated<T: ?Sized> {
    /// Marker value of an unmutated dummy mutation target.
    Sentinel(PhantomData<T>),
    /// Marker value expected to be present in places where a dummy mutation target was successfully mutated.
    Mutated(PhantomData<T>),
}

// The automatic derives for Debug, PartialEq, and Eq restrict their impls
// based on whether T implements the respective trait, so
// we implement these traits ourselves.
impl<T: ?Sized> Debug for AssertMutated<T> {
    fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Self::Sentinel(_) => fmt.debug_tuple("Sentinel").finish(),
            Self::Mutated(_) => fmt.debug_tuple("Mutated").finish(),
        }
    }
}
impl<T: ?Sized> PartialEq for AssertMutated<T> {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Sentinel(_), Self::Sentinel(_)) => true,
            (Self::Mutated(_), Self::Mutated(_)) => true,
            _ => false,
        }
    }
}
impl<T: ?Sized> Eq for AssertMutated<T> {}

// The mutation operator `call_value_default_shadow` replaces the return value
// of a dummy mutation target function with the type's Default value.
// To detect mutation, we define Default to be the "mutated" marker value,
// replacing the "sentinel" marker of an unmutated dummy mutation target.
impl<T: ?Sized> Default for AssertMutated<T> {
    fn default() -> Self {
        Self::Mutated(PhantomData)
    }
}

macro t($f:ident $(<$($param_ty:tt),*>)? -> $ty:ty $([<$($arg_ty:ty),*>])?) {
    // NOTE: The dummy function argument is required for the
    //       `call_value_default_shadow` mutation operator.
    #[mutest::skip]
    fn $f $(<$($param_ty),*>)? (_: ()) -> AssertMutated<$ty> {
        AssertMutated::Sentinel(PhantomData)
    }
    // Make the function accessible, and thus eligible for mutations.
    let v = $f $(::<$($arg_ty),*>)? (());
    // Assert that the return value was successfully mutated which,
    // given a successful build, implies that the type was successfully printed:
    // `{ let _: $ty = f(); Default::default() }`.
    assert_eq!(AssertMutated::Mutated(PhantomData), v);
}

fn ty_printing_cases() {
    // NOTE: We need to declare a non-#[test] function to place the
    //       mutation-based test cases into.
    //       This function must also not be in a #[cfg(test)] context.
    fn f() {
        #![allow(missing_abi)]
        #![allow(improper_ctypes_definitions)]

        use std::cmp::Ordering;
        use std::collections::{HashMap, HashSet, VecDeque};
        use std::convert::Infallible;
        use std::error::Error;
        use std::fs;
        use std::path::PathBuf;
        use std::thread::Thread;
        use std::time::{Duration, Instant};

        t!(f_bool -> bool);
        t!(f_u32 -> u32);
        t!(f_isize -> isize);
        t!(f_char -> char);
        t!(f_unit -> ());
        t!(f_never -> !);
        t!(f_tuple -> (i16, usize, i8));
        t!(f_nested_tuple -> ((), bool, (u64, i64)));
        t!(f_unit_const_ptr -> *const ());
        t!(f_never_mut_ptr -> *mut !);
        t!(f_str_static_ref -> &'static str);
        t!(f_bool_static_mut_ref -> &'static mut bool);
        t!(f_bool_a_ref<'a> -> &'a bool);
        t!(f_f32_slice -> [f32]);
        t!(f_f64_slice_ref<'a> -> &'a [f64]);
        t!(f_u16_array -> [u16; 5]);
        t!(f_i32_array_const_expr -> [i32; 5 + 7]);

        { struct S<const C: i32>; t!(f_struct_const_i32 -> S<-2>); }
        { struct S<const C: usize>; t!(f_struct_const_expr -> S<{ 8 - 7 - 1 }>); }
        // { struct S<const C: f64>; t!(f_struct_const_f64 -> S<0.21f64>); } // Forbidden const generic parameter: f64.
        { struct S<const C: bool>; t!(f_struct_const_bool -> S<true>); }
        { struct S<const C: char>; t!(f_struct_const_char -> S<'c'>); }
        { struct S<const C: char>; t!(f_struct_const_char_quote -> S<'\''>); }
        { struct S<const C: char>; t!(f_struct_const_char_backslash -> S<'\\'>); }
        { struct S<const C: char>; t!(f_struct_const_char_newline -> S<'\n'>); }
        { struct S<const C: char>; t!(f_struct_const_char_non_ascii -> S<'\u{e9}'>); }
        { struct S<const C: i8>; t!(f_struct_const_i8_min -> S<{ i8::MIN }>); }
        { struct S<const C: i16>; t!(f_struct_const_i16_min -> S<{ i16::MIN }>); }
        { struct S<const C: i64>; t!(f_struct_const_i64_min -> S<{ i64::MIN }>); }
        { struct S<const C: i128>; t!(f_struct_const_i128_min -> S<{ i128::MIN }>); }
        { struct S<const C: isize>; t!(f_struct_const_isize_min -> S<{ isize::MIN }>); }
        { struct S<const C: i128>; t!(f_struct_const_i128_max -> S<{ i128::MAX }>); }
        { struct S<const C: u8>; t!(f_struct_const_u8_max -> S<{ u8::MAX }>); }
        { struct S<const C: u16>; t!(f_struct_const_u16_max -> S<{ u16::MAX }>); }
        { struct S<const C: u32>; t!(f_struct_const_u32_max -> S<{ u32::MAX }>); }
        { struct S<const C: u64>; t!(f_struct_const_u64_max -> S<{ u64::MAX }>); }
        { struct S<const C: u128>; t!(f_struct_const_u128_max -> S<{ u128::MAX }>); }
        { struct S<const C: usize>; t!(f_struct_const_usize_max -> S<{ usize::MAX }>); }

        {
            fn f_const_generic_wrapper<const N: usize>() {
                // HACK: Since we cannot parse const generics with macro matchers, we have to resort to
                //       a manual expansion of the following, would-be macro call:
                //       `t!(f_const_generic<const N: usize> -> [(); N]);`
                #[mutest::skip]
                fn f_const_generic<const N: usize>(_: ()) -> AssertMutated<[(); N]> {
                    AssertMutated::Sentinel(PhantomData)
                }
                let v = f_const_generic::<N>(());
                assert_eq!(AssertMutated::Mutated(PhantomData), v);
            }
            f_const_generic_wrapper::<0>();
        }

        { struct S; t!(f_local_struct -> S); }
        t!(f_option_str_static_ref -> Option<&'static str>);
        t!(f_option_t<T> -> Option<T> [<usize>]);
        t!(f_hashmap -> HashMap<PathBuf, fs::File>);

        t!(f_fn_ptr -> fn());
        t!(f_in_out_fn_ptr -> fn(&mut Vec<(Instant, Duration)>) -> Result<usize, String>);
        t!(f_extern_rust_fn_ptr -> extern fn(usize, usize) -> Result<Ordering, Infallible>);
        t!(f_extern_c_fn_ptr -> extern "C" fn(&(usize, usize)) -> bool);

        t!(f_dyn_iterator -> dyn Iterator<Item = usize>);
        t!(f_dyn_error_ref<'a> -> &'a dyn Error);
        t!(f_dyn_error_box -> Box<dyn Error>);
        t!(f_fn_trait -> dyn Fn());
        t!(f_in_out_fn_trait -> dyn Fn(&mut VecDeque<Thread>) -> Result<HashSet<usize>, String>);

        {
            struct S;
            trait I { type T; }
            impl I for S { type T = Result<(), !>; }
            t!(f_qself -> <S as I>::T);
        }
    }

    // TODO: Enable all mutations and run this test.

    f();
}

// NOTE: Functions nested in a #[test] function are not mutated, so the cases live outside it.
#[test]
fn test() {
    ty_printing_cases();
}
