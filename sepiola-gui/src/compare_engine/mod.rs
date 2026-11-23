use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use sepiola::book::BookMeta;

mod images;
mod matching;
mod metadata;
mod operations;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompareMode {
    Files,
    Images,
    Books,
    Music,
}

impl CompareMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Files => "文件",
            Self::Images => "图片",
            Self::Books => "书籍",
            Self::Music => "音乐",
        }
    }
}

#[derive(Clone, Debug)]
pub struct CompareItem {
    pub a: Option<PathBuf>,
    pub b: Option<PathBuf>,
    pub details: Vec<String>,
    pub exact_duplicate: bool,
    pub similarity: Option<f32>,
}

#[derive(Clone, Debug, Default)]
pub struct CompareReport {
    pub items: Vec<CompareItem>,
    pub files_a: usize,
    pub files_b: usize,
    pub cancelled: bool,
}

#[derive(Clone, Debug)]
struct FileRecord {
    relative: PathBuf,
    name: String,
    size: u64,
    modified: Option<std::time::SystemTime>,
    hash: Option<String>,
    book: Option<BookMeta>,
    audio: Option<metadata::AudioMeta>,
    difference_hash: Option<u64>,
}

pub fn compare_roots(
    a_root: &Path,
    b_root: &Path,
    mode: CompareMode,
    cancel: &AtomicBool,
    mut progress: impl FnMut(usize, usize) + Send,
    scan_callback: impl Fn(bool, PathBuf) + Sync + Send,
    mut item_callback: impl FnMut(CompareItem),
) -> Result<CompareReport, String> {
    if !a_root.is_dir() || !b_root.is_dir() {
        return Err("A、B 均须为已存在的目录".to_string());
    }
    let a_files = operations::collect_files(a_root, cancel)?;
    let b_files = operations::collect_files(b_root, cancel)?;
    if cancel.load(Ordering::Relaxed) {
        return Ok(CompareReport {
            files_a: a_files.len(),
            files_b: b_files.len(),
            cancelled: true,
            ..Default::default()
        });
    }

    let total = a_files.len() + b_files.len();
    let a_file_count = a_files.len();

    let a_records = operations::inspect_files_parallel(
        a_root,
        a_files,
        mode,
        cancel,
        0,
        total,
        &mut progress,
        true,
        &scan_callback,
    );
    if cancel.load(Ordering::Relaxed) {
        return Ok(CompareReport {
            files_a: a_records.len(),
            files_b: b_files.len(),
            cancelled: true,
            ..Default::default()
        });
    }

    let a_offset = a_file_count;
    let b_records = operations::inspect_files_parallel(
        b_root,
        b_files,
        mode,
        cancel,
        a_offset,
        total,
        &mut progress,
        false,
        &scan_callback,
    );
    if cancel.load(Ordering::Relaxed) {
        return Ok(CompareReport {
            files_a: a_records.len(),
            files_b: b_records.len(),
            cancelled: true,
            ..Default::default()
        });
    }

    Ok(matching::compare_records(
        a_records,
        b_records,
        mode,
        cancel,
        &mut item_callback,
    ))
}

pub fn exact_duplicate_paths(
    keep_root: &Path,
    remove_root: &Path,
    mode: CompareMode,
    cancel: &AtomicBool,
) -> Result<Vec<PathBuf>, String> {
    operations::exact_duplicate_paths(keep_root, remove_root, mode, cancel)
}

pub fn sync_files(
    source_root: &Path,
    target_root: &Path,
    mode: CompareMode,
    cancel: &AtomicBool,
    progress: impl FnMut(usize, usize),
) -> Result<(usize, usize), String> {
    operations::sync_files(source_root, target_root, mode, cancel, progress)
}

pub fn copy_selected_files(
    source_root: &Path,
    target_root: &Path,
    files: &[(PathBuf, PathBuf)],
    cancel: &AtomicBool,
    progress: impl FnMut(usize, usize),
) -> Result<(usize, usize), String> {
    operations::copy_selected_files(source_root, target_root, files, cancel, progress)
}

pub fn remove_exact_duplicates(
    keep_root: &Path,
    remove_root: &Path,
    mode: CompareMode,
    candidates: &[PathBuf],
    cancel: &AtomicBool,
) -> Result<(usize, usize), String> {
    operations::remove_exact_duplicates(keep_root, remove_root, mode, candidates, cancel)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use sepiola::book::BookMeta;

    use super::images::{hash_buckets, similarity};
    use super::metadata::differences;
    use super::{CompareMode, FileRecord};

    #[test]
    fn image_similarity_is_one_for_equal_hashes_and_bounded() {
        assert_eq!(similarity(0, 0), 1.0);
        assert_eq!(similarity(u64::MAX, 0), 0.0);
        assert!(similarity(0, 1) > 0.98);
    }

    #[test]
    fn image_bucket_index_covers_the_similarity_threshold() {
        let left = 0u64;
        let right = (1u64 << 0)
            | (1u64 << 6)
            | (1u64 << 12)
            | (1u64 << 18)
            | (1u64 << 24)
            | (1u64 << 30)
            | (1u64 << 36)
            | (1u64 << 42)
            | (1u64 << 48)
            | (1u64 << 54);
        assert_eq!((left ^ right).count_ones(), 10);
        let left_buckets = hash_buckets(left);
        assert!(hash_buckets(right)
            .iter()
            .any(|bucket| left_buckets.contains(bucket)));
    }

    #[test]
    fn book_comparison_reports_requested_metadata_fields() {
        let left = FileRecord {
            relative: PathBuf::from("left.epub"),
            name: "left.epub".to_string(),
            size: 1024,
            modified: None,
            hash: Some("a".to_string()),
            book: Some(BookMeta {
                author: Some("A".to_string()),
                publisher: Some("P1".to_string()),
                edition: Some("1".to_string()),
                translator: Some("T".to_string()),
                ..Default::default()
            }),
            audio: None,
            difference_hash: None,
        };
        let right = FileRecord {
            relative: PathBuf::from("right.epub"),
            name: "right.epub".to_string(),
            size: 2048,
            modified: None,
            hash: Some("b".to_string()),
            book: Some(BookMeta {
                author: Some("B".to_string()),
                publisher: Some("P2".to_string()),
                edition: Some("2".to_string()),
                translator: Some("U".to_string()),
                ..Default::default()
            }),
            audio: None,
            difference_hash: None,
        };
        let details = differences(&left, &right, CompareMode::Books).join(" ");
        for field in ["名称", "大小", "作者", "出版社", "版次", "译者"] {
            assert!(details.contains(field), "missing {field}: {details}");
        }
    }
}
