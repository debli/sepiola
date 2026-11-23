use std::collections::HashSet;
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex;

use rayon::prelude::*;
use sepiola::book;
use sepiola::library::{self, FileKind};
use sha2::{Digest, Sha256};
use xxhash_rust::xxh3::Xxh3;

use super::images::difference_hash;
use super::metadata::audio_meta;
use super::{CompareMode, FileRecord};

pub(super) fn collect_files(root: &Path, cancel: &AtomicBool) -> Result<Vec<PathBuf>, String> {
    let mut pending = vec![root.to_path_buf()];
    let mut files = Vec::new();
    while let Some(directory) = pending.pop() {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        let entries = fs::read_dir(&directory)
            .map_err(|error| format!("读取目录 {} 失败：{error}", directory.display()))?;
        for entry in entries {
            if cancel.load(Ordering::Relaxed) {
                break;
            }
            let entry = entry.map_err(|error| error.to_string())?;
            let kind = entry.file_type().map_err(|error| error.to_string())?;
            if kind.is_dir() {
                pending.push(entry.path());
            } else if kind.is_file() {
                files.push(entry.path());
            }
        }
    }
    files.sort();
    Ok(files)
}

pub(super) fn inspect_files_parallel(
    root: &Path,
    files: Vec<PathBuf>,
    mode: CompareMode,
    cancel: &AtomicBool,
    done: usize,
    total: usize,
    progress: &mut (impl FnMut(usize, usize) + Send),
    side_a: bool,
    scan_callback: &(impl Fn(bool, PathBuf) + Sync + Send),
) -> Vec<FileRecord> {
    let processed = AtomicUsize::new(0);
    let records: Mutex<Vec<FileRecord>> = Mutex::new(Vec::new());
    let progress = Mutex::new(progress);

    files.par_iter().for_each(|path| {
        if cancel.load(Ordering::Relaxed) {
            return;
        }
        let relative = match path.strip_prefix(root) {
            Ok(relative) => relative.to_path_buf(),
            Err(_) => return,
        };
        let mut scanned_relative = None;
        if is_in_mode(path, mode) {
            if let Some(hash) = fast_hash_file(path, cancel) {
                scanned_relative = Some(relative.clone());
                let metadata = fs::metadata(path).ok();
                let size = metadata.as_ref().map(|m| m.len()).unwrap_or(0);
                let record = FileRecord {
                    name: path
                        .file_name()
                        .map(|name| name.to_string_lossy().to_string())
                        .unwrap_or_default(),
                    size,
                    modified: metadata.as_ref().and_then(|meta| meta.modified().ok()),
                    hash: Some(hash),
                    book: (mode == CompareMode::Books)
                        .then(|| book::extract(path))
                        .flatten(),
                    audio: (mode == CompareMode::Music).then(|| audio_meta(path)),
                    difference_hash: (mode == CompareMode::Images)
                        .then(|| difference_hash(path))
                        .flatten(),
                    relative,
                };
                if let Ok(mut guard) = records.lock() {
                    guard.push(record);
                }
            }
        }
        processed.fetch_add(1, Ordering::Relaxed);
        let current = processed.load(Ordering::Relaxed);
        if current % 32 == 0 || current == files.len() {
            if let Some(relative) = scanned_relative {
                scan_callback(side_a, relative);
            }
            if let Ok(mut p) = progress.lock() {
                p(current + done, total);
            }
        }
    });

    let mut records = records.into_inner().unwrap_or_default();
    records.sort_by(|a, b| a.relative.cmp(&b.relative));
    records
}

pub(super) fn fast_hash_file(path: &Path, cancel: &AtomicBool) -> Option<String> {
    let mut file = File::open(path).ok()?;
    let mut hasher = Xxh3::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        if cancel.load(Ordering::Relaxed) {
            return None;
        }
        let count = file.read(&mut buffer).ok()?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    Some(format!("{:x}", hasher.digest()))
}

pub(super) fn hash_file(path: &Path, cancel: &AtomicBool) -> Option<String> {
    let mut file = File::open(path).ok()?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        if cancel.load(Ordering::Relaxed) {
            return None;
        }
        let count = file.read(&mut buffer).ok()?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    Some(format!("{:x}", hasher.finalize()))
}

pub(super) fn exact_duplicate_paths(
    keep_root: &Path,
    remove_root: &Path,
    mode: CompareMode,
    cancel: &AtomicBool,
) -> Result<Vec<PathBuf>, String> {
    let keep_files: Vec<_> = collect_files(keep_root, cancel)?
        .into_iter()
        .filter(|path| is_in_mode(path, mode))
        .collect();
    let remove_files: Vec<_> = collect_files(remove_root, cancel)?
        .into_iter()
        .filter(|path| is_in_mode(path, mode))
        .collect();
    let mut keep_hashes = HashSet::new();
    for path in keep_files {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        if let Some(hash) = hash_file(&path, cancel) {
            keep_hashes.insert(hash);
        }
    }
    let mut remove = Vec::new();
    for path in remove_files {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        if hash_file(&path, cancel).is_some_and(|hash| keep_hashes.contains(&hash)) {
            remove.push(path);
        }
    }
    Ok(remove)
}

pub(super) fn sync_files(
    source_root: &Path,
    target_root: &Path,
    mode: CompareMode,
    cancel: &AtomicBool,
    mut progress: impl FnMut(usize, usize),
) -> Result<(usize, usize), String> {
    let files: Vec<_> = collect_files(source_root, cancel)?
        .into_iter()
        .filter(|path| is_in_mode(path, mode))
        .collect();
    let total = files.len();
    let mut copied = 0;
    for source in files {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        let relative = source
            .strip_prefix(source_root)
            .map_err(|error| error.to_string())?;
        let target = target_root.join(relative);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        fs::copy(&source, &target).map_err(|error| {
            format!(
                "复制 {} 到 {} 失败：{error}",
                source.display(),
                target.display()
            )
        })?;
        copied += 1;
        progress(copied, total);
    }
    Ok((copied, total))
}

pub(super) fn copy_selected_files(
    source_root: &Path,
    target_root: &Path,
    files: &[(PathBuf, PathBuf)],
    cancel: &AtomicBool,
    mut progress: impl FnMut(usize, usize),
) -> Result<(usize, usize), String> {
    let total = files.len();
    let mut copied = 0;
    for (source_relative, target_relative) in files {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        if source_relative.is_absolute()
            || target_relative.is_absolute()
            || source_relative
                .components()
                .any(|part| part == std::path::Component::ParentDir)
            || target_relative
                .components()
                .any(|part| part == std::path::Component::ParentDir)
        {
            return Err("比较项路径必须位于所选目录内".to_string());
        }
        let source = source_root.join(source_relative);
        let target = target_root.join(target_relative);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        fs::copy(&source, &target).map_err(|error| {
            format!(
                "复制 {} 到 {} 失败：{error}",
                source.display(),
                target.display()
            )
        })?;
        copied += 1;
        progress(copied, total);
    }
    Ok((copied, total))
}

pub(super) fn remove_exact_duplicates(
    keep_root: &Path,
    remove_root: &Path,
    mode: CompareMode,
    candidates: &[PathBuf],
    cancel: &AtomicBool,
) -> Result<(usize, usize), String> {
    let keep_files: Vec<_> = collect_files(keep_root, cancel)?
        .into_iter()
        .filter(|path| is_in_mode(path, mode))
        .collect();
    let mut keep_hashes = HashSet::new();
    for keep_file in &keep_files {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        if let Some(hash) = hash_file(keep_file, cancel) {
            keep_hashes.insert(hash);
        }
    }
    let total = candidates.len();
    let mut removed = 0;
    for candidate in candidates {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        if !candidate.starts_with(remove_root) || !is_in_mode(candidate, mode) {
            continue;
        }
        let Some(candidate_hash) = hash_file(candidate, cancel) else {
            continue;
        };
        if keep_hashes.contains(&candidate_hash) {
            fs::remove_file(candidate)
                .map_err(|error| format!("删除重复文件 {} 失败：{error}", candidate.display()))?;
            removed += 1;
        }
    }
    Ok((removed, total))
}

fn is_in_mode(path: &Path, mode: CompareMode) -> bool {
    match mode {
        CompareMode::Files => true,
        CompareMode::Images => library::classify_kind(path) == FileKind::Image,
        CompareMode::Books => library::classify_kind(path) == FileKind::Book,
        CompareMode::Music => matches!(
            library::classify_kind(path),
            FileKind::Audio | FileKind::Recording
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skips_files_that_cannot_be_read() {
        let root = std::env::temp_dir().join(format!(
            "sepiola-read-failure-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let missing_file = root.join("missing.jpg");
        let cancel = AtomicBool::new(false);
        let progress_calls = AtomicUsize::new(0);
        let mut progress = |_, _| {
            progress_calls.fetch_add(1, Ordering::Relaxed);
        };

        let records = inspect_files_parallel(
            &root,
            vec![missing_file],
            CompareMode::Files,
            &cancel,
            0,
            1,
            &mut progress,
            false,
            &|_, _| {},
        );

        assert!(records.is_empty());
        assert_eq!(progress_calls.load(Ordering::Relaxed), 1);
    }
}
