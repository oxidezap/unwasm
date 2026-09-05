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

fn parse_captures(data: &str) -> Result<Vec<WasmCapture>> {
    data.lines()
        .filter(|line| !line.trim().is_empty())
        .enumerate()
        .map(|(index, line)| {
            let mut fields = line.split_whitespace();
            let sha256 = fields
                .next()
                .ok_or_else(|| anyhow::anyhow!("line {}: capture hash missing", index + 1))?;
            let size = fields
                .next()
                .ok_or_else(|| anyhow::anyhow!("line {}: capture size missing", index + 1))?
                .parse::<u64>()?;
            let file_name = fields
                .next()
                .ok_or_else(|| anyhow::anyhow!("line {}: capture filename missing", index + 1))?;
            anyhow::ensure!(
                fields.next().is_none(),
                "line {}: unexpected capture fields",
                index + 1
            );
            Ok(WasmCapture {
                sha256: sha256.into(),
                file_name: file_name.trim_start_matches('*').into(),
                size: Some(size),
                url: None,
            })
        })
        .collect()
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
            let captures = parse_captures(&data)?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_manifest_requires_hash_size_and_name() {
        let captures = parse_captures(&format!("{} 4 tiny.wasm\n", "a".repeat(64))).unwrap();
        assert_eq!(captures[0].size, Some(4));
        assert!(parse_captures("abc tiny.wasm\n").is_err());
        assert!(parse_captures("abc 4 tiny.wasm extra\n").is_err());
    }
}
