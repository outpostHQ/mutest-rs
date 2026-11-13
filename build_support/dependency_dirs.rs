use std::fs;
use std::path::{Path, PathBuf};

/// `deps_dir`, the profile directory beside it where Cargo uplifts a primary package's unhashed
/// rlib, and the `build/<crate>/<hash>/out` directories Cargo's newer build layout puts under it.
pub fn dependency_dirs(deps_dir: &Path) -> Vec<PathBuf> {
    let Some(profile_dir) = deps_dir.parent() else { return vec![deps_dir.to_owned()]; };
    let mut dirs = vec![deps_dir.to_owned(), profile_dir.to_owned()];
    dirs.extend(fs::read_dir(profile_dir.join("build")).into_iter().flatten().flatten().filter_map(|krate| fs::read_dir(krate.path()).ok()).flatten().flatten()
        .map(|hash| hash.path().join("out")).filter(|out| out.is_dir()));
    dirs
}

/// The most recently modified `.rlib` named `flat_name` or `flat_name` with a `-<hash>` suffix before
/// the extension, across `dirs`.
pub fn find_rlib(dirs: &[PathBuf], flat_name: &str) -> Option<PathBuf> {
    let hashed_prefix = format!("{}-", flat_name.strip_suffix(".rlib")?);
    dirs.iter().filter_map(|dir| fs::read_dir(dir).ok()).flatten().flatten()
        .map(|entry| entry.path())
        .filter(|path| path.file_name().and_then(|s| s.to_str()).is_some_and(|name| name == flat_name || (name.starts_with(&hashed_prefix) && name.ends_with(".rlib"))))
        .max_by_key(|path| fs::metadata(path).and_then(|metadata| metadata.modified()).ok())
}
