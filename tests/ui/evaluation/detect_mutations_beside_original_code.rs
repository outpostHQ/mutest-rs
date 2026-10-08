//@ print-mutations
//@ run
//@ stdout
//@ stderr: empty
//@ mutation-operators: math_op_add_sub_swap
//@ run-flags: --exhaustive

//! A function body keeps its original code beside the code with its substitutions, and one check chooses between
//! them. Each body shape here must build, and the mutation in it must still be detected.

use std::future::Future;
use std::pin::pin;
use std::task::{Context, Poll, Waker};

fn plain(a: i32, b: i32) -> i32 {
    a + b
}

fn early_return(values: &[i32]) -> Option<i32> {
    for value in values {
        if value % 2 == 0 { return Some(value + 1); }
    }
    None
}

fn loops(values: &[i32]) -> i32 {
    let mut sum = 0;
    let mut rest = values.iter();
    while let Some(value) = rest.next() {
        sum = sum + value;
    }
    for value in values {
        sum = sum + value;
    }
    loop {
        sum = sum + 1;
        break;
    }
    sum
}

fn const_block(value: i32) -> i32 {
    value + const { let mut turns = 0; while turns < 3 { turns += 1; } turns }
}

struct Pair(i32, i32);

impl Pair {
    fn sum(self) -> i32 {
        self.0 + self.1
    }
}

trait Step {
    fn base(&self) -> i32;

    fn next(&self) -> i32 {
        self.base() + 1
    }
}

impl Step for i32 {
    fn base(&self) -> i32 { *self }
}

// NOTE: Each body of this function makes a closure of its own type.
fn holds_closure(values: &[i32], step: i32) -> Vec<i32> {
    let step = step + 1;
    values.iter().map(|value| value * step).collect()
}

// NOTE: Two bodies of this function would return closures of two types.
fn opaque_return(step: i32) -> impl Fn(i32) -> i32 {
    let step = step + 1;
    move |value| value * step
}

// NOTE: Two bodies of this function would each declare the item.
fn inner_item(value: i32) -> i32 {
    fn inner(value: i32) -> i32 { value + 1 }
    inner(value) + 1
}

async fn coroutine(value: i32) -> i32 {
    value + 1
}

fn coroutine_block(value: i32) -> impl Future<Output = i32> {
    let value = value + 1;
    async move { value * 2 }
}

#[cfg(test)]
fn block_on<F: Future>(future: F) -> F::Output {
    match pin!(future).poll(&mut Context::from_waker(Waker::noop())) {
        Poll::Ready(output) => output,
        Poll::Pending => unreachable!(),
    }
}

#[test]
fn test_plain() {
    assert_eq!(5, plain(2, 3));
}

#[test]
fn test_early_return() {
    assert_eq!(Some(5), early_return(&[1, 4]));
}

#[test]
fn test_loops() {
    assert_eq!(7, loops(&[1, 2]));
}

#[test]
fn test_const_block() {
    assert_eq!(5, const_block(2));
}

#[test]
fn test_method() {
    assert_eq!(5, Pair(2, 3).sum());
}

#[test]
fn test_trait_method() {
    assert_eq!(3, 2.next());
}

#[test]
fn test_holds_closure() {
    assert_eq!(vec![3, 6], holds_closure(&[1, 2], 2));
}

#[test]
fn test_opaque_return() {
    assert_eq!(9, opaque_return(2)(3));
}

#[test]
fn test_inner_item() {
    assert_eq!(4, inner_item(2));
}

#[test]
fn test_coroutine() {
    assert_eq!(3, block_on(coroutine(2)));
}

#[test]
fn test_coroutine_block() {
    assert_eq!(6, block_on(coroutine_block(2)));
}
