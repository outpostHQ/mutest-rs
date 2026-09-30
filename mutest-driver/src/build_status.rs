use std::env;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

// Only cargo-mutest supplies this run-scoped marker; standalone drivers do not coordinate cancellation.
const MARKER_PATH_ENV_VAR: &str = "MUTEST_BUILD_FAILURE_MARKER";

const POLL_INTERVAL: Duration = Duration::from_millis(500);

pub fn marker_path() -> Option<PathBuf> {
    env::var_os(MARKER_PATH_ENV_VAR).map(PathBuf::from)
}

/// Whether a compiler invocation is the mutation run's own: a replay, or a compilation writing under the
/// run's target directory, unlike the builds of a test that starts its own Cargo with the run's environment.
pub fn belongs_to_run(args: &[String], replay: bool, run_target_dir: Option<&Path>) -> bool {
    if replay {
        return true;
    }
    let Some(root) = run_target_dir else { return false };
    let out_dir = args
        .iter()
        .position(|arg| arg == "--out-dir")
        .and_then(|i| args.get(i + 1))
        .map(String::as_str)
        .or_else(|| args.iter().find_map(|arg| arg.strip_prefix("--out-dir=")));
    out_dir.is_some_and(|dir| Path::new(dir).starts_with(root))
}

/// A single-line label identifying the failed crate and package, or the directory an unnamed build ran in.
pub fn build_label(args: &[String], package: Option<&str>, test_harness: bool, directory: Option<&Path>) -> String {
    let crate_name = args
        .iter()
        .position(|arg| arg == "--crate-name")
        .and_then(|i| args.get(i + 1))
        .map(String::as_str)
        .or_else(|| {
            args.iter()
                .find_map(|arg| arg.strip_prefix("--crate-name="))
        });

    let mut label = match (test_harness, crate_name) {
        (true, Some(crate_name)) => format!("the test harness of `{crate_name}`"),
        (true, None) => "a test harness".to_owned(),
        (false, Some(crate_name)) => format!("crate `{crate_name}`"),
        (false, None) => "a crate".to_owned(),
    };
    if let Some(package) = package {
        label.push_str(&format!(" in package `{package}`"));
    }
    if crate_name.is_none() && let Some(directory) = directory {
        label.push_str(&format!(" run in `{}`", directory.display()));
    }
    label
}

/// Append one failed build to the run's marker.
pub fn record_failure(marker_path: &Path, label: &str) {
    // Cargo still reports failure if this optional diagnostic cannot be written.
    let Ok(mut marker) = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(marker_path)
    else {
        return;
    };
    let _ = writeln!(marker, "{label}");
}

pub fn build_failed(marker_path: &Path) -> bool {
    marker_path.exists()
}

/// Stop analysis after a sibling compiler fails because Cargo will discard the build.
pub fn stop_once_the_build_has_failed() {
    let Some(marker_path) = marker_path() else { return; };
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
    use std::path::Path;

    use super::{belongs_to_run, build_failed, build_label, record_failure};

    #[test]
    fn a_driver_sees_the_failure_that_another_driver_of_the_same_build_recorded() {
        let marker_path =
            std::env::temp_dir().join(format!("mutest-build-failure-{}", std::process::id()));
        let _ = fs::remove_file(&marker_path);

        assert!(!build_failed(&marker_path));

        record_failure(
            &marker_path,
            "the test harness of `krate` in package `krate`",
        );
        assert!(build_failed(&marker_path));

        let _ = fs::remove_file(&marker_path);
    }

    #[test]
    fn a_build_whose_drivers_all_succeeded_leaves_no_failure_behind() {
        let marker_path = std::env::temp_dir().join(format!(
            "mutest-build-failure-unrecorded-{}",
            std::process::id()
        ));
        let _ = fs::remove_file(&marker_path);

        assert!(!build_failed(&marker_path));
    }

    #[test]
    fn every_failed_build_of_a_run_is_named_on_its_own_line() {
        let marker_path =
            std::env::temp_dir().join(format!("mutest-build-failure-named-{}", std::process::id()));
        let _ = fs::remove_file(&marker_path);

        record_failure(
            &marker_path,
            "the test harness of `krate` in package `krate`",
        );
        record_failure(&marker_path, "the test harness of `cli` in package `krate`");

        assert_eq!(
            fs::read_to_string(&marker_path).unwrap(),
            "the test harness of `krate` in package `krate`\nthe test harness of `cli` in package `krate`\n"
        );
        let _ = fs::remove_file(&marker_path);
    }

    #[test]
    fn a_build_is_named_by_its_crate_whether_it_is_a_test_harness_and_its_package() {
        let args = [
            "--crate-name",
            "cli",
            "--edition=2024",
            "tests/cli.rs",
            "--test",
        ]
        .map(str::to_owned);
        assert_eq!(
            build_label(&args, Some("krate"), true, None),
            "the test harness of `cli` in package `krate`"
        );

        let args = ["--crate-name=krate", "src/lib.rs"].map(str::to_owned);
        assert_eq!(
            build_label(&args, Some("krate"), false, Some(Path::new("/work"))),
            "crate `krate` in package `krate`"
        );

        let args = ["src/lib.rs".to_owned()];
        assert_eq!(build_label(&args, None, true, None), "a test harness");
        assert_eq!(build_label(&args, None, false, Some(Path::new("/work"))), "a crate run in `/work`");
    }

    #[test]
    fn only_a_replay_or_a_build_under_the_run_target_directory_belongs_to_the_run() {
        let root = Path::new("/work/target/mutest");
        let own = ["--crate-name", "krate", "--out-dir", "/work/target/mutest/debug/deps"].map(str::to_owned);
        assert!(belongs_to_run(&own, false, Some(root)));
        let own_joined = ["--out-dir=/work/target/mutest/debug/deps".to_owned()];
        assert!(belongs_to_run(&own_joined, false, Some(root)));
        // A test's own Cargo build of a fixture, and a compiler probe with no output, are not the run's.
        let fixture = ["--crate-name", "fixture", "--out-dir", "/tmp/fixture/target/debug/deps"].map(str::to_owned);
        assert!(!belongs_to_run(&fixture, false, Some(root)));
        let probe = ["-", "--print=file-names", "--crate-name", "___"].map(str::to_owned);
        assert!(!belongs_to_run(&probe, false, Some(root)));
        let sibling = ["--out-dir".to_owned(), "/work/target/mutest-other/debug/deps".to_owned()];
        assert!(!belongs_to_run(&sibling, false, Some(root)));
        assert!(!belongs_to_run(&own, false, None));
        assert!(belongs_to_run(&probe, true, None));
    }
}
