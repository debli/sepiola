//! 按书持久化的 AI 分析结果：每章一条，存放在 `<库>/.sepiola/analysis/<书>.json`。

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::reader::analyze::{Analysis, AnalysisItem, AnalysisStats, Annotation};

/// 单章分析结果；`fingerprint` 记录生成时的正文指纹，正文变化后缓存自动失效。
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ChapterAnalysis {
    pub fingerprint: u64,
    pub summary: String,
    pub items: Vec<AnalysisItem>,
    pub annotations: Vec<Annotation>,
    pub updated_at: i64,
    /// 上次分析的耗时与估算消耗。
    pub stats: AnalysisStats,
}

impl ChapterAnalysis {
    pub fn analysis(&self) -> Analysis {
        Analysis {
            summary: self.summary.clone(),
            items: self.items.clone(),
            annotations: self.annotations.clone(),
            stats: self.stats.clone(),
        }
    }
}

/// 一本书的全部分析结果（一个文件）。
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct BookAnalysis {
    /// 相对书库的书籍路径，用于校验文件归属。
    pub book: String,
    pub title: String,
    pub updated_at: i64,
    pub chapters: BTreeMap<usize, ChapterAnalysis>,
}

impl BookAnalysis {
    /// 指纹匹配的章节结果；正文改过后视为无缓存。
    pub fn entry(&self, chapter: usize, fingerprint: u64) -> Option<&ChapterAnalysis> {
        self.chapters
            .get(&chapter)
            .filter(|entry| entry.fingerprint == fingerprint)
    }

    pub fn insert(&mut self, chapter: usize, fingerprint: u64, analysis: Analysis, now: i64) {
        let entry = ChapterAnalysis {
            fingerprint,
            summary: analysis.summary,
            items: analysis.items,
            annotations: analysis.annotations,
            updated_at: now,
            stats: analysis.stats,
        };
        // 内容为空时视为清空该章。
        if entry.items.is_empty() && entry.summary.trim().is_empty() {
            self.chapters.remove(&chapter);
        } else {
            self.chapters.insert(chapter, entry);
        }
        self.updated_at = now;
    }

    pub fn remove(&mut self, chapter: usize) -> bool {
        self.chapters.remove(&chapter).is_some()
    }
}

/// 正文指纹（FNV-1a），用于判断章节内容是否变化。
pub fn fingerprint(text: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in text.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x1000_0000_01b3);
    }
    hash
}

/// 分析结果文件：`<root>/.sepiola/analysis/<rel>.json`。
pub fn path(root: &Path, rel: &Path) -> PathBuf {
    let mut file = PathBuf::from(".sepiola").join("analysis").join(rel);
    file.set_extension("json");
    root.join(file)
}

pub fn load(root: &Path, rel: &Path) -> BookAnalysis {
    let Ok(text) = fs::read_to_string(path(root, rel)) else {
        return BookAnalysis::default();
    };
    serde_json::from_str(&text).unwrap_or_default()
}

pub fn save(root: &Path, rel: &Path, book: &BookAnalysis) -> Result<(), String> {
    let file = path(root, rel);
    if let Some(parent) = file.parent() {
        fs::create_dir_all(parent).map_err(|error| format!("创建分析目录失败：{error}"))?;
    }
    let json = serde_json::to_string_pretty(book).map_err(|error| format!("序列化失败：{error}"))?;
    let tmp = file.with_extension("json.tmp");
    fs::write(&tmp, json).map_err(|error| format!("写入失败：{error}"))?;
    fs::rename(&tmp, &file).map_err(|error| format!("替换失败：{error}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn analysis_file_mirrors_relative_book_path() {
        let root = Path::new("/library");
        assert_eq!(
            path(root, Path::new("novels/book.epub")),
            PathBuf::from("/library/.sepiola/analysis/novels/book.json")
        );
        assert_eq!(
            path(root, Path::new("plain.epub")),
            PathBuf::from("/library/.sepiola/analysis/plain.json")
        );
    }

    #[test]
    fn fingerprint_changes_with_text() {
        assert_eq!(fingerprint("abc"), fingerprint("abc"));
        assert_ne!(fingerprint("abc"), fingerprint("abd"));
    }

    #[test]
    fn entries_round_trip_and_respect_fingerprint() {
        let mut book = BookAnalysis::default();
        let analysis = Analysis {
            summary: "summary".to_string(),
            items: vec![AnalysisItem {
                text: "take off".to_string(),
                kind: crate::reader::analyze::AnnotationKind::Phrase,
                note: "note".to_string(),
                detail: "detail".to_string(),
            }],
            annotations: vec![Annotation {
                chapter: 2,
                start: 3,
                end: 11,
                kind: crate::reader::analyze::AnnotationKind::Phrase,
                note: "note".to_string(),
                detail: "detail".to_string(),
            }],
            stats: crate::reader::analyze::AnalysisStats {
                chunks: 2,
                elapsed_secs: 12.5,
                tokens: 900,
                sent_chars: 3000,
                received_chars: 600,
            },
        };
        book.insert(2, 42, analysis.clone(), 100);
        assert!(book.entry(2, 42).is_some());
        assert!(book.entry(2, 7).is_none());
        assert_eq!(book.entry(2, 42).map(|e| e.analysis()), Some(analysis));
        assert_eq!(book.updated_at, 100);
        assert!(book.remove(2));
        assert!(book.chapters.is_empty());
    }
}
