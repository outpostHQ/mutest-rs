#![no_std]

#[derive(Clone, Eq, PartialEq, Hash, Debug)]
pub enum TestName {
    StaticTestName(&'static str),
}

pub use TestName::*;

#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub enum TestType {
    UnitTest,
    IntegrationTest,
    DocTest,
    Unknown,
}

#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub enum ShouldPanic {
    No,
    Yes,
    YesWithMessage(&'static str),
}

#[derive(Clone, Debug)]
pub struct TestDesc {
    pub name: TestName,
    pub ignore: bool,
    pub ignore_message: Option<&'static str>,
    pub source_file: &'static str,
    pub start_line: usize,
    pub start_col: usize,
    pub end_line: usize,
    pub end_col: usize,
    pub should_panic: ShouldPanic,
    pub compile_fail: bool,
    pub no_run: bool,
    pub test_type: TestType,
}

#[derive(Debug)]
pub enum TestFn {
    StaticTestFn(fn() -> Result<(), ()>),
}

pub use TestFn::*;

#[derive(Debug)]
pub struct TestDescAndFn {
    pub desc: TestDesc,
    pub testfn: TestFn,
}

/// Invoked when unit tests terminate.
/// Returns `Result::Err` if the test is considered a failure.
pub fn assert_test_result(_result: ()) -> Result<(), ()> {
    Ok(())
}

/// Stands in for `std::process::ExitCode`, which libtest returns from its entry points
/// but which is out of reach in a `no_std` environment.
pub struct ExitCode;

pub fn test_main_env_args(_tests: &[&TestDescAndFn]) -> ExitCode {
    panic!("test_metadata_shim::test_main_env_args: dummy definition for rustc test code generation");
}
