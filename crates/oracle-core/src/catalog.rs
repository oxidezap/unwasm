//! Discovery of captured modules on disk.
//!
//! The captured artifacts are not vendored here. `scripts/fetch-wasm.py` places
//! them in `wasm/` at the workspace root, verified against the hashes in
//! `wasm.lock.json`; a copy committed to this repository would drift from the
//! capture that the protocol notes refer to.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// Environment variable pointing at the directory holding captured `.wasm`
/// files. Overrides the lookup below.
pub const DIR_ENV: &str = "WA_WASM_DIR";

/// The lock naming the modules and their hashes. Its presence is what marks an
/// ancestor as this workspace's root, so a directory called `wasm` somewhere
/// above the checkout is not mistaken for the capture directory.
const LOCK_FILE: &str = "wasm.lock.json";

/// Where `fetch-wasm.py` puts the captures, relative to the workspace root.
const FETCH_DIR: &str = "wasm";

/// Where the captures lived first. Kept as a fallback so a working tree that
/// already has the whatsapp-rust checkout does not have to download them again.
const SIBLING_DIR: &str = "whatsapp-rust/docs/captured-js/wasm";

/// How far to walk up looking for a capture directory. Deep enough to cover a
/// test run, whose working directory is the crate rather than the workspace
/// root.
const MAX_ANCESTOR_WALK: usize = 5;

/// Walks up from the crate directory and the working directory looking for the
/// capture path. Two anchors are needed because `cargo run` and `cargo test`
/// have different working directories, and only the crate directory is stable
/// across both.
fn find_capture_dir() -> Option<PathBuf> {
    let anchors = [
        PathBuf::from(env!("CARGO_MANIFEST_DIR")),
        std::env::current_dir().ok()?,
    ];

    anchors
        .iter()
        .flat_map(|anchor| anchor.ancestors().take(MAX_ANCESTOR_WALK))
        .flat_map(|ancestor| {
            let fetched = ancestor
                .join(LOCK_FILE)
                .is_file()
                .then(|| ancestor.join(FETCH_DIR));
            [fetched, Some(ancestor.join(SIBLING_DIR))]
        })
        .flatten()
        .find(|candidate| candidate.is_dir())
}

/// A captured module found on disk.
#[derive(Debug, Clone)]
pub struct CapturedModule {
    /// The file stem, which for WhatsApp Web assets is its content hash id.
    pub id: String,
    /// Where it sits on disk.
    pub path: PathBuf,
    /// Its size in bytes, which is how the catalogue orders itself.
    pub size: u64,
}

/// The set of captured modules available to this run.
#[derive(Debug, Clone)]
pub struct Catalog {
    dir: PathBuf,
    modules: Vec<CapturedModule>,
}

impl Catalog {
    /// Resolves the capture directory from `WA_WASM_DIR`, then by walking up
    /// from this crate and the working directory looking for a sibling
    /// whatsapp-rust checkout, and lists every `.wasm` in it.
    pub fn discover() -> Result<Self> {
        let dir = match std::env::var_os(DIR_ENV) {
            Some(dir) => PathBuf::from(dir),
            None => find_capture_dir().with_context(|| {
                format!(
                    "no capture directory found; set {DIR_ENV} to the directory holding the \
                     captured .wasm files"
                )
            })?,
        };
        Self::from_dir(dir)
    }

    /// Lists every `.wasm` in one directory, smallest first.
    ///
    /// # Errors
    ///
    /// Returns an error if the directory cannot be read.
    pub fn from_dir(dir: impl Into<PathBuf>) -> Result<Self> {
        let dir = dir.into();
        let entries = std::fs::read_dir(&dir)
            .with_context(|| format!("reading capture directory {}", dir.display()))?;

        let mut modules = Vec::new();
        for entry in entries {
            let entry = entry?;
            let path = entry.path();
            if path.extension().is_none_or(|ext| ext != "wasm") {
                continue;
            }
            let id = path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .unwrap_or_default()
                .to_owned();
            modules.push(CapturedModule {
                id,
                size: entry.metadata()?.len(),
                path,
            });
        }
        modules.sort_by_key(|module| module.size);

        Ok(Self { dir, modules })
    }

    /// The directory these modules were found in.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Every module found, smallest first.
    pub fn modules(&self) -> &[CapturedModule] {
        &self.modules
    }

    /// Resolves a user-supplied target: either a path to a `.wasm` file or the
    /// id of a catalogued module.
    pub fn resolve(&self, target: &str) -> Result<CapturedModule> {
        let as_path = Path::new(target);
        if as_path.is_file() {
            return Ok(CapturedModule {
                id: as_path
                    .file_stem()
                    .and_then(|stem| stem.to_str())
                    .unwrap_or(target)
                    .to_owned(),
                size: as_path.metadata()?.len(),
                path: as_path.to_path_buf(),
            });
        }

        self.modules
            .iter()
            .find(|module| module.id == target)
            .cloned()
            .with_context(|| {
                format!(
                    "no module `{target}` in {} and no such file",
                    self.dir.display()
                )
            })
    }
}
