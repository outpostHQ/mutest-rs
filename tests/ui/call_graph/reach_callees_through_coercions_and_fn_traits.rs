//@ print-call-graph
//@ print-targets
//@ stdout

fn by_fn_once_closure(x: u8) -> u8 { x }

fn by_fn_item(x: u8) -> u8 { x }

fn by_fn_ptr() {}

fn by_closure_fn_ptr() {}

trait Shape {
    fn area(&self) -> u8;
}

struct Square;

impl Shape for Square {
    fn area(&self) -> u8 { 4 }
}

impl Drop for Square {
    fn drop(&mut self) {}
}

fn area_of(shape: &dyn Shape) -> u8 {
    shape.area()
}

#[test]
fn test() {
    let _ = Some(1).map(|x| by_fn_once_closure(x));
    let _ = Some(1).map(by_fn_item);

    let f: fn() = by_fn_ptr;
    f();
    let g: fn() = || by_closure_fn_ptr();
    g();

    let square: Box<dyn Shape> = Box::new(Square);
    let _ = area_of(&*square);
}
