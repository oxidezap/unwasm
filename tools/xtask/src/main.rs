//! Repository automation through `cargo xt`.
mod mlow;
mod patches;
use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::{Path, PathBuf};
use xtask_support::{archive, read_json};

#[derive(Parser)]
#[command(
    name = "cargo xt",
    bin_name = "cargo xt",
    about = "Rust maintenance tasks for unwasm"
)]
struct Args {
    #[command(subcommand)]
    task: Task,
}
#[derive(Subcommand)]
enum Task {
    /// SHA-256 of a file or explicit hexadecimal bytes.
    Sha256 {
        value: String,
        #[arg(long)]
        hex: bool,
    },
    /// Fetch hash-pinned oracle captures, trying original URLs then release archives.
    FetchWasm { destination: Option<PathBuf> },
    /// Fetch the captured decompiler-test corpus.
    FetchCaptures { destination: Option<PathBuf> },
    /// Add inspection-only exports for globals, preserving other sections.
    ExportGlobals {
        source: PathBuf,
        destination: PathBuf,
        count: u32,
    },
    /// Diagnostic only: suppress the pinned D5 thread profiler guard.
    NeutralizeThreadProfiler {
        source: PathBuf,
        destination: PathBuf,
    },
    /// Diagnostic only: force the uniquely identified outgoing offer guard.
    ForceOfferGuard {
        source: PathBuf,
        destination: PathBuf,
    },
    /// Diagnostic only: distinguish the nine pinned D5 offer error sites.
    TagOfferErrorSites {
        source: PathBuf,
        destination: PathBuf,
    },
    /// Generate, assemble and verify MLOW oracle data.
    Mlow {
        #[command(subcommand)]
        task: mlow::Task,
    },
}
fn root() -> Result<PathBuf> {
    Ok(Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()?)
}
fn fetch(root: &Path, destination: &Path, ids: Option<&[&str]>) -> Result<()> {
    let lock = read_json(&root.join("wasm.lock.json"))?;
    let captures: Vec<archive::Capture> = serde_json::from_value(lock["modules"].clone())?;
    let captures = captures
        .into_iter()
        .filter(|c| ids.is_none_or(|ids| ids.contains(&c.filename.trim_end_matches(".wasm"))))
        .collect();
    let sources = lock["sources"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("capture sources missing"))?
        .iter()
        .map(|s| {
            Ok((
                s["repo"]
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("repository missing"))?
                    .to_owned(),
                s["release"]
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("release missing"))?
                    .to_owned(),
            ))
        })
        .collect::<Result<Vec<_>>>()?;
    archive::fetch(captures, &sources, destination)
}
fn main() -> Result<()> {
    let root = root()?;
    match Args::parse().task {
        Task::Sha256 { value, hex } => {
            println!("{}", xtask_support::hash_input(&value, hex)?);
            Ok(())
        }
        Task::FetchWasm { destination } => {
            fetch(&root, &destination.unwrap_or(root.join("wasm")), None)
        }
        Task::FetchCaptures { destination } => {
            let data = std::fs::read_to_string(root.join("fixtures/captures.sha256"))?;
            let captures = data
                .lines()
                .filter(|l| !l.trim().is_empty())
                .map(|l| {
                    let mut p = l.split_whitespace();
                    Ok(archive::Capture {
                        sha256: p
                            .next()
                            .ok_or_else(|| anyhow::anyhow!("capture hash missing"))?
                            .into(),
                        filename: p
                            .next()
                            .ok_or_else(|| anyhow::anyhow!("capture filename missing"))?
                            .trim_start_matches('*')
                            .into(),
                        size: None,
                        url: None,
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            archive::fetch(
                captures,
                &[("oxidezap/whatspec".into(), "bundle-store".into())],
                &destination.unwrap_or(root.join("fixtures/wasm")),
            )
        }
        Task::ExportGlobals {
            source,
            destination,
            count,
        } => patches::globals(&source, &destination, count),
        Task::NeutralizeThreadProfiler {
            source,
            destination,
        } => patches::profiler(&source, &destination),
        Task::ForceOfferGuard {
            source,
            destination,
        } => patches::offer_guard(&source, &destination),
        Task::TagOfferErrorSites {
            source,
            destination,
        } => patches::offer_errors(&source, &destination),
        Task::Mlow { task } => mlow::run(&root, task),
    }
}
