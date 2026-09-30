use std::sync::OnceLock;

use rustc_middle::ty::TyCtxt;

/// Why the compilation was asked to stop, e.g. because another part of the build has failed.
static REASON: OnceLock<String> = OnceLock::new();

/// Ask long-running analyses to stop, reporting the reason as a compilation error.
pub fn request(reason: String) {
    let _ = REASON.set(reason);
}

pub fn requested() -> Option<&'static str> {
    REASON.get().map(String::as_str)
}

/// Abort the compilation if a stop was requested; cheap enough to call in loops.
pub fn abort_if_requested(tcx: TyCtxt<'_>) {
    if let Some(reason) = requested() {
        tcx.dcx().fatal(reason);
    }
}

#[cfg(test)]
mod tests {
    use super::{request, requested};

    #[test]
    fn a_stop_request_keeps_its_reason() {
        assert_eq!(requested(), None);

        request("another part of the build has already failed".to_owned());

        assert_eq!(requested(), Some("another part of the build has already failed"));
    }
}
