//@ build
//@ stderr: empty

//! The crates that the runtime links in must not add trait impls that make comparisons of the crate ambiguous.

const LIMIT: i32 = 2;

#[test]
fn test() {
    let count: u64 = 2;
    assert_eq!(count, LIMIT as _);
    assert_eq!(count, "2".parse().unwrap());
}
