# mutest-rs &mdash; Mutation testing tools for Rust

[![Docs](https://img.shields.io/badge/mutest.rs-black?logo=mdbook)](https://mutest.rs)
[![DOI 10.1109/ICST57152.2023.00014](https://img.shields.io/badge/10.1109%2FICST57152.2023.00014-black?logo=DOI)](https://doi.org/10.1109/ICST57152.2023.00014)

Generate and analyze runtime-swappable code mutants of Rust programs using a dynamic set of abstract mutation operators. For more information, see [the mutest-rs book](https://mutest.rs).

![Output of mutest-rs](docs/res/output.png)

> [!NOTE]
> mutest-rs is primarily a research tool, but is an extremely capable mutation testing tool for all use cases. It was originally developed for my ongoing PhD work.

## Mutation Operators

Currently, the following list of mutation operators is implemented:

| Mutation Operator           | Short Description                                                      |
| --------------------------- | ---------------------------------------------------------------------- |
| `arg_default_shadow`        | Ignore argument by shadowing it with `Default::default()`.             |
| `bit_op_or_and_swap`        | Swap bitwise OR for bitwise AND and vice versa.                        |
| `bit_op_or_xor_swap`        | Swap bitwise OR for bitwise XOR and vice versa.                        |
| `bit_op_shift_dir_swap`     | Swap the direction of bitwise shift operator.                          |
| `bit_op_xor_and_swap`       | Swap bitwise XOR for bitwise AND and vice versa.                       |
| `bool_expr_negate`          | Negate boolean expression.                                             |
| `call_delete`               | Delete call and replace it with `Default::default()`.                  |
| `call_value_default_shadow` | Ignore return value of call by shadowing it with `Default::default()`. |
| `continue_break_swap`       | Swap continue for break and vice versa.                                |
| `eq_op_invert`              | Invert equality check.                                                 |
| `fn_return_default`         | Return a fixed value without evaluating the function body.             |
| `logical_op_and_or_swap`    | Swap logical *and* for logical *or* and vice versa.                    |
| `match_arm_delete`          | Delete match arm, so its values fall through to a later arm.           |
| `match_guard_value`         | Replace match arm guard with `true` or `false`.                        |
| `math_op_add_mul_swap`      | Swap addition for multiplication and vice versa.                       |
| `math_op_add_sub_swap`      | Swap addition for subtraction and vice versa.                          |
| `math_op_div_rem_swap`      | Swap division for modulus and vice versa.                              |
| `math_op_mul_div_swap`      | Swap multiplication for division and vice versa.                       |
| `range_limit_swap`          | Swap limit (inclusivity) of range expression.                          |
| `relational_op_eq_swap`     | Include or remove the boundary (equality) of relational operator.      |
| `relational_op_invert`      | Invert relational operator.                                              |
| `struct_field_delete`       | Take field from the base of struct expression instead.                 |
| `unary_op_delete`           | Delete `!` or `-` unary operator.                                      |

For more information, and examples, see [docs/operators.md](docs/operators.md).

## Build

> mutest-rs relies on the nightly compiler toolchain. `rustup` is configured to automatically install and use the right nightly version.

Build the `mutest-runtime` crate in release mode.

```sh
cargo build --release -p mutest-runtime
```

Install `mutest-driver` and the Cargo subcommand `cargo-mutest` locally.

```sh
cargo install --force --path mutest-driver
cargo install --force --path cargo-mutest
```

> `mutest-driver` embeds the release build of `mutest-runtime`, so build the runtime first. The installed driver unpacks it into `target/mutest/mutest_deps` of the package it tests.

## Usage

Run the `cargo mutest run` subcommand against a standard Cargo package or workspace directory containing your crate.

```sh
cargo mutest run -p <PACKAGE>
```

> [!TIP]
> It is recommended to specify specific Cargo test targets to start with, using the standard `--lib`, `--bin <BIN>`, `--test <TEST>`, and `--example <EXAMPLE>` targeting options, alongside the `-p <PACKAGE>` option.

See `--help` for more options and subcommands.

### Exit codes

`cargo mutest run` exits with a code for scripts and CI to act on, following [cargo-mutants](https://mutants.rs/exit-codes.html):

| Code | Meaning |
| ---- | ------- |
| 0    | The analysis completed, and the tests caught every mutation. |
| 1    | The command could not run as given: an argument is wrong, or a part of mutest-rs it needs is not installed. |
| 2    | The analysis completed, and the tests missed some mutations. |
| 3    | The analysis completed, and some mutations timed out, also when run again alone. |
| 4    | A test harness did not build, or its tests failed without mutations, so its mutations were not evaluated. |
| 101  | mutest-rs panicked, or a test harness ended abnormally, such as being killed by a signal. |

A run of several test harnesses exits with the most severe of these codes, in the order 101, 1, 4, 3, 2, 0. With `--simulate`, it exits 0 if the tests catch the mutation and 2 if they miss it.

The time limit of a test is its time in the reference run and half more, but at least one second more. A mutation that times out runs again after the analysis, alone and one test at a time. Each test then gets twice its first time limit, and at least ten seconds more. The result of this rerun is the result of the mutation, and the line `timeouts confirmed: R re-run alone; D detected, U undetected, C crashed, T timed out again` counts the reruns.

### What mutest-rs mutates

mutest-rs mutates a function only if a test can call it. The call graph starts from each test, and follows:

* direct calls, and calls through generic functions after monomorphization;
* calls through `dyn Trait` objects, which reach each method of the vtables built for the trait;
* calls through function pointers and closures coerced to function pointers;
* calls through the `Fn`, `FnMut`, and `FnOnce` traits, such as a closure given to `Option::map`;
* calls inside a `const fn` that runs at runtime, and `Drop` implementations run by drop glue.

The distance from a test to a function counts only the frames of the crate under test. Frames of the standard library and of dependencies add nothing, so `--depth` and `--call-graph-depth-limit` limit the frames of your own code.

Use `--print unreached` to list the functions that mutest-rs does not mutate: functions that no test calls, and functions beyond the mutation depth.

```sh
cargo mutest run -p <PACKAGE> --lib --print unreached
```

## Platforms

mutest-rs builds with the nightly toolchain pinned in [`rust-toolchain.toml`](rust-toolchain.toml), and runs on Linux, macOS, and Windows. A mutation that crashes or hangs the test process ends only that process; mutest-rs records the crash or timeout and continues. Each platform makes sure that processes the tests start do not outlive them:

| Platform | Processes started by tests | When mutest-rs is killed | Crash reports |
| -------- | -------------------------- | ------------------------ | ------------- |
| Linux    | A subreaper adopts them, and kills them when the test ends. | `PR_SET_PDEATHSIG` kills the test process. | Core dumps are limited to 1 byte. |
| macOS    | They are in the process group of the test, which is killed when the test ends. | A thread in the test process checks its parent every 50 ms, and kills the process group of the test when the parent exits. | Core dumps are disabled. |
| Windows  | They are in a job object, which is closed when the test ends. | Closing the job object kills them. | The job object sets `JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION`, so a crash shows no error dialog. |

On macOS, a process that leaves the process group on purpose, with `setsid` or `setpgid`, is not killed. The job object on Windows does not allow `CREATE_BREAKAWAY_FROM_JOB`.

### Using `cfg(mutest)`

When running `cargo mutest`, the `mutest` cfg is set. This can be used to detect if code is running under mutest-rs, and enable conditional compilation based on it.

Starting with Rust 1.80, cfgs are checked against a known set of config names and values. If your Cargo package is checked with a regular Cargo command, it will warn you about the "unexpected" `mutest` cfg. To [let rustc know that this custom cfg is expected](https://blog.rust-lang.org/2024/05/06/check-cfg.html#expecting-custom-cfgs), ensure that `cfg(mutest)` is present in the `lints.rust.unexpected_cfgs.check-cfg` array in the package's `Cargo.toml`, like so:

```toml
[lints.rust]
unexpected_cfgs = { level = "warn", check-cfg = ["cfg(mutest)"] }
```

### Annotating code with tool attributes

mutest-rs provides [tool attributes](https://doc.rust-lang.org/reference/attributes.html#tool-attributes) that can be used to optionally annotate your code for use with the tool. Note that these attributes are only available when running `cargo mutest`, so they need to be wrapped in `#[cfg_attr(mutest, <MUTEST_ATTRIBUTE>)]` for regular Cargo commands to run.

#### `#[mutest::skip]` (use `#[cfg_attr(mutest, mutest::skip)]`)

Tells mutest-rs to skip the function when applying mutation operators. Useful for marking helper functions for tests (test cases themselves are automatically skipped).

This attribute can only be applied to function declarations:
```rs
#[cfg_attr(mutest, mutest::skip)]
fn perform_tests() {
```

#### `#[mutest::ignore]` (use `#[cfg_attr(mutest, mutest::ignore)]`)

Tells mutest-rs to ignore the statement or expression, including any subexpressions, or function parameter, when applying mutation operators. Useful if mutest-rs is trying to apply mutations to a critical piece of code that might be causing problems.

This attribute can be applied to
* statements (note that expression statements might have to be wrapped in `{}`, [see this linked Rust issue](https://github.com/rust-lang/rust/issues/59144)):
  ```rs
  #[cfg_attr(mutest, mutest::ignore)]
  let buff_len = mem::size_of::<u16>() * 1024;
  ```
* expressions ([wherever the compiler supports attributes on expressions](https://doc.rust-lang.org/reference/expressions.html#expression-attributes)):
  ```rs
      #[cfg_attr(mutest, mutest::ignore)]
      Some(body)
  }
   ```
* and function parameters:
  ```rs
  fn foo(&self, #[cfg_attr(mutest, mutest::ignore)] experimental: bool) {
  ```

## License

The mutest-rs project is dual-licensed under Apache 2.0 and MIT terms.

See [LICENSE-APACHE](LICENSE-APACHE), [LICENSE-MIT](LICENSE-MIT), and [COPYRIGHT](COPYRIGHT) for details.
