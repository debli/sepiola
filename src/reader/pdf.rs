//! PDF 阅读：使用纯 Rust `pdf-extract` 取文本层，按页或段落分块。

use std::path::Path;

use super::{BookDocument, Chapter};

const MAX_BYTES: u64 = 256 * 1024 * 1024;
/// 无分页符时的分块长度（字符数），控制单页排版耗时。
const CHUNK_CHARS: usize = 4000;

pub fn parse(path: &Path) -> Result<BookDocument, String> {
    let text = crate::pdf_text::extract_cached(path, MAX_BYTES)?;
    if text.trim().is_empty() {
        return Err("PDF 无文本层（扫描版），暂不支持阅读".to_string());
    }
    let title = path
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned());
    Ok(BookDocument {
        title,
        author: None,
        chapters: split(&text),
    })
}

fn split(text: &str) -> Vec<Chapter> {
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    if normalized.contains('\u{c}') {
        let pages: Vec<Chapter> = normalized
            .split('\u{c}')
            .enumerate()
            .filter(|(_, page)| !page.trim().is_empty())
            .map(|(index, page)| Chapter {
                title: format!("第 {} 页", index + 1),
                text: page.trim().to_string(),
                spans: Vec::new(),
                links: Vec::new(),
                images: Vec::new(),
            })
            .collect();
        if !pages.is_empty() {
            return pages;
        }
    }
    chunk(&normalized)
}

fn chunk(text: &str) -> Vec<Chapter> {
    let mut chapters = Vec::new();
    let mut buffer = String::new();
    for line in text.lines() {
        buffer.push_str(line);
        buffer.push('\n');
        if buffer.chars().count() >= CHUNK_CHARS {
            push(&mut chapters, &mut buffer);
        }
    }
    if !buffer.trim().is_empty() {
        push(&mut chapters, &mut buffer);
    }
    chapters
}

fn push(chapters: &mut Vec<Chapter>, buffer: &mut String) {
    let text = buffer.trim();
    if text.is_empty() {
        buffer.clear();
        return;
    }
    chapters.push(Chapter {
        title: format!("第 {} 部分", chapters.len() + 1),
        text: text.to_string(),
        spans: Vec::new(),
        links: Vec::new(),
        images: Vec::new(),
    });
    buffer.clear();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_pages_on_form_feed() {
        let chapters = split("第一页内容\n\u{c}第二页内容");
        assert_eq!(chapters.len(), 2);
        assert_eq!(chapters[0].title, "第 1 页");
        assert_eq!(chapters[1].text, "第二页内容");
    }

    #[test]
    fn opens_pdf_with_text_layer() {
        let dir = std::env::temp_dir().join(format!(
            "sepiola-reader-pdf-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).expect("mkdir");
        let path = dir.join("book.pdf");
        std::fs::write(&path, crate::book::build_test_pdf(&["Hello book"])).expect("write");
        let book = parse(&path).expect("pdf");
        assert!(book.chapters.iter().any(|chapter| chapter.text.contains("Hello book")));
        std::fs::remove_dir_all(&dir).ok();
    }
}
