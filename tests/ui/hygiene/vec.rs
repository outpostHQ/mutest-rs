//@ build
//@ stderr: empty

#![feature(decl_macro)]

macro m() {
    let _: Vec<()> = vec![];
    let _: Vec<usize> = vec![1, 2, 3];

    enum E {
        A,
    }
    let _: Vec<E> = vec![E::A];

    let _: Vec<Vec<&str>> = vec![vec!["a"], vec!["b"], vec!["c"], vec!["d"], vec!["e"]];

    let _: Vec<Box<dyn FnMut(&mut u8)>> = vec![Box::new(|x| *x += 1), Box::new(|x| *x -= 1)];
    let _: Vec<fn(&u8) -> u8> = vec![|x: &u8| *x, |x: &u8| *x + 1];
}

#[test]
fn test() {
    m!();
}
