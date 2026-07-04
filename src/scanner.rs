// Copyright (c) 2026 PHINs Group
// SPDX-License-Identifier: MIT OR Apache-2.0

use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};

use anyhow::{Context, Result};
use ignore::{DirEntry, WalkBuilder};

#[derive(Debug, Clone)]
pub struct ScannedFile {
    pub abs_path: PathBuf,
    pub rel_path: String,
    pub extension: Option<String>,
    pub language: Option<String>,
    pub hash: String,
    pub size: i64,
    pub modified_at: i64,
}

#[derive(Debug, Clone)]
pub struct ScannedEntry {
    pub abs_path: PathBuf,
    pub rel_path: String,
    pub extension: Option<String>,
    pub language: Option<String>,
    pub size: i64,
    pub modified_at: i64,
}

pub fn scan_repo(repo_path: &Path) -> Result<Vec<ScannedFile>> {
    scan_repo_entries(repo_path)?
        .into_iter()
        .map(hash_entry)
        .collect()
}

pub fn scan_repo_entries(repo_path: &Path) -> Result<Vec<ScannedEntry>> {
    let root = repo_path
        .canonicalize()
        .with_context(|| format!("failed to canonicalize {}", repo_path.display()))?;
    let mut files = Vec::new();
    let walker = WalkBuilder::new(&root)
        .hidden(false)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .filter_entry(should_keep_entry)
        .build();

    for entry in walker {
        let entry = entry?;
        if !entry.file_type().map(|ty| ty.is_file()).unwrap_or(false) {
            continue;
        }
        let abs_path = entry.path().to_path_buf();
        if is_binary_file(&abs_path)? {
            continue;
        }
        let rel_path = abs_path
            .strip_prefix(&root)
            .unwrap_or(&abs_path)
            .to_string_lossy()
            .replace('\\', "/");
        files.push(scan_entry(abs_path, rel_path)?);
    }

    files.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));
    Ok(files)
}

pub fn scan_path(repo_path: &Path, rel_path: &str) -> Result<Option<ScannedFile>> {
    let root = repo_path
        .canonicalize()
        .with_context(|| format!("failed to canonicalize {}", repo_path.display()))?;
    if should_skip_rel_path(rel_path) {
        return Ok(None);
    }
    let abs_path = root.join(rel_path);
    if !abs_path.is_file() || is_binary_file(&abs_path)? {
        return Ok(None);
    }
    let entry = scan_entry(abs_path, rel_path.replace('\\', "/"))?;
    Ok(Some(hash_entry(entry)?))
}

pub fn hash_entry(entry: ScannedEntry) -> Result<ScannedFile> {
    let hash = blake3::hash(&fs::read(&entry.abs_path)?)
        .to_hex()
        .to_string();
    Ok(ScannedFile {
        abs_path: entry.abs_path,
        rel_path: entry.rel_path,
        extension: entry.extension,
        language: entry.language,
        hash,
        size: entry.size,
        modified_at: entry.modified_at,
    })
}

pub fn language_for_extension(extension: &str) -> Option<&'static str> {
    match extension {
        "js" | "jsx" | "mjs" | "cjs" => Some("javascript"),
        "ts" | "tsx" => Some("typescript"),
        "rs" => Some("rust"),
        "md" | "mdx" => Some("markdown"),
        "json" => Some("json"),
        "toml" => Some("toml"),
        "yaml" | "yml" => Some("yaml"),
        _ => None,
    }
}

fn scan_entry(abs_path: PathBuf, rel_path: String) -> Result<ScannedEntry> {
    let metadata = fs::metadata(&abs_path)?;
    let extension = abs_path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.to_ascii_lowercase());
    let language = extension.as_deref().and_then(language_for_extension);
    let modified_at = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or_default();
    Ok(ScannedEntry {
        abs_path,
        rel_path,
        extension,
        language: language.map(str::to_string),
        size: metadata.len() as i64,
        modified_at,
    })
}

fn should_keep_entry(entry: &DirEntry) -> bool {
    if !entry.file_type().map(|ty| ty.is_dir()).unwrap_or(false) {
        return true;
    }
    let name = entry.file_name().to_string_lossy();
    !matches!(
        name.as_ref(),
        ".git" | ".ckg" | "node_modules" | "dist" | "build" | "target"
    )
}

pub fn should_skip_rel_path(path: &str) -> bool {
    path.split('/').any(|component| {
        matches!(
            component,
            ".git" | ".ckg" | "node_modules" | "dist" | "build" | "target"
        )
    })
}

fn is_binary_file(path: &Path) -> Result<bool> {
    let mut file = fs::File::open(path)?;
    let mut buf = [0u8; 8192];
    let read = file.read(&mut buf)?;
    if buf[..read].contains(&0) {
        return Ok(true);
    }
    Ok(std::str::from_utf8(&buf[..read]).is_err())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scanner_respects_ignored_dirs() -> Result<()> {
        let dir = tempfile::tempdir()?;
        fs::create_dir_all(dir.path().join("src"))?;
        fs::create_dir_all(dir.path().join("node_modules/pkg"))?;
        fs::write(dir.path().join("src/main.ts"), "export function run() {}")?;
        fs::write(
            dir.path().join("node_modules/pkg/index.js"),
            "module.exports = {}",
        )?;

        let files = scan_repo(dir.path())?;
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].rel_path, "src/main.ts");
        Ok(())
    }
}
