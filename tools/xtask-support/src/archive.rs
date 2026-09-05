//! Capture archives: select regular files by locked basename and verify before writing.
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::{Cursor, Read};
use std::path::Path;

/// One immutable capture, regardless of which archive currently serves it.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Capture {
    /// Filename only; archive paths never control the output directory.
    #[serde(rename = "fileName")]
    pub filename: String,
    /// Expected content hash.
    pub sha256: String,
    /// Known byte size, when the source manifest records it.
    pub size: Option<u64>,
    /// Original CDN URL, if retained.
    pub url: Option<String>,
}
impl Capture {
    /// Whether bytes match both available identity checks.
    pub fn matches(&self, bytes: &[u8]) -> bool {
        self.size.is_none_or(|size| size == bytes.len() as u64)
            && crate::sha256(bytes) == self.sha256
    }
    /// Whether the on-disk file is already the pinned capture.
    pub fn present(&self, directory: &Path) -> bool {
        std::fs::read(directory.join(&self.filename)).is_ok_and(|bytes| self.matches(&bytes))
    }
}

/// Extract only wanted regular files with matching hashes from a tar.xz archive.
pub fn take(
    archive: &[u8],
    missing: &mut BTreeMap<String, Capture>,
    directory: &Path,
) -> Result<()> {
    let decoder = xz2::read::XzDecoder::new(Cursor::new(archive));
    for entry in tar::Archive::new(decoder).entries()? {
        let mut entry = entry?;
        if !entry.header().entry_type().is_file() {
            continue;
        }
        let path = entry.path()?;
        let Some(name) = path.file_name().and_then(|s| s.to_str()).map(str::to_owned) else {
            continue;
        };
        let Some(capture) = missing.get(&name) else {
            continue;
        };
        let mut payload = Vec::new();
        entry.read_to_end(&mut payload)?;
        if !capture.matches(&payload) {
            continue;
        }
        crate::write(&directory.join(&name), &payload)?;
        missing.remove(&name);
    }
    Ok(())
}

/// Fetch captures from origins, then walk newest release archives until all hashes are satisfied.
pub fn fetch(
    captures: Vec<Capture>,
    releases: &[(String, String)],
    directory: &Path,
) -> Result<()> {
    let mut missing = BTreeMap::new();
    for capture in captures {
        ensure!(
            Path::new(&capture.filename)
                .file_name()
                .and_then(|s| s.to_str())
                == Some(capture.filename.as_str()),
            "capture filename must be a basename"
        );
        if !capture.present(directory) {
            missing.insert(capture.filename.clone(), capture);
        }
    }
    for name in missing.keys().cloned().collect::<Vec<_>>() {
        let capture = &missing[&name];
        if let Some(url) = &capture.url
            && let Ok(payload) = crate::fetch::get(url, "application/octet-stream", None)
            && capture.matches(&payload)
        {
            crate::write(&directory.join(&name), &payload)?;
            missing.remove(&name);
        }
    }
    if missing.is_empty() {
        return Ok(());
    }
    let token = crate::fetch::github_token();
    for (repo, release) in releases {
        if missing.is_empty() {
            break;
        }
        let url = format!("https://api.github.com/repos/{repo}/releases/tags/{release}");
        let Ok(bytes) = crate::fetch::get(&url, "application/vnd.github+json", token.as_deref())
        else {
            continue;
        };
        let value: serde_json::Value = serde_json::from_slice(&bytes)?;
        let Some(assets) = value["assets"].as_array() else {
            continue;
        };
        let mut assets = assets
            .iter()
            .filter(|a| {
                a["name"]
                    .as_str()
                    .is_some_and(|n| n.starts_with("wasm-") && n.ends_with(".tar.xz"))
            })
            .collect::<Vec<_>>();
        assets.sort_by_key(|a| std::cmp::Reverse(a["created_at"].as_str().unwrap_or_default()));
        for asset in assets {
            if missing.is_empty() {
                break;
            }
            let Some(url) = asset["url"].as_str() else {
                continue;
            };
            if let Ok(bytes) = crate::fetch::get(url, "application/octet-stream", token.as_deref())
            {
                take(&bytes, &mut missing, directory)?;
            }
        }
    }
    ensure!(
        missing.is_empty(),
        "pinned captures unavailable: {}",
        missing.keys().cloned().collect::<Vec<_>>().join(", ")
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn archive_paths_and_wrong_hashes_never_control_destination() {
        let expected = b"pinned capture";
        let mut encoder = xz2::write::XzEncoder::new(Vec::new(), 6);
        {
            let mut tar = tar::Builder::new(&mut encoder);
            for (name, bytes) in [
                ("old/capture.wasm", b"wrong capture".as_slice()),
                ("nested/capture.wasm", expected.as_slice()),
            ] {
                let mut header = tar::Header::new_gnu();
                header.set_size(bytes.len() as u64);
                header.set_mode(0o644);
                header.set_cksum();
                tar.append_data(&mut header, name, bytes).unwrap();
            }
            tar.finish().unwrap();
        }
        let bytes = encoder.finish().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let pin = Capture {
            filename: "capture.wasm".into(),
            sha256: crate::sha256(expected),
            size: Some(expected.len() as u64),
            url: None,
        };
        let mut missing = BTreeMap::from([("capture.wasm".into(), pin.clone())]);
        take(&bytes, &mut missing, dir.path()).unwrap();
        assert!(missing.is_empty());
        assert!(pin.present(dir.path()));
        assert!(!dir.path().join("nested").exists());
        std::fs::write(dir.path().join("capture.wasm"), b"truncated").unwrap();
        assert!(!pin.present(dir.path()));
    }
}
