//! 电子书内容级元数据抽取（M36）：直接解析 EPUB / FB2 / MOBI·AZW / AZW3 / PDF
//! 文件自身的内容（而非文件名或外部 metadata 服务），抽取书名、作者、出版社、
//! 出版年份、译者、版本、ISBN。解析失败或所有字段均缺失时返回 `None`。
//!
//! 已知局限：PDF 依赖文本层，扫描版（纯图片）无 OCR 时返回 `None`；
//! MOBI/AZW 仅读 EXTH 头部字段，正文级译者/版本不解析；djvu 暂不支持。

use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use encoding_rs::{BIG5, EUC_KR, GBK, SHIFT_JIS, UTF_8, WINDOWS_1252};
use quick_xml::events::Event as XmlEvent;
use serde::{Deserialize, Serialize};

const MAX_CONTENT_BYTES: usize = 64 * 1024 * 1024;

/// 内容级抽取的书籍元数据，各字段独立可缺省。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct BookMeta {
    pub title: Option<String>,
    pub author: Option<String>,
    pub publisher: Option<String>,
    /// 出版年份。
    pub year: Option<u32>,
    pub translator: Option<String>,
    pub edition: Option<String>,
    pub isbn: Option<String>,
}

#[cfg(test)]
pub(crate) fn build_test_pdf(lines: &[&str]) -> Vec<u8> {
    let ops: Vec<String> = lines
        .iter()
        .map(|line| format!("BT /F1 12 Tf 72 700 Td ({}) Tj ET", line.replace('\\', "\\\\")))
        .collect();
    let stream = ops.join("\n");
    let objects = vec![
        "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>".to_string(),
        format!("<< /Length {} >>\nstream\n{stream}\nendstream", stream.len()),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_string(),
    ];
    let mut pdf = String::from("%PDF-1.4\n");
    let mut offsets = Vec::new();
    for (index, object) in objects.iter().enumerate() {
        offsets.push(pdf.len());
        pdf.push_str(&format!("{} 0 obj\n{object}\nendobj\n", index + 1));
    }
    let xref_pos = pdf.len();
    pdf.push_str("xref\n0 6\n0000000000 65535 f \n");
    for offset in offsets {
        pdf.push_str(&format!("{offset:010} 00000 n \n"));
    }
    pdf.push_str(&format!("trailer\n<< /Size 6 /Root 1 0 R >>\nstartxref\n{xref_pos}\n%%EOF\n"));
    pdf.into_bytes()
}

impl BookMeta {
    /// 所有字段均为空（视为抽取失败）。
    pub fn is_empty(&self) -> bool {
        self.title.is_none()
            && self.author.is_none()
            && self.publisher.is_none()
            && self.year.is_none()
            && self.translator.is_none()
            && self.edition.is_none()
            && self.isbn.is_none()
    }

}

/// 按扩展名分派内容级抽取；不支持的类型或全部字段缺失时返回 `None`。
pub fn extract(path: &Path) -> Option<BookMeta> {
    extract_inner(path, None)
}

/// 可取消版本：解析过程中轮询 `cancel`，取消后尽快返回 `None`。
pub fn extract_cancellable(path: &Path, cancel: &AtomicBool) -> Option<BookMeta> {
    extract_inner(path, Some(cancel))
}

fn extract_inner(path: &Path, cancel: Option<&AtomicBool>) -> Option<BookMeta> {
    if is_cancelled(cancel) {
        return None;
    }
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())?;
    let meta = match ext.as_str() {
        "epub" => epub_meta(path, cancel)?,
        "fb2" => fb2_meta(path, cancel)?,
        "mobi" | "azw" | "azw3" | "asw3" => mobi_meta(path, cancel)?,
        "pdf" => pdf_meta(path, cancel)?,
        // TXT 无内嵌元数据，用文件名作书名，避免书籍页把它统计为提取失败。
        "txt" => BookMeta {
            title: path
                .file_stem()
                .map(|stem| stem.to_string_lossy().into_owned()),
            ..BookMeta::default()
        },
        _ => return None,
    };
    (!meta.is_empty()).then_some(meta)
}

fn is_cancelled(cancel: Option<&AtomicBool>) -> bool {
    cancel.is_some_and(|flag| flag.load(Ordering::Relaxed))
}

/// 判断电子书正文是否包含关键词；格式解析在调用方后台线程运行。
pub fn contains_text(path: &Path, needle: &str) -> bool {
    let needle = needle.trim().to_lowercase();
    if needle.is_empty() {
        return false;
    }
    let Some(ext) = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
    else {
        return false;
    };
    let text = match ext.as_str() {
        "epub" => epub_content_text(path, None),
        "pdf" => pdf_content_text(path, None),
        "fb2" => fb2_content_text(path, None),
        "mobi" | "azw" | "azw3" | "asw3" => mobi_content_text(path, None),
        _ => None,
    };
    text.is_some_and(|text| text.to_lowercase().contains(&needle))
}

fn read_bounded(path: &Path, cancel: Option<&AtomicBool>) -> Option<Vec<u8>> {
    let file = std::fs::File::open(path).ok()?;
    if file.metadata().ok()?.len() > MAX_CONTENT_BYTES as u64 {
        return None;
    }
    read_stream(file, Some(MAX_CONTENT_BYTES), cancel)
}

/// 分块读取（不设上限），每块前检查取消标志，避免大文件读盘期间长时间无法取消。
fn read_all_cancellable(path: &Path, cancel: Option<&AtomicBool>) -> Option<Vec<u8>> {
    read_stream(std::fs::File::open(path).ok()?, None, cancel)
}

fn read_stream<R: Read>(
    mut reader: R,
    limit: Option<usize>,
    cancel: Option<&AtomicBool>,
) -> Option<Vec<u8>> {
    let mut bytes = Vec::new();
    let mut chunk = [0u8; 256 * 1024];
    loop {
        if is_cancelled(cancel) {
            return None;
        }
        if limit.is_some_and(|limit| bytes.len() >= limit) {
            break;
        }
        let cap = limit.map_or(chunk.len(), |limit| (limit - bytes.len()).min(chunk.len()));
        if cap == 0 {
            break;
        }
        let read = reader.read(&mut chunk[..cap]).ok()?;
        if read == 0 {
            break;
        }
        bytes.extend_from_slice(&chunk[..read]);
    }
    Some(bytes)
}

fn epub_content_text(path: &Path, cancel: Option<&AtomicBool>) -> Option<String> {
    let file = std::fs::File::open(path).ok()?;
    let mut zip = zip::ZipArchive::new(file).ok()?;
    let mut text = String::new();
    let mut remaining = MAX_CONTENT_BYTES;
    for index in 0..zip.len() {
        if remaining == 0 || is_cancelled(cancel) {
            break;
        }
        let entry = zip.by_index(index).ok()?;
        let name = entry.name().to_ascii_lowercase();
        if !(name.ends_with(".xhtml") || name.ends_with(".html") || name.ends_with(".htm")) {
            continue;
        }
        let mut bytes = Vec::new();
        entry
            .take(remaining as u64)
            .read_to_end(&mut bytes)
            .ok()?;
        remaining -= bytes.len();
        text.push_str(&strip_markup(&String::from_utf8_lossy(&bytes)));
    }
    Some(text)
}

fn pdf_content_text(path: &Path, cancel: Option<&AtomicBool>) -> Option<String> {
    if is_cancelled(cancel) {
        return None;
    }
    let text = crate::pdf_text::extract_cached(path, MAX_CONTENT_BYTES as u64).ok()?;
    (!is_cancelled(cancel)).then_some(text)
}

fn fb2_content_text(path: &Path, cancel: Option<&AtomicBool>) -> Option<String> {
    let bytes = read_bounded(path, cancel)?;
    let raw = String::from_utf8_lossy(&bytes);
    let mut reader = quick_xml::Reader::from_str(&raw);
    reader.config_mut().trim_text(true);
    let mut text = String::new();
    loop {
        if is_cancelled(cancel) {
            return None;
        }
        match reader.read_event() {
            Ok(XmlEvent::Text(event)) => text.push_str(&event.unescape().ok()?),
            Ok(XmlEvent::CData(event)) => text.push_str(&String::from_utf8_lossy(&event)),
            Ok(XmlEvent::Eof) | Err(_) => break,
            _ => {}
        }
    }
    Some(text)
}

fn strip_markup(raw: &str) -> String {
    let mut text = String::with_capacity(raw.len());
    let mut in_tag = false;
    for ch in raw.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => {
                in_tag = false;
                text.push(' ');
            }
            _ if in_tag => {}
            _ => text.push(ch),
        }
    }
    text.replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&#39;", "'")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn mobi_content_text(path: &Path, cancel: Option<&AtomicBool>) -> Option<String> {
    if is_cancelled(cancel) {
        return None;
    }
    let mut file = std::fs::File::open(path).ok()?;
    let file_len = file.metadata().ok()?.len();
    let mut database_header = [0u8; 78];
    file.read_exact(&mut database_header).ok()?;
    let record_count = u16::from_be_bytes([database_header[76], database_header[77]]) as usize;
    if record_count < 2 || record_count > 65_536 {
        return None;
    }
    let table_len = record_count.checked_mul(8)?;
    let table_end = 78u64.checked_add(table_len as u64)?;
    if table_end > file_len {
        return None;
    }
    let mut table = vec![0u8; table_len];
    file.read_exact(&mut table).ok()?;
    let offsets: Vec<u64> = table
        .chunks_exact(8)
        .map(|entry| Some(u32::from_be_bytes(entry[..4].try_into().ok()?) as u64))
        .collect::<Option<_>>()?;
    if offsets.iter().any(|offset| *offset < table_end || *offset >= file_len)
        || offsets.windows(2).any(|pair| pair[0] >= pair[1])
    {
        return None;
    }
    let read_record = |file: &mut std::fs::File, index: usize| -> Option<Vec<u8>> {
        let start = *offsets.get(index)?;
        let end = offsets.get(index + 1).copied().unwrap_or(file_len);
        let len = end.checked_sub(start)?;
        if len == 0 || len > MAX_CONTENT_BYTES as u64 {
            return None;
        }
        file.seek(SeekFrom::Start(start)).ok()?;
        let mut bytes = vec![0u8; len as usize];
        file.read_exact(&mut bytes).ok()?;
        Some(bytes)
    };
    let record0 = read_record(&mut file, 0)?;
    let compression = u16_be(&record0, 0)?;
    if !matches!(compression, 1 | 2) || u16_be(&record0, 12)? != 0 {
        return None;
    }
    let text_length = u32_be(&record0, 4)? as usize;
    let text_records = (u16_be(&record0, 8)? as usize).min(record_count - 1);
    let mobi_offset = find_sub(&record0, b"MOBI", 16)?;
    let codepage = u32_be(&record0, mobi_offset + 12)?;
    let mut content = Vec::with_capacity(text_length.min(MAX_CONTENT_BYTES));
    for index in 1..=text_records {
        if content.len() >= MAX_CONTENT_BYTES || is_cancelled(cancel) {
            break;
        }
        let record = read_record(&mut file, index)?;
        let decoded = if compression == 1 {
            record
        } else {
            decompress_palmdoc(&record, MAX_CONTENT_BYTES - content.len())?
        };
        content.extend_from_slice(&decoded[..decoded.len().min(MAX_CONTENT_BYTES - content.len())]);
    }
    if text_length > 0 {
        content.truncate(content.len().min(text_length));
    }
    let encoding = match codepage {
        65001 => UTF_8,
        936 => GBK,
        932 => SHIFT_JIS,
        950 => BIG5,
        949 => EUC_KR,
        _ => WINDOWS_1252,
    };
    let decoded = encoding.decode(&content).0;
    Some(strip_markup(&decoded))
}

fn decompress_palmdoc(input: &[u8], limit: usize) -> Option<Vec<u8>> {
    let mut output = Vec::new();
    let mut cursor = 0;
    while cursor < input.len() && output.len() < limit {
        let byte = input[cursor];
        cursor += 1;
        match byte {
            0x00 => output.push(byte),
            0x01..=0x08 => {
                let end = cursor.checked_add(byte as usize)?;
                output.extend_from_slice(input.get(cursor..end)?);
                cursor = end;
            }
            0x09..=0x7f => output.push(byte),
            0x80..=0xbf => {
                let next = *input.get(cursor)?;
                cursor += 1;
                let distance = (((byte & 0x3f) as usize) << 5) | ((next >> 3) as usize);
                let length = (next as usize & 0x07) + 3;
                if distance == 0 {
                    return None;
                }
                for _ in 0..length.min(limit - output.len()) {
                    let source = output.len().checked_sub(distance)?;
                    output.push(output[source]);
                }
            }
            0xc0..=0xff => {
                output.push(b' ');
                output.push(byte ^ 0x80);
            }
        }
    }
    Some(output)
}

// ---------------------------------------------------------------- EPUB

fn epub_meta(path: &Path, cancel: Option<&AtomicBool>) -> Option<BookMeta> {
    let file = std::fs::File::open(path).ok()?;
    let mut zip = zip::ZipArchive::new(file).ok()?;
    let container = read_zip_text(&mut zip, "META-INF/container.xml", cancel)?;
    let opf_path = find_attr(&container, "full-path")?;
    let opf = read_zip_text(&mut zip, &opf_path, cancel)?;
    let mut meta = parse_opf(&opf);
    // 版权页/扉页文本启发式补齐 OPF 缺失字段（译者、版本、ISBN、年份等）。
    let text = epub_body_text(&mut zip, cancel);
    fill_from_text(&mut meta, &text);
    Some(meta)
}

fn read_zip_text<R: Read + Seek>(
    zip: &mut zip::ZipArchive<R>,
    name: &str,
    cancel: Option<&AtomicBool>,
) -> Option<String> {
    if is_cancelled(cancel) {
        return None;
    }
    let mut entry = zip.by_name(name).ok()?;
    let mut buf = Vec::new();
    entry.read_to_end(&mut buf).ok()?;
    if is_cancelled(cancel) {
        return None;
    }
    Some(String::from_utf8_lossy(&buf).into_owned())
}

/// 取首个 `key="value"` 属性值。
fn find_attr(text: &str, key: &str) -> Option<String> {
    let pat = format!("{key}=\"");
    let i = text.find(&pat)? + pat.len();
    let rest = &text[i..];
    let j = rest.find('"')?;
    Some(rest[..j].to_string())
}

/// OPF Dublin Core：dc:title / dc:creator / dc:publisher / dc:date / dc:contributor。
fn parse_opf(opf: &str) -> BookMeta {
    let mut meta = BookMeta::default();
    let mut reader = quick_xml::Reader::from_str(opf);
    reader.config_mut().trim_text(true);
    // (当前字段, contributor 是否 role="trl")
    let mut field: Option<(&'static str, bool)> = None;
    loop {
        match reader.read_event() {
            Ok(XmlEvent::Start(e)) => {
                let tag = e.name().local_name().as_ref().to_vec();
                let role_trl = e
                    .attributes()
                    .flatten()
                    .any(|a| a.key.as_ref() == b"role"
                        && a.unescape_value().map(|v| v == "trl").unwrap_or(false));
                field = match tag.as_slice() {
                    b"title" => Some(("title", false)),
                    b"creator" => Some(("author", false)),
                    b"publisher" => Some(("publisher", false)),
                    b"date" => Some(("date", false)),
                    b"contributor" => Some(("contributor", role_trl)),
                    _ => None,
                };
            }
            Ok(XmlEvent::Empty(_)) => field = None,
            Ok(XmlEvent::Text(t)) => {
                if let Some((key, trl)) = field {
                    if let Some(s) = t.unescape().ok().map(|v| v.trim().to_string()).filter(|v| !v.is_empty()) {
                        match key {
                            "title" => { meta.title.get_or_insert(s); }
                            "author" => { meta.author.get_or_insert(s); }
                            "publisher" => { meta.publisher.get_or_insert(s); }
                            "date" => meta.year = meta.year.or_else(|| find_year(&s)),
                            _ => {
                                if trl || s.contains('译') {
                                    meta.translator.get_or_insert(s);
                                }
                            }
                        }
                    }
                }
            }
            Ok(XmlEvent::Eof) | Err(_) => break,
            _ => {}
        }
    }
    meta
}

/// 收集前若干部 XHTML/HTML 的纯文本（限 200KB），供版权页启发式使用。
fn epub_body_text<R: Read + Seek>(zip: &mut zip::ZipArchive<R>, cancel: Option<&AtomicBool>) -> String {
    let mut out = String::new();
    let mut budget = 200_000usize;
    for i in 0..zip.len() {
        if budget == 0 || is_cancelled(cancel) {
            break;
        }
        let Ok(mut entry) = zip.by_index(i) else { continue };
        let name = entry.name().to_ascii_lowercase();
        if !(name.ends_with(".xhtml") || name.ends_with(".html") || name.ends_with(".htm")) {
            continue;
        }
        let take = budget.min(65_536);
        let mut tmp = vec![0u8; take];
        let n = entry.read(&mut tmp).unwrap_or(0);
        budget -= n;
        let raw = String::from_utf8_lossy(&tmp[..n]).replace("&nbsp;", " ");
        let mut in_tag = false;
        for c in raw.chars() {
            match c {
                '<' => in_tag = true,
                '>' => in_tag = false,
                _ if in_tag => {}
                c => out.push(c),
            }
        }
        out.push('\n');
    }
    out
}

// ---------------------------------------------------------------- FB2

fn fb2_meta(path: &Path, cancel: Option<&AtomicBool>) -> Option<BookMeta> {
    let raw = read_all_cancellable(path, cancel)?;
    let text = String::from_utf8_lossy(&raw);
    let mut meta = BookMeta::default();
    let mut reader = quick_xml::Reader::from_str(&text);
    reader.config_mut().trim_text(true);
    let mut stack: Vec<Vec<u8>> = Vec::new();
    let mut author_parts: Vec<String> = Vec::new();
    let mut translator_parts: Vec<String> = Vec::new();
    loop {
        if is_cancelled(cancel) {
            return None;
        }
        match reader.read_event() {
            Ok(XmlEvent::Start(e)) => {
                stack.push(e.name().local_name().as_ref().to_vec());
            }
            Ok(XmlEvent::End(e)) => {
                let tag = e.name().local_name().as_ref().to_vec();
                if tag == b"author" {
                    join_parts(&mut author_parts, &mut meta.author);
                } else if tag == b"translator" {
                    join_parts(&mut translator_parts, &mut meta.translator);
                }
                stack.pop();
            }
            Ok(XmlEvent::Text(t)) => {
                let Some(top) = stack.last().map(|v| v.as_slice()) else { continue };
                let Some(s) = t.unescape().ok().map(|v| v.trim().to_string()).filter(|v| !v.is_empty())
                else { continue };
                match top {
                    b"book-title" => { meta.title.get_or_insert(s); }
                    b"first-name" | b"middle-name" | b"last-name" => {
                        if contains(&stack, b"translator") {
                            translator_parts.push(s);
                        } else if contains(&stack, b"author") {
                            author_parts.push(s);
                        }
                    }
                    b"publisher" => { meta.publisher.get_or_insert(s); }
                    b"pubdate" => meta.year = meta.year.or_else(|| find_year(&s)),
                    b"isbn" => { meta.isbn.get_or_insert(s); }
                    b"sequence" => {
                        // FB2 的 <sequence name="…"> 携带丛书/版本名，暂不抽取。
                    }
                    _ => {}
                }
            }
            Ok(XmlEvent::Eof) | Err(_) => break,
            _ => {}
        }
    }
    join_parts(&mut author_parts, &mut meta.author);
    join_parts(&mut translator_parts, &mut meta.translator);
    Some(meta)
}

fn contains(stack: &[Vec<u8>], tag: &[u8]) -> bool {
    stack.iter().any(|t| t.as_slice() == tag)
}

fn join_parts(parts: &mut Vec<String>, slot: &mut Option<String>) {
    if parts.is_empty() {
        return;
    }
    let joined = parts.drain(..).collect::<Vec<_>>().join(" ");
    if !joined.trim().is_empty() {
        slot.get_or_insert(joined);
    }
}

// ---------------------------------------------------------------- MOBI / AZW / AZW3

fn mobi_meta(path: &Path, cancel: Option<&AtomicBool>) -> Option<BookMeta> {
    if is_cancelled(cancel) {
        return None;
    }
    let mut file = std::fs::File::open(path).ok()?;
    let head = read_prefix(&mut file, 128 * 1024)?;
    // 书名兜底：PalmDB 头 name 字段（0..32，NUL 结尾）。
    let db_name = trim_nul(&head[..std::cmp::min(32, head.len())]);
    // MOBI 魔数位于 record0 数据 + 16；record0 起始偏移随 num_records 变化，直接扫描。
    let m = find_sub(&head, b"MOBI", 68)?;
    let record0 = m - 16;
    let encryption = u16_be(&head, record0 + 12)?;
    if encryption != 0 {
        return None; // 加密文件无法解析
    }
    let header_len = u32_be(&head, m + 4)? as usize;
    let exth = m.checked_add(header_len)?;
    if exth + 12 > head.len() || &head[exth..exth + 4] != b"EXTH" {
        return None;
    }
    let exth_len = u32_be(&head, exth + 4)? as usize;
    let count = u32_be(&head, exth + 8)? as usize;
    let exth_bytes: Vec<u8> = if exth + exth_len <= head.len() {
        head[exth..exth + exth_len].to_vec()
    } else {
        let mut extra = vec![0u8; exth_len.min(1 << 20)];
        file.seek(SeekFrom::Start(exth as u64)).ok()?;
        let n = file.read(&mut extra).ok()?;
        extra.truncate(n);
        extra
    };
    let mut meta = BookMeta::default();
    let mut pos = 12;
    for _ in 0..count {
        if is_cancelled(cancel) {
            return None;
        }
        if pos + 8 > exth_bytes.len() {
            break;
        }
        let typ = u32_be(&exth_bytes, pos)?;
        let len = u32_be(&exth_bytes, pos + 4)? as usize;
        if len < 8 {
            break;
        }
        let payload = &exth_bytes[pos + 8..std::cmp::min(pos + len, exth_bytes.len())];
        let s = String::from_utf8_lossy(payload).trim().to_string();
        if !s.is_empty() {
            match typ {
                100 => { meta.author.get_or_insert(s); } // AUTHOR
                101 => { meta.publisher.get_or_insert(s); } // PUBLISHER
                104 => { meta.isbn.get_or_insert(s); } // ISBN
                106 => meta.year = meta.year.or_else(|| find_year(&s)), // PUBLISHED
                503 => { meta.title.get_or_insert(s); } // UPDATED TITLE
                _ => {}
            }
        }
        pos += len;
    }
    if meta.title.is_none() && !db_name.is_empty() {
        meta.title = Some(db_name);
    }
    Some(meta)
}

fn read_prefix(file: &mut std::fs::File, max: usize) -> Option<Vec<u8>> {
    let mut buf = vec![0u8; max];
    let mut total = 0;
    while total < max {
        let n = file.read(&mut buf[total..]).ok()?;
        if n == 0 {
            break;
        }
        total += n;
    }
    buf.truncate(total);
    Some(buf)
}

fn find_sub(hay: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if hay.len() < from + needle.len() {
        return None;
    }
    (from..=hay.len() - needle.len()).find(|i| &hay[*i..*i + needle.len()] == needle)
}

fn u16_be(b: &[u8], off: usize) -> Option<u16> {
    (off + 2 <= b.len()).then(|| u16::from_be_bytes([b[off], b[off + 1]]))
}

fn u32_be(b: &[u8], off: usize) -> Option<u32> {
    (off + 4 <= b.len())
        .then(|| u32::from_be_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]]))
}

fn trim_nul(bytes: &[u8]) -> String {
    let end = bytes.iter().position(|b| *b == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).trim().to_string()
}

// ---------------------------------------------------------------- PDF

fn pdf_meta(path: &Path, cancel: Option<&AtomicBool>) -> Option<BookMeta> {
    if is_cancelled(cancel) {
        return None;
    }
    let text = crate::pdf_text::extract_cached(path, u64::MAX).ok()?;
    if is_cancelled(cancel) {
        return None;
    }
    if text.trim().is_empty() {
        return None; // 扫描版无文本层
    }
    let prefix: String = text.chars().take(30_000).collect();
    let mut meta = BookMeta::default();
    fill_from_text(&mut meta, &prefix);
    Some(meta)
}

// ---------------------------------------------------------------- 文本启发式

/// 用正文/版权页文本补齐缺失字段。
fn fill_from_text(meta: &mut BookMeta, text: &str) {
    if meta.translator.is_none() {
        meta.translator = find_translator(text);
    }
    if meta.edition.is_none() {
        meta.edition = find_edition(text);
    }
    if meta.isbn.is_none() {
        meta.isbn = find_isbn(text);
    }
    if meta.year.is_none() {
        meta.year = find_year(text);
    }
    if meta.author.is_none() {
        meta.author = find_author(text);
    }
}

/// 1500..=2100 的四位年份；紧跟"年"的命中优先返回。
fn find_year(text: &str) -> Option<u32> {
    let b = text.as_bytes();
    let digit = |j: usize| b[j].is_ascii_digit();
    let mut first = None;
    let mut i = 0;
    while i + 4 <= b.len() {
        if b[i..i + 4].iter().all(u8::is_ascii_digit)
            && (i == 0 || !digit(i - 1))
            && (i + 4 == b.len() || !digit(i + 4))
        {
            if let Ok(v) = text[i..i + 4].parse::<u32>() {
                if (1500..=2100).contains(&v) {
                    if text[i + 4..].starts_with('年') {
                        return Some(v);
                    }
                    if first.is_none() {
                        first = Some(v);
                    }
                }
            }
        }
        i += 1;
    }
    first
}

/// "ISBN 978-…-X"：定位关键词后收集数字/X/连字符。
fn find_isbn(text: &str) -> Option<String> {
    let lower = text.to_lowercase();
    let mut from = 0;
    while let Some(rel) = lower[from..].find("isbn") {
        let start = from + rel + 4;
        let mut out = String::new();
        let mut digits = 0;
        for c in text[start..].chars().take(40) {
            if c.is_ascii_digit() {
                out.push(c);
                digits += 1;
            } else if c == 'X' || c == 'x' {
                out.push('X');
                digits += 1;
            } else if (c == '-' || c == ' ') && !out.is_empty() {
                out.push('-');
            } else if out.is_empty() {
                continue; // 跳过"：""："等前导符号
            } else {
                break;
            }
            if digits >= 13 {
                break;
            }
        }
        let isbn = out.trim_end_matches('-').to_string();
        if digits >= 10 {
            return Some(isbn);
        }
        from = start;
    }
    None
}

/// 译者："译者/翻译 + 名字" 或 "名字 +（空格）+ 译"。
fn find_translator(text: &str) -> Option<String> {
    if let Some(i) = text.find("译者") {
        let rest = &text[i + "译者".len()..];
        let rest = rest.trim_start_matches(['：', ':', ' ', '\t', '，', '、', '\n']);
        if let Some(name) = take_name(rest) {
            return Some(name);
        }
    }
    if let Some(i) = text.find("翻译") {
        // "由张三翻译"：名字在关键词之前，向前取窗口并裁掉引导虚词。
        let before: Vec<char> = text[..i].chars().collect();
        if let Some(name) = trailing_name(&before, &['翻']) {
            return Some(name);
        }
        let rest = &text[i + "翻译".len()..];
        let rest = rest.trim_start_matches(['：', ':', ' ', '\t', '，', '、', '\n']);
        if let Some(name) = take_name(rest) {
            return Some(name);
        }
    }
    let chars: Vec<char> = text.chars().collect();
    name_before(&chars, '译', &['翻', '编', '导', '转', '意'])
}

/// 作者："名字 +（空格）+ 著"。
fn find_author(text: &str) -> Option<String> {
    let chars: Vec<char> = text.chars().collect();
    name_before(&chars, '著', &['译', '编', '原'])
}

/// 扫描 `marker` 字符，取其前面紧邻的名字窗口（允许间隔一个空格）。
fn name_before(chars: &[char], marker: char, exclude_prev: &[char]) -> Option<String> {
    for k in 1..chars.len() {
        if chars[k] != marker {
            continue;
        }
        let mut end = k;
        if chars[k - 1].is_whitespace() {
            if k < 2 {
                continue;
            }
            end = k - 1;
        }
        let prev = chars[end - 1];
        if !is_name_char(prev) || exclude_prev.contains(&prev) {
            continue;
        }
        let mut start = end - 1;
        while start > 0 && is_name_char(chars[start - 1]) {
            start -= 1;
        }
        let name: String = chars[start..end].iter().collect();
        let len = name.chars().count();
        if (2..=20).contains(&len) {
            return Some(name);
        }
    }
    None
}

/// 取字符序列末尾的名字：反向收集名字字符，再裁掉常见引导虚词。
fn trailing_name(chars: &[char], stop: &[char]) -> Option<String> {
    let mut end = chars.len();
    while end > 0 && (chars[end - 1].is_whitespace() || stop.contains(&chars[end - 1])) {
        end -= 1;
    }
    let mut start = end;
    while start > 0 && is_name_char(chars[start - 1]) {
        start -= 1;
    }
    while start < end && "由是为请本书特约等的".contains(chars[start]) {
        start += 1;
    }
    let name: String = chars[start..end].iter().collect();
    let len = name.chars().count();
    (2..=20).contains(&len).then_some(name)
}

/// 版本："第X版（本）"或常见版本关键词。
fn find_edition(text: &str) -> Option<String> {
    let chars: Vec<char> = text.chars().collect();
    for i in 0..chars.len() {
        if chars[i] != '第' {
            continue;
        }
        let mut j = i + 1;
        let mut body = String::new();
        while j < chars.len() && j <= i + 8 && "0123456789一二三四五六七八九十百".contains(chars[j]) {
            body.push(chars[j]);
            j += 1;
        }
        if !body.is_empty() && j < chars.len() && chars[j] == '版' {
            let end = if chars.get(j + 1) == Some(&'本') { j + 2 } else { j + 1 };
            return Some(chars[i..end].iter().collect());
        }
    }
    for kw in ["修订版", "增订版", "珍藏版", "纪念版", "插图版", "全译本", "注疏版", "评注版"] {
        if text.contains(kw) {
            return Some(kw.to_string());
        }
    }
    None
}

fn is_name_char(c: char) -> bool {
    c.is_alphabetic() || c == '·'
}

/// 取到名字串：直到空白/中文标点为止，长度 2..=24。
fn take_name(s: &str) -> Option<String> {
    let mut out = String::new();
    for c in s.chars() {
        if c.is_whitespace() || matches!(c, '，' | '。' | '；' | '、' | '）' | '】' | '\n') {
            break;
        }
        out.push(c);
        if out.chars().count() > 24 {
            break;
        }
    }
    let len = out.chars().count();
    (2..=24).contains(&len).then_some(out)
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::path::PathBuf;

    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!("sepiola-book-{tag}-{nanos}"));
        std::fs::create_dir_all(&dir).expect("mkdir");
        dir
    }

    #[test]
    fn heuristics_from_copyright_text() {
        let text = "《三体》 刘慈欣 著 周华 译 修订版 重庆出版社 2008年 ISBN 978-7-5366-9293-0";
        let mut meta = BookMeta::default();
        fill_from_text(&mut meta, text);
        assert_eq!(meta.author.as_deref(), Some("刘慈欣"));
        assert_eq!(meta.translator.as_deref(), Some("周华"));
        assert_eq!(meta.edition.as_deref(), Some("修订版"));
        assert_eq!(meta.year, Some(2008));
        assert_eq!(meta.isbn.as_deref(), Some("978-7-5366-9293-0"));
    }

    #[test]
    fn heuristics_second_edition_and_no_false_author() {
        let text = "本书由张三翻译，李四 译，2021 年出版。";
        let mut meta = BookMeta::default();
        fill_from_text(&mut meta, text);
        assert_eq!(meta.translator.as_deref(), Some("张三")); // "翻译"关键词优先
        assert_eq!(meta.year, Some(2021));
        assert!(meta.author.is_none());
    }

    #[test]
    fn year_detection_handles_adjacent_chinese_text() {
        assert_eq!(find_year("1A中文 2020年"), Some(2020));
    }

    #[test]
    fn epub_opf_and_copyright_page() {
        let dir = temp_dir("epub");
        let path = dir.join("book.epub");
        let file = std::fs::File::create(&path).expect("create");
        let mut w = zip::ZipWriter::new(file);
        let opts = zip::write::SimpleFileOptions::default();
        let add = |w: &mut zip::ZipWriter<std::fs::File>, name: &str, data: &str| {
            w.start_file(name, opts).unwrap();
            w.write_all(data.as_bytes()).unwrap();
        };
        add(&mut w, "mimetype", "application/epub+zip");
        add(
            &mut w,
            "META-INF/container.xml",
            r#"<?xml version="1.0"?><container><rootfiles>
<rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/>
</rootfiles></container>"#,
        );
        add(
            &mut w,
            "OEBPS/content.opf",
            r#"<?xml version="1.0"?><package xmlns:dc="http://purl.org/dc/elements/1.1/">
<metadata>
<dc:title>活着</dc:title>
<dc:creator>余华</dc:creator>
<dc:publisher>作家出版社</dc:publisher>
<dc:date>2012-07-01</dc:date>
</metadata></package>"#,
        );
        add(
            &mut w,
            "OEBPS/copyright.xhtml",
            "<html><body><p>余华 著 第三版 北京出版社 2012年 ISBN 978-7-5063-6082-6</p></body></html>",
        );
        w.finish().expect("zip finish");

        let meta = extract(&path).expect("epub meta");
        assert_eq!(meta.title.as_deref(), Some("活着"));
        assert_eq!(meta.author.as_deref(), Some("余华"));
        assert_eq!(meta.publisher.as_deref(), Some("作家出版社"));
        assert_eq!(meta.year, Some(2012));
        assert_eq!(meta.edition.as_deref(), Some("第三版"));
        assert_eq!(meta.isbn.as_deref(), Some("978-7-5063-6082-6"));
        assert!(contains_text(&path, "第三版"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn fb2_basic_fields() {
        let dir = temp_dir("fb2");
        let path = dir.join("book.fb2");
        std::fs::write(
            &path,
            r#"<?xml version="1.0" encoding="UTF-8"?>
<FictionBook xmlns="http://www.fictionbook.org/FictionBook-2.0">
<description><title-info>
<book-title>白夜行</book-title>
<author><first-name>东野</first-name><last-name>圭吾</last-name></author>
<translator><first-name>刘</first-name><last-name>姿函</last-name></translator>
<publisher>南海出版公司</publisher>
<pubdate>2013-01</pubdate>
</title-info></description>
</FictionBook>"#,
        )
        .expect("write");
        let meta = extract(&path).expect("fb2 meta");
        assert_eq!(meta.title.as_deref(), Some("白夜行"));
        assert_eq!(meta.author.as_deref(), Some("东野 圭吾"));
        assert_eq!(meta.translator.as_deref(), Some("刘 姿函"));
        assert_eq!(meta.publisher.as_deref(), Some("南海出版公司"));
        assert_eq!(meta.year, Some(2013));
        assert!(contains_text(&path, "刘姿函"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn mobi_exth_records() {
        let dir = temp_dir("mobi");
        let path = dir.join("book.mobi");
        // 手工构造 record0：MOBI 魔数在 record0 数据 + 16，EXTH 紧随其后。
        let mut data = vec![0u8; 68];
        data[..12].copy_from_slice("测试书名".as_bytes());
        let record0 = 68usize;
        let m = record0 + 16;
        data.resize(m + 4, 0);
        data[m..m + 4].copy_from_slice(b"MOBI");
        let put = |data: &mut Vec<u8>, off: usize, v: u32| {
            let b = v.to_be_bytes();
            let start = m + off;
            data[start..start + 4].copy_from_slice(&b);
        };
        let header_len = 232usize;
        let exth = m + header_len;
        data.resize(exth + 12, 0);
        put(&mut data, 4, header_len as u32);
        put(&mut data, 12, 65001); // text encoding
        data[record0 + 12..record0 + 14].copy_from_slice(&0u16.to_be_bytes()); // 未加密
        // EXTH
        data[exth..exth + 4].copy_from_slice(b"EXTH");
        let records: Vec<(u32, &[u8])> = vec![
            (100, "东野圭吾".as_bytes()),
            (101, "南海出版公司".as_bytes()),
            (102, "南海出版社".as_bytes()), // IMPRINT
            (104, b"9787544253994"),
            (106, b"2013-01-01"),
            (503, "白夜行".as_bytes()),
        ];
        let body_len: usize = records.iter().map(|(_, p)| 8 + p.len()).sum();
        let total = 12 + body_len;
        data[exth + 4..exth + 8].copy_from_slice(&(total as u32).to_be_bytes());
        data[exth + 8..exth + 12].copy_from_slice(&(records.len() as u32).to_be_bytes());
        for (typ, payload) in &records {
            data.extend_from_slice(&typ.to_be_bytes());
            data.extend_from_slice(&((8 + payload.len()) as u32).to_be_bytes());
            data.extend_from_slice(payload);
        }
        std::fs::write(&path, &data).expect("write mobi");

        let meta = extract(&path).expect("mobi meta");
        assert_eq!(meta.title.as_deref(), Some("白夜行"));
        assert_eq!(meta.author.as_deref(), Some("东野圭吾"));
        assert_eq!(meta.publisher.as_deref(), Some("南海出版公司"));
        assert_eq!(meta.year, Some(2013));
        assert_eq!(meta.isbn.as_deref(), Some("9787544253994"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn pdf_text_layer_heuristics() {
        let dir = temp_dir("pdf");
        let path = dir.join("book.pdf");
        std::fs::write(&path, build_test_pdf(&["Copyright 2015", "ISBN 978-0-00-000000-1"]))
            .expect("write");
        let meta = extract(&path).expect("pdf meta");
        assert_eq!(meta.year, Some(2015));
        assert_eq!(meta.isbn.as_deref(), Some("978-0-00-000000-1"));
        assert!(contains_text(&path, "copyright"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn mobi_palm_doc_content() {
        let dir = temp_dir("mobi-content");
        let path = dir.join("book.mobi");
        let text = [4, 0xd6, 0xd0, 0xb9, 0xfa];
        let mut record0 = vec![0u8; 32];
        record0[0..2].copy_from_slice(&2u16.to_be_bytes());
        record0[4..8].copy_from_slice(&4u32.to_be_bytes());
        record0[8..10].copy_from_slice(&1u16.to_be_bytes());
        record0[12..14].copy_from_slice(&0u16.to_be_bytes());
        record0[16..20].copy_from_slice(b"MOBI");
        record0[28..32].copy_from_slice(&936u32.to_be_bytes());

        let first_record = 78 + 2 * 8;
        let second_record = first_record + record0.len();
        let mut file = vec![0u8; first_record];
        file[76..78].copy_from_slice(&2u16.to_be_bytes());
        file[78..82].copy_from_slice(&(first_record as u32).to_be_bytes());
        file[86..90].copy_from_slice(&(second_record as u32).to_be_bytes());
        file.extend_from_slice(&record0);
        file.extend_from_slice(&text);
        std::fs::write(&path, file).expect("write mobi");

        assert!(contains_text(&path, "中国"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn palm_doc_literal_and_back_reference() {
        let decoded = decompress_palmdoc(&[5, b'h', b'e', b'l', b'l', b'o', 0x80, 0x2a], 64)
            .expect("decode");
        assert_eq!(decoded, b"hellohello");
    }

    #[test]
    fn markup_text_preserves_tag_boundaries_and_decodes_entities() {
        let text = strip_markup("<p>hello</p><p>world &amp; friends</p>");
        assert!(text.contains("hello world & friends"));
    }

    #[test]
    fn unsupported_or_broken_returns_none() {
        let dir = temp_dir("none");
        assert!(extract(&dir.join("a.djvu")).is_none());
        std::fs::write(dir.join("broken.epub"), b"not a zip").expect("write");
        assert!(extract(&dir.join("broken.epub")).is_none());
        std::fs::remove_dir_all(&dir).ok();
    }
}
