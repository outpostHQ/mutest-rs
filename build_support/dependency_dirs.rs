use std::fs;
use std::path::{Path, PathBuf};

/// `deps_dir`, then the `build/<crate>/<hash>/out` directories beside it, where Cargo's newer build
/// directory layout puts crate outputs.
pub fn dependency_dirs(deps_dir: &Path) -> Vec<PathBuf> {
    let mut dirs = vec![deps_dir.to_owned()];
    let Some(Ok(crates)) = deps_dir.parent().map(|profile_dir| fs::read_dir(profile_dir.join("build"))) else { return dirs; };
    for krate in crates.flatten() {
        let Ok(hashes) = fs::read_dir(krate.path()) else { continue; };
        dirs.extend(hashes.flatten().map(|hash| hash.path().join("out")).filter(|out| out.is_dir()));
    }
    dirs
}
