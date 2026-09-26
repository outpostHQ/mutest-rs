use std::env;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

/// Set by `cargo-mutest` to a path unique to the run, so that a marker left by an earlier run can
/// never stop a later one, and so that a driver invoked outside `cargo mutest` ignores all of this.
const MARKER_PATH_ENV_VAR: &str = "MUTEST_BUILD_FAILURE_MARKER";

const POLL_INTERVAL: Duration = Duration::from_millis(500);

pub fn marker_path() -> Option<PathBuf> {
    env::var_os(MARKER_PATH_ENV_VAR).map(PathBuf::from)
}

/// What `cargo mutest` calls a build that failed: its crate, whether it is a test harness, and its
/// package. One line, as the marker holds one per failed build.
pub fn build_label(args: &[String], package: Option<&str>, test_harness: bool) -> String {
    let crate_name = args.iter().position(|arg| arg == "--crate-name").and_then(|i| args.get(i + 1)).map(String::as_str)
        .or_else(|| args.iter().find_map(|arg| arg.strip_prefix("--crate-name=")));

    let mut label = match (test_harness, crate_name) {
        (true, Some(crate_name)) => format!("the test harness of `{crate_name}`"),
        (true, None) => "a test harness".to_owned(),
        (false, Some(crate_name)) => format!("crate `{crate_name}`"),
        (false, None) => "a crate".to_owned(),
    };
    if let Some(package) = package {
        label.push_str(&format!(" in package `{package}`"));
    }
    label
}

/// Appended, one line per build, as every driver of the run that fails records itself here.
pub fn record_failure(marker_path: &Path, label: &str) {
    // A marker that cannot be written costs the sibling invocations some wasted work and `cargo
    // mutest` the name of the build; the run still ends with Cargo's failure.
    let Ok(mut marker) = fs::OpenOptions::new().create(true).append(true).open(marker_path) else { return; };
    let _ = writeln!(marker, "{label}");
}

pub fn build_failed(marker_path: &Path) -> bool {
    marker_path.exists()
}

/// Cargo lets the rustc invocations it has already spawned run to completion after one of them has
/// failed, leaving this driver to finish an analysis whose result nothing will read.
pub fn stop_once_the_build_has_failed(marker_path: PathBuf) {
    thread::spawn(move || {
        while !build_failed(&marker_path) {
            thread::sleep(POLL_INTERVAL);
        }

        mutest_emit::stop::request("another part of the build has already failed".to_owned());
    });
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::{build_failed, build_label, record_failure};

    #[test]
    fn a_driver_sees_the_failure_that_another_driver_of_the_same_build_recorded() {
        let marker_path = std::env::temp_dir().join(format!("mutest-build-failure-{}", std::process::id()));
        let _ = fs::remove_file(&marker_path);

        assert_eq!(build_failed(&marker_path), false);

        record_failure(&marker_path, "the test harness of `krate` in package `krate`");
        assert_eq!(build_failed(&marker_path), true);

        let _ = fs::remove_file(&marker_path);
    }

    #[test]
    fn a_build_whose_drivers_all_succeeded_leaves_no_failure_behind() {
        let marker_path = std::env::temp_dir().join(format!("mutest-build-failure-unrecorded-{}", std::process::id()));
        let _ = fs::remove_file(&marker_path);

        assert_eq!(build_failed(&marker_path), false);
    }

    #[test]
    fn every_failed_build_of_a_run_is_named_on_its_own_line() {
        let marker_path = std::env::temp_dir().join(format!("mutest-build-failure-named-{}", std::process::id()));
        let _ = fs::remove_file(&marker_path);

        record_failure(&marker_path, "the test harness of `krate` in package `krate`");
        record_failure(&marker_path, "the test harness of `cli` in package `krate`");

        assert_eq!(fs::read_to_string(&marker_path).unwrap(), "the test harness of `krate` in package `krate`\nthe test harness of `cli` in package `krate`\n");
        let _ = fs::remove_file(&marker_path);
    }

    #[test]
    fn a_build_is_named_by_its_crate_whether_it_is_a_test_harness_and_its_package() {
        let args = ["--crate-name", "cli", "--edition=2024", "tests/cli.rs", "--test"].map(str::to_owned);
        assert_eq!(build_label(&args, Some("chock"), true), "the test harness of `cli` in package `chock`");

        let args = ["--crate-name=chock", "src/lib.rs"].map(str::to_owned);
        assert_eq!(build_label(&args, Some("chock"), false), "crate `chock` in package `chock`");

        let args = ["src/lib.rs".to_owned()];
        assert_eq!(build_label(&args, None, true), "a test harness");
    }
}
