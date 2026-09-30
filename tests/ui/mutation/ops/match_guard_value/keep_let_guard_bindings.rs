//@ print-mutations
//@ build
//@ stdout
//@ stderr: empty
//@ mutation-operators: match_guard_value
//@ edition: 2024

#![allow(irrefutable_let_patterns)]

fn guarded(input: Option<Option<u8>>) -> u8 {
    match input {
        Some(inner) if let Some(value) = inner => value,
        _ => 0,
    }
}

fn ordinary(input: Option<u8>) -> u8 {
    match input {
        Some(value) if value > 3 => value,
        _ => 0,
    }
}

fn nonbinding(input: u8) -> u8 {
    match input {
        value if let _ = value && value > 3 => value,
        _ => 0,
    }
}

#[test]
fn guard_bindings_remain_available_to_the_arm() {
    assert_eq!(guarded(Some(Some(7))), 7);
    assert_eq!(guarded(Some(None)), 0);
    assert_eq!(guarded(None), 0);
    assert_eq!(ordinary(Some(7)), 7);
    assert_eq!(ordinary(Some(2)), 0);
    assert_eq!(nonbinding(7), 7);
    assert_eq!(nonbinding(2), 0);
}
