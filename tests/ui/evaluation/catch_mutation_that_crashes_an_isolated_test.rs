//@ run
//@ stderr: empty
//@ mutation-operators: fn_return_default
//@ run-flags: --isolate=all

//! A mutation that crashes an isolated test is caught, so the analysis completes and exits 0.

fn checked(value: u32) -> u32 {
    value
}

#[test]
fn test() {
    // NOTE: An exit code that reports no test result, rather than `abort`, whose core dump a host may take longer than the test timeout to collect.
    if checked(7) != 7 {
        std::process::exit(9);
    }
}
