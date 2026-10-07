//@ build
//@ stderr: empty

pub struct Block<F: ?Sized>(Box<F>);

impl<F: ?Sized> Block<F> {
    pub fn new(f: Box<F>) -> Self {
        Block(f)
    }
}

// TEST: Written as `'_`, the lifetimes of the `Fn(..)` arguments and return types would bind new lifetimes.
fn args<'a, 'b>(f: impl Fn(&'a i32, &'b i32) + 'static) -> Block<dyn Fn(&'a i32, &'b i32)> {
    Block::new(Box::new(f))
}

fn ret<'a>(f: impl Fn() -> &'a i32 + 'static) -> Block<dyn Fn() -> &'a i32> {
    Block::new(Box::new(f))
}

fn fn_ptr<'a>(f: fn(&'a i32)) -> Block<fn(&'a i32)> {
    Block::new(Box::new(f))
}

#[test]
fn test() {
    args(|_, _| {});
    ret(|| &1);
    fn_ptr(|_| {});
}
