//! 用户手动高亮：按书持久化在 `<库>/.sepiola/highlights/<书>.json`。

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// 一段用户高亮：章节内的字节区间与颜色。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Highlight {
    pub chapter: usize,
    /// 章节正文中的起止字节（左闭右开）。
    pub start: usize,
    pub end: usize,
    /// RGB 颜色。
    pub color: [u8; 3],
}

/// 一本书的全部高亮（一个文件）。
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct BookHighlights {
    /// 相对书库的书籍路径，用于校验文件归属。
    pub book: String,
    pub items: Vec<Highlight>,
}

impl BookHighlights {
    /// 同一章中与 `start..end` 有交集的条目。
    pub fn overlapping(&self, chapter: usize, start: usize, end: usize) -> impl Iterator<Item = &Highlight> {
        self.items
            .iter()
            .filter(move |item| item.chapter == chapter && item.start < end && start < item.end)
    }

    /// 添加一段高亮：覆盖到的旧高亮先移除，再按位置插入。
    pub fn add(&mut self, highlight: Highlight) {
        self.remove(highlight.chapter, highlight.start, highlight.end);
        self.items.push(highlight);
        self.items.sort_by_key(|item| (item.chapter, item.start, item.end));
    }

    /// 移除与区间相交的高亮，返回是否删掉了东西。
    pub fn remove(&mut self, chapter: usize, start: usize, end: usize) -> bool {
        let before = self.items.len();
        self.items
            .retain(|item| !(item.chapter == chapter && item.start < end && start < item.end));
        before != self.items.len()
    }
}

/// 高亮文件：`<root>/.sepiola/highlights/<rel>.json`。
pub fn path(root: &Path, rel: &Path) -> PathBuf {
    let mut file = PathBuf::from(".sepiola").join("highlights").join(rel);
    file.set_extension("json");
    root.join(file)
}

pub fn load(root: &Path, rel: &Path) -> BookHighlights {
    let Ok(text) = fs::read_to_string(path(root, rel)) else {
        return BookHighlights::default();
    };
    serde_json::from_str(&text).unwrap_or_default()
}

pub fn save(root: &Path, rel: &Path, book: &BookHighlights) -> Result<(), String> {
    let file = path(root, rel);
    if let Some(parent) = file.parent() {
        fs::create_dir_all(parent).map_err(|error| format!("创建高亮目录失败：{error}"))?;
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

    fn highlight(start: usize, end: usize) -> Highlight {
        Highlight {
            chapter: 1,
            start,
            end,
            color: [255, 221, 105],
        }
    }

    #[test]
    fn adding_overlapping_highlight_replaces_it() {
        let mut book = BookHighlights::default();
        book.add(highlight(10, 20));
        book.add(highlight(15, 30));
        assert_eq!(book.items.len(), 1);
        assert_eq!(book.items[0].start, 15);
        assert_eq!(book.items[0].end, 30);
    }

    #[test]
    fn removed_range_only_touches_intersections() {
        let mut book = BookHighlights::default();
        book.add(highlight(10, 20));
        book.add(highlight(40, 50));
        assert!(!book.remove(1, 20, 40));
        assert_eq!(book.items.len(), 2);
        assert!(book.remove(1, 19, 21));
        assert_eq!(book.items.len(), 1);
        assert_eq!(book.items[0].start, 40);
    }

    #[test]
    fn file_path_mirrors_book_location() {
        assert_eq!(
            path(Path::new("/library"), Path::new("novels/book.epub")),
            PathBuf::from("/library/.sepiola/highlights/novels/book.json")
        );
    }
}
