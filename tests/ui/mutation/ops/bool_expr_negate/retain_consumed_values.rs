//@ print-mutations
//@ build
//@ stdout
//@ stderr: empty
//@ mutation-operators: bool_expr_negate

#[mutest::skip]
fn consume(value: bool) { assert!(value); }

#[mutest::skip]
fn borrow(value: &bool) { assert!(*value); }

#[mutest::skip]
fn touch(trace: &mut Vec<u8>, tag: u8, value: bool) -> bool {
    trace.push(tag);
    value
}

fn used(value: bool, trace: &mut Vec<u8>) -> bool {
    consume(value);
    borrow(&value);
    let _ = value;
    let _: bool = Default::default();
    if value { trace.push(1); }
    match value { true => trace.push(2), false => () }
    value
}

fn returned(value: bool) -> bool { return value; }

#[allow(unused_braces, reason = "cover block wrappers and a statement without a semicolon")]
fn wrappers(value: bool) -> bool {
    { value };
    { consume(value) }
    { consume(value); value }
}

fn question(outcome: Result<bool, u8>, value: bool) -> Result<bool, u8> {
    Ok::<bool, u8>(value)?;
    let _ = outcome?;
    if outcome? { return Ok(value); }
    Err(9)
}

#[allow(unused_must_use, reason = "only the complete short-circuit result is discarded")]
fn short_circuit(trace: &mut Vec<u8>, first: bool, second: bool) {
    (touch(trace, 1, first) && touch(trace, 2, second)) || touch(trace, 3, first);
}

#[test]
fn consumed_results_remain_observable() {
    let mut trace = Vec::new();
    assert!(used(true, &mut trace));
    assert_eq!(trace, [1, 2]);
    assert!(returned(true));
    assert!(wrappers(true));
    assert_eq!(question(Ok(true), true), Ok(true));
    assert_eq!(question(Ok(false), false), Err(9));
    assert_eq!(question(Err(7), true), Err(7));
    for (first, second, expected) in [
        (false, false, vec![1, 3]),
        (false, true, vec![1, 3]),
        (true, false, vec![1, 2, 3]),
        (true, true, vec![1, 2]),
    ] {
        trace.clear();
        short_circuit(&mut trace, first, second);
        assert_eq!(trace, expected);
    }
}
