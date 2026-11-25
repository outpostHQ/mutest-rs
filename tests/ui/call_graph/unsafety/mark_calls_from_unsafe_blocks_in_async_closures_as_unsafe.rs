//@ print-targets
//@ stdout
//@ stderr: empty

#![allow(unused_unsafe)]

fn called_from_unsafe_block_in_async_closure() {}

fn async_closure(owned: String) -> impl AsyncFn() {
    async move || unsafe { let _ = called_from_unsafe_block_in_async_closure(); let _ = owned.len(); }
}

fn call_once_and_poll(f: impl AsyncFnOnce()) {
    let _ = std::future::Future::poll(Box::pin(f()).as_mut(), &mut std::task::Context::from_waker(std::task::Waker::noop()));
}

// TEST: An async closure of kind `Fn`, called once, runs a by-move copy of the body of its coroutine, which has no HIR.
#[test]
fn test() {
    call_once_and_poll(async_closure(String::new()));
}
