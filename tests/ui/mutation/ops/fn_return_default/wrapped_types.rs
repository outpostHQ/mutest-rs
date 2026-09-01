//@ print-mutations
//@ build
//@ stdout
//@ stderr: empty
//@ mutation-operators: fn_return_default

// `Result` implements no `Default`, so unwrapping it is what keeps fallible functions from being
// skipped entirely.
fn a_result(x: u32) -> Result<u32, String> {
    Ok(x + 1)
}

fn a_unit_result(x: u32) -> Result<(), String> {
    let _ = x;
    Ok(())
}

// The inner type's own values are wrapped in turn, rather than only its default.
fn a_result_bool(x: u32) -> Result<bool, String> {
    Ok(x > 2)
}

fn an_option(x: u32) -> Option<u32> {
    Some(x)
}

fn a_vec(x: u32) -> Vec<u32> {
    vec![x]
}

#[test]
fn test() {
    assert_eq!(a_result(1).unwrap(), 2);
    assert!(a_unit_result(1).is_ok());
    assert!(a_result_bool(3).unwrap());
    assert_eq!(an_option(1), Some(1));
    assert_eq!(a_vec(1), vec![1]);
}
