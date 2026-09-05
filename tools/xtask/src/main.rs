//! Repository automation through `cargo xt`.
use anyhow::Result;
use clap::{Parser, Subcommand};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use wa_store::capture::{ReleaseSource, WasmCapture, restore_captures};

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
    /// Fetch the captured decompiler-test corpus.
    FetchCaptures { destination: Option<PathBuf> },
}
fn root() -> Result<PathBuf> {
    Ok(Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()?)
}
fn main() -> Result<()> {
    let root = root()?;
    match Args::parse().task {
        Task::Sha256 { value, hex } => {
            let bytes = if hex {
                hex::decode(&value)?
            } else {
                std::fs::read(&value)?
            };
            println!("{}", hex::encode(Sha256::digest(bytes)));
            Ok(())
        }
        Task::FetchCaptures { destination } => {
            let data = std::fs::read_to_string(root.join("fixtures/captures.sha256"))?;
            let captures = data
                .lines()
                .filter(|l| !l.trim().is_empty())
                .map(|l| {
                    let mut p = l.split_whitespace();
                    Ok(WasmCapture {
                        sha256: p
                            .next()
                            .ok_or_else(|| anyhow::anyhow!("capture hash missing"))?
                            .into(),
                        file_name: p
                            .next()
                            .ok_or_else(|| anyhow::anyhow!("capture filename missing"))?
                            .trim_start_matches('*')
                            .into(),
                        size: None,
                        url: None,
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            restore_captures(
                &captures,
                &[ReleaseSource {
                    repo: "oxidezap/whatspec".into(),
                    release: "bundle-store".into(),
                }],
                &destination.unwrap_or(root.join("fixtures/wasm")),
            )
        }
    }
}
