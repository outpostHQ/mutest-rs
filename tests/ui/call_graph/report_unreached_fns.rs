//@ print-targets
//@ print-unreached
//@ mutest-flags: --depth 2
//@ stdout
//@ stderr: empty

#[allow(dead_code)]
fn never_called() {}

fn beyond_depth() {}

fn at_depth_2() {
    beyond_depth();
}

fn at_depth_1() {
    at_depth_2();
}

#[test]
fn test() {
    at_depth_1();
}
