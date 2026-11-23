//! TXT 阅读：编码识别与章节切分。

use std::path::Path;

use encoding_rs::{GBK, UTF_16BE, UTF_16LE};

use super::{BookDocument, Chapter};

const MAX_BYTES: usize = 64 * 1024 * 1024;
/// 无章节标题时的分块长度（字符数），避免整本书一次排版卡住界面。
const CHUNK_CHARS: usize = 6000;

pub fn parse_file(path: &Path) -> Result<BookDocument, String> {
    let bytes = std::fs::metadata(path)
        .map_err(|error| format!("读取 {} 失败：{error}", path.display()))?;
    if bytes.len() > MAX_BYTES as u64 {
        return Err("TXT 文件超过 64MB，暂不支持".to_string());
    }
    let bytes = std::fs::read(path).map_err(|error| format!("读取 {} 失败：{error}", path.display()))?;
    let text = decode(&bytes);
    if text.trim().is_empty() {
        return Err("TXT 文件为空".to_string());
    }
    let title = path
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned());
    Ok(BookDocument {
        title,
        author: None,
        chapters: split_chapters(&text),
        is_pdf: false,
    })
}

/// 依次尝试 BOM、UTF-8、GBK；都失败时按 UTF-8 有损解码。
pub fn decode(bytes: &[u8]) -> String {
    if let Some(rest) = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]) {
        return String::from_utf8_lossy(rest).into_owned();
    }
    if let Some(rest) = bytes.strip_prefix(&[0xff, 0xfe]) {
        return UTF_16LE.decode(rest).0.into_owned();
    }
    if let Some(rest) = bytes.strip_prefix(&[0xfe, 0xff]) {
        return UTF_16BE.decode(rest).0.into_owned();
    }
    if let Ok(text) = std::str::from_utf8(bytes) {
        return text.to_string();
    }
    GBK.decode(bytes).0.into_owned()
}

/// 按「第X章/节/回/卷」或 Chapter N 等标题行切分；无标题时按段落分块。
pub fn split_chapters(text: &str) -> Vec<Chapter> {
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    let mut chapters: Vec<Chapter> = Vec::new();
    let mut title = String::new();
    let mut body = String::new();
    for line in normalized.lines() {
        if heading_like(line) {
            flush(&mut chapters, &mut title, &mut body);
            title = line.trim().trim_start_matches('#').trim().to_string();
        } else {
            body.push_str(line);
            body.push('\n');
        }
    }
    flush(&mut chapters, &mut title, &mut body);
    if chapters.is_empty() {
        return chunk(&normalized, "正文");
    }
    if chapters.len() == 1 && chapters[0].text.chars().count() > CHUNK_CHARS {
        let base = if chapters[0].title.is_empty() {
            "正文".to_string()
        } else {
            chapters[0].title.clone()
        };
        return chunk(&chapters[0].text, &base);
    }
    chapters
}

fn flush(chapters: &mut Vec<Chapter>, title: &mut String, body: &mut String) {
    let text = body.trim();
    if !text.is_empty() {
        chapters.push(Chapter {
            title: if title.trim().is_empty() {
                "正文".to_string()
            } else {
                title.trim().to_string()
            },
            text: text.to_string(),
            spans: Vec::new(),
            links: Vec::new(),
            images: Vec::new(),
        });
    }
    body.clear();
    title.clear();
}

/// 标题行：长度受限，命中「第X章」或常见章节关键词。
fn heading_like(raw: &str) -> bool {
    let line = raw.trim().trim_start_matches('#').trim();
    let length = line.chars().count();
    if line.is_empty() || length > 40 {
        return false;
    }
    let lower = line.to_ascii_lowercase();
    if ["chapter ", "part ", "section ", "prologue", "epilogue"]
        .iter()
        .any(|prefix| lower.starts_with(prefix))
    {
        return true;
    }
    if ["序章", "楔子", "前言", "序言", "后记", "尾声", "番外", "附录"]
        .iter()
        .any(|keyword| line.starts_with(keyword))
    {
        return true;
    }
    let mut chars = line.chars();
    if chars.next() != Some('第') {
        return false;
    }
    let mut digits = 0;
    for ch in chars {
        if ch.is_whitespace() {
            continue;
        }
        if ch.is_ascii_digit() || "一二三四五六七八九十百千零两〇".contains(ch) {
            digits += 1;
            if digits > 6 {
                return false;
            }
        } else {
            return digits > 0 && "章节回卷部篇集".contains(ch);
        }
    }
    false
}

/// 在段落边界把长文分块，标题为「基础名（N）」。
fn chunk(text: &str, base: &str) -> Vec<Chapter> {
    let mut chapters = Vec::new();
    let mut buffer = String::new();
    for line in text.lines() {
        buffer.push_str(line);
        buffer.push('\n');
        if buffer.chars().count() >= CHUNK_CHARS {
            push_chunk(&mut chapters, base, &mut buffer);
        }
    }
    if !buffer.trim().is_empty() {
        push_chunk(&mut chapters, base, &mut buffer);
    }
    chapters
}

fn push_chunk(chapters: &mut Vec<Chapter>, base: &str, buffer: &mut String) {
    let text = buffer.trim();
    if text.is_empty() {
        buffer.clear();
        return;
    }
    let index = chapters.len() + 1;
    chapters.push(Chapter {
        title: format!("{base}（{index}）"),
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
    fn decodes_utf8_bom_and_gbk() {
        assert_eq!(decode(&[0xef, 0xbb, 0xbf, b'h', b'i']), "hi");
        let gbk: Vec<u8> = vec![0xd6, 0xd0, 0xb9, 0xfa]; // 中国
        assert_eq!(decode(&gbk), "中国");
        assert_eq!(decode("第一行".as_bytes()), "第一行");
    }

    #[test]
    fn splits_chinese_chapters() {
        let text = "第一章 初见\n他推开门。\n\n第二章 离别\n她转身离开。\n";
        let chapters = split_chapters(text);
        assert_eq!(chapters.len(), 2);
        assert_eq!(chapters[0].title, "第一章 初见");
        assert!(chapters[0].text.contains("他推开门"));
        assert_eq!(chapters[1].title, "第二章 离别");
    }

    #[test]
    fn splits_long_text_without_headings() {
        let text = "段落。\n".repeat(CHUNK_CHARS);
        let chapters = split_chapters(&text);
        assert!(chapters.len() > 1);
        assert!(chapters[0].title.starts_with("正文"));
    }

    #[test]
    fn chapter_word_requires_number() {
        assert!(!heading_like("第一次见面"));
        assert!(heading_like("第 1 章"));
        assert!(heading_like("Chapter 3"));
    }
}
