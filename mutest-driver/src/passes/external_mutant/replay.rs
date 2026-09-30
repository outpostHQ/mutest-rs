//! The request a driver sends to the child that compiles a specialized mutant crate, and its result.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use mutest_emit::analysis::call_graph::{
    EntryPointAssoc, TargetReachability, UnsafeSource, Unsafety,
};
use rustc_data_structures::fingerprint::Fingerprint;
use rustc_span::def_id::DefPathHash;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use super::{ExternalTargets, StableTarget};
use crate::config::CargoTargetKind;
use crate::passes::analysis::AnalysisPassResult;

#[derive(Serialize, Deserialize)]
pub struct Target {
    hash: (u64, u64),
    unsafety: u8,
    distance: Option<usize>,
    entries: Vec<((u64, u64), usize, Option<u8>)>,
}

#[derive(Serialize, Deserialize)]
pub struct Targets {
    targets: Vec<Target>,
    paths: Vec<((u64, u64), String)>,
    definitions: Vec<((u64, u64), mutest_json::DefId)>,
}

fn hash(value: DefPathHash) -> (u64, u64) {
    let (a, b) = value.0.split();
    (a.as_u64(), b.as_u64())
}

fn unhash(value: (u64, u64)) -> DefPathHash {
    DefPathHash(Fingerprint::new(value.0, value.1))
}

fn source(value: UnsafeSource) -> u8 {
    match value {
        UnsafeSource::EnclosingUnsafe => 0,
        UnsafeSource::Unsafe => 1,
    }
}

fn unsource(value: u8) -> Result<UnsafeSource, String> {
    match value {
        0 => Ok(UnsafeSource::EnclosingUnsafe),
        1 => Ok(UnsafeSource::Unsafe),
        _ => Err("invalid replay unsafe source".to_owned()),
    }
}

impl Targets {
    pub fn encode(value: ExternalTargets) -> Self {
        Self {
            targets: value
                .stable_targets
                .into_iter()
                .map(|target| Target {
                    hash: hash(target.def_path_hash),
                    unsafety: match target.unsafety {
                        Unsafety::None => 0,
                        Unsafety::Tainted(s) => 1 + source(s),
                        Unsafety::Unsafe(s) => 3 + source(s),
                    },
                    distance: match target.reachability {
                        TargetReachability::DirectEntry => None,
                        TargetReachability::NestedCallee { distance } => Some(distance),
                    },
                    entries: target
                        .reachable_from
                        .into_iter()
                        .map(|(h, entry)| {
                            (hash(h), entry.distance, entry.unsafe_call_path.map(source))
                        })
                        .collect(),
                })
                .collect(),
            paths: value
                .path_strs
                .into_iter()
                .map(|(h, p)| (hash(h), p))
                .collect(),
            definitions: value
                .json_definitions
                .into_iter()
                .map(|(h, d)| (hash(h), d))
                .collect(),
        }
    }

    pub fn decode(self) -> Result<ExternalTargets, String> {
        let targets = self
            .targets
            .into_iter()
            .map(|target| {
                Ok(StableTarget {
                    def_path_hash: unhash(target.hash),
                    unsafety: match target.unsafety {
                        0 => Unsafety::None,
                        1..=2 => Unsafety::Tainted(unsource(target.unsafety - 1)?),
                        3..=4 => Unsafety::Unsafe(unsource(target.unsafety - 3)?),
                        _ => return Err("invalid replay unsafety".to_owned()),
                    },
                    reachability: target
                        .distance
                        .map_or(TargetReachability::DirectEntry, |distance| {
                            TargetReachability::NestedCallee { distance }
                        }),
                    reachable_from: target
                        .entries
                        .into_iter()
                        .map(|(h, distance, s)| {
                            Ok((
                                unhash(h),
                                EntryPointAssoc {
                                    distance,
                                    unsafe_call_path: s.map(unsource).transpose()?,
                                },
                            ))
                        })
                        .collect::<Result<_, String>>()?,
                })
            })
            .collect::<Result<_, String>>()?;
        Ok(ExternalTargets {
            stable_targets: targets,
            path_strs: self
                .paths
                .into_iter()
                .map(|(h, p)| (unhash(h), p))
                .collect(),
            json_definitions: self
                .definitions
                .into_iter()
                .map(|(h, d)| (unhash(h), d))
                .collect(),
        })
    }
}

#[derive(Serialize, Deserialize)]
pub struct Request {
    pub targets: Targets,
    pub suffix: String,
    pub cargo_target_kind: Option<CargoTargetKind>,
    pub metadata_directory: PathBuf,
    pub result: PathBuf,
}

#[derive(Serialize, Deserialize)]
pub struct ResultRecord {
    pub analysis: AnalysisPassResult,
    pub compilation_duration: Option<Duration>,
    pub metadata: Option<PathBuf>,
    /// The artifacts of every crate the specialized crate was compiled against.
    pub dependencies: Vec<PathBuf>,
}

pub fn write<T: Serialize>(path: &Path, value: &T) -> Result<(), String> {
    let bytes = serde_json::to_vec(value).map_err(|error| error.to_string())?;
    fs::write(path, bytes).map_err(|error| format!("cannot write `{}`: {error}", path.display()))
}

pub fn read<T: DeserializeOwned>(path: &Path) -> Result<T, String> {
    let bytes = fs::read(path).map_err(|error| format!("cannot read `{}`: {error}", path.display()))?;
    serde_json::from_slice(&bytes).map_err(|error| format!("invalid `{}`: {error}", path.display()))
}
