use std::env;
use std::fs;
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

pub fn record_failure(marker_path: &Path) {
    // A marker that cannot be written only costs the sibling invocations some wasted work.
    let _ = fs::write(marker_path, []);
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

    use super::{build_failed, record_failure};

    #[test]
    fn a_driver_sees_the_failure_that_another_driver_of_the_same_build_recorded() {
        let marker_path = std::env::temp_dir().join(format!("mutest-build-failure-{}", std::process::id()));
        let _ = fs::remove_file(&marker_path);

        assert_eq!(build_failed(&marker_path), false);

        record_failure(&marker_path);
        assert_eq!(build_failed(&marker_path), true);

        let _ = fs::remove_file(&marker_path);
    }

    #[test]
    fn a_build_whose_drivers_all_succeeded_leaves_no_failure_behind() {
        let marker_path = std::env::temp_dir().join(format!("mutest-build-failure-unrecorded-{}", std::process::id()));
        let _ = fs::remove_file(&marker_path);

        assert_eq!(build_failed(&marker_path), false);
    }
}
