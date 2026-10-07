//! Edition 2021 blocks for the code that mutest-rs prints from pre-2024 macro expansions,
//! see <https://doc.rust-lang.org/edition-guide/rust-2024/temporary-tail-expr-scope.html>.

#![no_std]

/// Expands to the given block expression as an edition 2021 block.
#[macro_export]
macro_rules! block {
    ({ $($stmts:tt)* }) => { { $($stmts)* } };
    (unsafe { $($stmts:tt)* }) => { unsafe { $($stmts)* } };
    ($label:lifetime: { $($stmts:tt)* }) => { $label: { $($stmts)* } };
}
