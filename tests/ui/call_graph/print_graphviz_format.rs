//@ print-call-graph
//@ stdout
//@ mutest-flags: --graph-format=graphviz

fn by_fn_item(x: u8) -> u8 { x }

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
    let _ = Some(1).map(by_fn_item);

    let square: Box<dyn Shape> = Box::new(Square);
    let _ = area_of(&*square);
}
