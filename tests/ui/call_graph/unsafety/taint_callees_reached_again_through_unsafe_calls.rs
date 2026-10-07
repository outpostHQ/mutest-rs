//@ print-targets
//@ stdout
//@ stderr: empty

fn visit(depth: usize) {
    if depth == 0 { return; }
    descend(depth - 1);
}

fn descend(depth: usize) {
    let _ = unsafe { visit_unchecked(depth) };
}

unsafe fn visit_unchecked(depth: usize) {
    visit(depth);
}

#[test]
fn test() {
    visit(2);
}
