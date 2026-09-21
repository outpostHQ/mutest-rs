use std::sync::OnceLock;

use rustc_middle::ty::TyCtxt;

/// A stop asked for from outside the compilation, so that the long single-threaded analyses can give
/// up once their result has stopped being of use to anyone.
static REASON: OnceLock<String> = OnceLock::new();

pub fn request(reason: String) {
    let _ = REASON.set(reason);
}

pub fn requested() -> Option<&'static str> {
    REASON.get().map(String::as_str)
}

/// Call from wherever an analysis would otherwise keep running for minutes; cheap enough to sit in a
/// loop, and reports the reason it stopped as an ordinary compilation error.
pub fn abort_if_requested(tcx: TyCtxt<'_>) {
    if let Some(reason) = requested() {
        tcx.dcx().fatal(reason);
    }
}

#[cfg(test)]
mod tests {
    use super::{request, requested};

    #[test]
    fn a_requested_stop_carries_the_reason_the_analysis_is_being_given_up_on() {
        assert_eq!(requested(), None);

        request("another part of the build has already failed".to_owned());

        assert_eq!(requested(), Some("another part of the build has already failed"));
    }
}
