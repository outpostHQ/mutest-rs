//@ run
//@ stdout
//@ stderr
//@ eval-stream
//@ mutation-operators: fn_return_default

//! A mutation that ends the harness process runs again in a process of its own, where it counts as crashed.

fn checked(value: u32) -> u32 {
    value
}

fn double(value: u32) -> u32 {
    value * 2
}

#[test]
fn test() {
    // NOTE: An exit code that reports no test result, rather than a stack overflow, whose core dump a host may take longer than the test timeout to collect.
    if checked(7) != 7 {
        std::process::exit(9);
    }
    assert_eq!(4, double(2));
}
