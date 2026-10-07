//@ build
//@ stderr: empty

pub struct Error;

pub type Result<T> = core::result::Result<T, Error>;

pub enum Shape<T> {
    Point { at: T },
}

pub type Alias<T> = Shape<T>;

fn first_ok<T>(mut f: impl FnMut() -> Result<T>) -> Option<T> {
    f().ok()
}

// NOTE: In each case, only the generic args of the type alias give the type.
#[test]
fn test() {
    let result = first_ok(|| crate::Result::<()>::Err(Error));
    assert!(result.is_none());

    let result = first_ok(|| <crate::Result<()>>::Err(Error));
    assert!(result.is_none());

    let shape = crate::Alias::<u8>::Point { at: 1 };
    match shape {
        Shape::Point { at } => assert_eq!(u64::from(at), 1),
    }

    match crate::Result::<u8>::Ok(1) {
        crate::Result::<u8>::Ok(_) => {}
        crate::Result::<u8>::Err(_) => {}
    }
}
