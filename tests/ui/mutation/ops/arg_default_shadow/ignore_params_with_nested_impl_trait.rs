//@ print-mutations
//@ build
//@ stdout
//@ stderr: empty
//@ mutation-operators: arg_default_shadow

use std::io::Read;

fn f(n: usize, pipe: Option<impl Read + Send + 'static>, pipes: Vec<Option<impl Read>>) -> usize {
    n + usize::from(pipe.is_some()) + pipes.len()
}

#[test]
fn test() {
    assert_eq!(f(1, Some(std::io::empty()), vec![Some(std::io::empty())]), 3);
}
