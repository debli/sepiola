//! MOBI / AZW / AZW3 阅读：由纯 Rust `mobi` crate 解 PalmDOC / HUFF-CDIC 压缩正文。

use std::path::Path;

use ::mobi::headers::Encryption;
use ::mobi::Mobi;

use super::markup::html_to_text;
use super::{text, BookDocument};

pub fn parse(path: &Path) -> Result<BookDocument, String> {
    let book = Mobi::from_path(path).map_err(|error| format!("MOBI 解析失败：{error}"))?;
    if book.encryption() != Encryption::No {
        return Err("书籍已加密（DRM），无法读取正文".to_string());
    }
    // 不少 MOBI 头部声明 UTF-8 却混入 CP1252 字节，严格解码会整本失败，故取有损结果。
    // KF8/混合版常在正文前带全零填充记录，一并剔除空字符。
    let raw = book.content_as_string_lossy().replace('\0', "");
    let content = html_to_text(&raw);
    if content.trim().is_empty() {
        return Err("MOBI 中没有可读正文".to_string());
    }
    let title = book.title();
    Ok(BookDocument {
        title: (!title.trim().is_empty()).then_some(title),
        author: book.author(),
        chapters: text::split_chapters(&content),
    })
}
