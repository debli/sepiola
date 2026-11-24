//! PDF 阅读：优先系统 `mutool` 提取文本层（回退 `pdf-extract`）。
//!
//! 界面采用页面渲染模式：每章对应 PDF 的一页，正文文本层保留（供 AI 分析），
//! 显示则由 GUI 通过 `render_page_png` 按页渲染成位图，保留原始版面
//! （扫描书、图表、公式都能正常显示）。文本无法对齐页数时退回段落分块。

use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

use super::{BookDocument, Chapter};

const MAX_BYTES: u64 = 256 * 1024 * 1024;
/// 无分页符时的分块长度（字符数），控制单页排版耗时。
const CHUNK_CHARS: usize = 4000;
/// 页面渲染的默认分辨率；GUI 首帧后会按屏幕像素自适应调整。
pub const DEFAULT_DPI: u32 = 150;

pub fn parse(path: &Path) -> Result<BookDocument, String> {
    let text = crate::pdf_text::extract_cached(path, MAX_BYTES)?;
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    let title = path
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned());
    // 页数对得上的章节数才能保证「第 N 章 ↔ 第 N 页」的映射，页面渲染依赖它；
    // mutool 不可用或读不到页数时退回段落分块。
    let chapters = match page_count(path) {
        Ok(pages) if pages > 0 => page_chapters(&normalized, pages),
        _ => chunk(&normalized),
    };
    if chapters.is_empty() {
        return Err("未解析到正文（可能是加密书籍或损坏文件）".to_string());
    }
    Ok(BookDocument {
        title,
        author: None,
        is_pdf: true,
        chapters,
    })
}

/// 每章对应一页；页文本取自文本层对应分段（扫描页为空，界面显示渲染图）。
fn page_chapters(text: &str, pages: usize) -> Vec<Chapter> {
    let mut parts: Vec<&str> = text.split('\u{c}').collect();
    // mutool 在最后一页后也输出分页符，末尾会多出一个空段。
    if parts.len() == pages + 1 && parts.last().is_some_and(|part| part.trim().is_empty()) {
        parts.pop();
    }
    (0..pages)
        .map(|index| Chapter {
            title: format!("第 {} 页", index + 1),
            text: parts
                .get(index)
                .map(|part| part.trim())
                .unwrap_or("")
                .to_string(),
            spans: Vec::new(),
            links: Vec::new(),
            images: Vec::new(),
        })
        .collect()
}

/// 读取 PDF 的正文页面尺寸（点），用于首帧排版和渲染分辨率计算。
///
/// 不能直接用首页或 `mutool info` 的 MediaBox：
/// - 封面、插图页的尺寸常与正文不同；
/// - 很多书设置了 CropBox（实际显示区域），比 MediaBox 小。
///
/// 这里用 `mutool pages` 取每页的 CropBox（缺失时用 MediaBox），按页数最多的
/// 尺寸作为整本书的代表尺寸，并处理 `/Rotate` 带来的宽高互换。
pub fn page_size(path: &Path) -> Option<(f32, f32)> {
    let output = std::process::Command::new("mutool")
        .arg("pages")
        .arg(path)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut counts: HashMap<(i64, i64), (usize, f32, f32)> = HashMap::new();
    let mut media = None;
    let mut crop = None;
    let mut rotate = 0;
    for line in stdout.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("<MediaBox") {
            media = parse_box(rest);
        } else if let Some(rest) = line.strip_prefix("<CropBox") {
            crop = parse_box(rest);
        } else if let Some(rest) = line.strip_prefix("<Rotate") {
            rotate = parse_rotate(rest).unwrap_or(0);
        } else if line.starts_with("</page>") {
            let mut size = crop.or(media);
            if rotate.rem_euclid(180) == 90 {
                size = size.map(|(width, height)| (height, width));
            }
            if let Some((width, height)) = size {
                let key = (width.round() as i64, height.round() as i64);
                let entry = counts.entry(key).or_insert((0, width, height));
                entry.0 += 1;
            }
            media = None;
            crop = None;
            rotate = 0;
        }
    }
    counts
        .into_iter()
        .max_by_key(|(_, (pages, _, _))| *pages)
        .map(|(_, (_, width, height))| (width, height))
}

/// 解析 `<MediaBox l="0" b="0" r="612" t="792" />` 这类行，返回宽高。
fn parse_box(text: &str) -> Option<(f32, f32)> {
    let (mut l, mut b, mut r, mut t) = (None, None, None, None);
    for part in text.split_whitespace() {
        let Some((key, value)) = part.split_once('=') else {
            continue;
        };
        let parsed: f32 = value.trim_matches('"').parse().ok()?;
        match key {
            "l" => l = Some(parsed),
            "b" => b = Some(parsed),
            "r" => r = Some(parsed),
            "t" => t = Some(parsed),
            _ => {}
        }
    }
    let width = (r? - l?).abs();
    let height = (t? - b?).abs();
    (width > 0.0 && height > 0.0).then_some((width, height))
}

/// 解析 `<Rotate v="90" />`，返回归一化到 0/90/180/270 的角度。
fn parse_rotate(text: &str) -> Option<i32> {
    let value = text.split("v=").nth(1)?;
    let angle: i32 = value
        .split(|c: char| !c.is_ascii_digit() && c != '-')
        .next()?
        .parse()
        .ok()?;
    Some(angle.rem_euclid(360))
}

/// 用 `mutool info` 读取 PDF 总页数。
pub fn page_count(path: &Path) -> Result<usize, String> {
    let output = std::process::Command::new("mutool")
        .arg("info")
        .arg(path)
        .output()
        .map_err(|error| format!("mutool 不可用：{error}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let reason = stderr.lines().next().unwrap_or("未知错误");
        return Err(format!("mutool 读取页数失败：{reason}"));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        if let Some(rest) = line.trim().strip_prefix("Pages:") {
            if let Ok(pages) = rest.trim().parse() {
                return Ok(pages);
            }
        }
    }
    Err("mutool 输出中没有页数".to_string())
}

/// 渲染一页为 PNG（1 起始页码），优先读取落盘缓存。
/// `cache_dir` 通常为 `<库根>/.sepiola/pdf_render`，按文件身份（路径+大小+修改时间）
/// 哈希分目录，同一本书换 dpi 会各自缓存一份。
pub fn render_page_png(
    path: &Path,
    page: usize,
    dpi: u32,
    cache_dir: &Path,
) -> Result<Vec<u8>, String> {
    let cached = cache_file(path, page, dpi, cache_dir);
    if let Ok(bytes) = std::fs::read(&cached) {
        return Ok(bytes);
    }
    // 临时文件保留 .png 扩展名，mutool 靠扩展名判断输出格式。
    let tmp = cached.with_extension(format!("tmp{}.png", std::process::id()));
    if let Some(parent) = cached.parent() {
        std::fs::create_dir_all(parent).map_err(|error| format!("创建缓存目录失败：{error}"))?;
    }
    let output = std::process::Command::new("mutool")
        .args(["draw", "-q", "-r", &dpi.to_string(), "-o"])
        .arg(&tmp)
        .arg(path)
        .arg(page.to_string())
        .output()
        .map_err(|error| format!("mutool 不可用：{error}"))?;
    // mutool 会对一些可忽略的问题（如 ICC 缺失）在 stderr 告警并返回非零，
    // 只要渲染出了非空的图片文件就视为成功。
    let rendered = std::fs::metadata(&tmp).map(|meta| meta.len() > 0).unwrap_or(false);
    if !rendered {
        std::fs::remove_file(&tmp).ok();
        let stderr = String::from_utf8_lossy(&output.stderr);
        let reason = stderr
            .lines()
            .find(|line| !line.starts_with("warning:"))
            .unwrap_or("未知错误");
        return Err(format!("渲染第 {page} 页失败：{reason}"));
    }
    std::fs::rename(&tmp, &cached).map_err(|error| format!("写入渲染缓存失败：{error}"))?;
    std::fs::read(&cached).map_err(|error| format!("读取渲染缓存失败：{error}"))
}

/// 渲染缓存的落盘路径：`<cache_dir>/<文件身份哈希>/p<页>-r<dpi>.png`。
fn cache_file(path: &Path, page: usize, dpi: u32, cache_dir: &Path) -> PathBuf {
    let mut hasher = DefaultHasher::new();
    if let Ok(canonical) = path.canonicalize() {
        canonical.hash(&mut hasher);
    } else {
        path.hash(&mut hasher);
    }
    if let Ok(metadata) = std::fs::metadata(path) {
        metadata.len().hash(&mut hasher);
        if let Ok(modified) = metadata.modified() {
            if let Ok(duration) = modified.duration_since(std::time::UNIX_EPOCH) {
                duration.as_nanos().hash(&mut hasher);
            }
        }
    }
    cache_dir
        .join(format!("{:016x}", hasher.finish()))
        .join(format!("p{page}-r{dpi}.png"))
}

/// 一页的文字层布局：坐标系为页面点（1/72 英寸），原点左上、y 向下。
/// 用于在页面位图之上叠加透明文字层，做到「保留版式 + 文字可选中」。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PdfTextPage {
    pub width: f32,
    pub height: f32,
    pub lines: Vec<PdfTextLine>,
}

/// 同一基线上的一行文字。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PdfTextLine {
    /// 行包围盒 (x0, y0, x1, y1)，页面点。
    pub rect: (f32, f32, f32, f32),
    pub chars: Vec<PdfChar>,
}

/// 单个字符及其版面矩形。
#[derive(Clone, Debug, PartialEq)]
pub struct PdfChar {
    pub c: char,
    pub rect: (f32, f32, f32, f32),
}

/// 用 `mutool draw -F stext` 取一页的字符级版面。
pub fn page_text_layout(path: &Path, page: usize) -> Result<PdfTextPage, String> {
    // 临时文件名带上路径哈希：多本书的渲染线程可能同时取同一页码。
    let mut hasher = DefaultHasher::new();
    path.hash(&mut hasher);
    let tmp = std::env::temp_dir().join(format!(
        "sepiola-stext-{}-{}-{}.xml",
        std::process::id(),
        hasher.finish(),
        page
    ));
    let output = std::process::Command::new("mutool")
        .args(["draw", "-F", "stext", "-q", "-o"])
        .arg(&tmp)
        .arg(path)
        .arg(page.to_string())
        .output()
        .map_err(|error| format!("mutool 不可用：{error}"))?;
    let xml = std::fs::read_to_string(&tmp).unwrap_or_default();
    std::fs::remove_file(&tmp).ok();
    if xml.is_empty() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let reason = stderr.lines().next().unwrap_or("无输出");
        return Err(format!("读取第 {page} 页版面失败：{reason}"));
    }
    parse_stext(&xml)
}

/// 解析 stext XML：只关心 `<page>` 尺寸与 `<line>` / `<char>` 的坐标。
pub fn parse_stext(xml: &str) -> Result<PdfTextPage, String> {
    let mut page = PdfTextPage::default();
    let mut current: Option<PdfTextLine> = None;
    let mut pos = 0;
    while let Some(offset) = xml[pos..].find('<') {
        let start = pos + offset;
        let Some(close) = xml[start..].find('>') else {
            break;
        };
        let end = start + close + 1;
        let tag = &xml[start..end];
        if tag.starts_with("<page") {
            page.width = attr(tag, "width")
                .and_then(|value| value.parse().ok())
                .unwrap_or(0.0);
            page.height = attr(tag, "height")
                .and_then(|value| value.parse().ok())
                .unwrap_or(0.0);
        } else if tag.starts_with("<line") {
            if let Some(line) = current.take() {
                push_line(&mut page, line);
            }
            current = Some(PdfTextLine::default());
        } else if tag.starts_with("<char") {
            if let Some(line) = current.as_mut() {
                if let (Some(c), Some(quad)) = (attr(tag, "c"), attr(tag, "quad")) {
                    if let (Some(c), Some(rect)) = (decode_entity(c), quad_rect(quad)) {
                        line.chars.push(PdfChar { c, rect });
                    }
                }
            }
        }
        pos = end;
    }
    if let Some(line) = current.take() {
        push_line(&mut page, line);
    }
    Ok(page)
}

fn push_line(page: &mut PdfTextPage, line: PdfTextLine) {
    if line.chars.is_empty() {
        return;
    }
    let mut rect = (
        f32::INFINITY,
        f32::INFINITY,
        f32::NEG_INFINITY,
        f32::NEG_INFINITY,
    );
    for PdfChar { rect: char_rect, .. } in &line.chars {
        rect.0 = rect.0.min(char_rect.0);
        rect.1 = rect.1.min(char_rect.1);
        rect.2 = rect.2.max(char_rect.2);
        rect.3 = rect.3.max(char_rect.3);
    }
    page.lines.push(PdfTextLine { rect, chars: line.chars });
}

/// 取标签里的属性值：`<char c="x" quad="...">`。
fn attr<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
    let key = format!(" {name}=\"");
    let start = tag.find(&key)? + key.len();
    let end = start + tag[start..].find('"')?;
    Some(&tag[start..end])
}

/// stext 的 quad 是四个角点：左上、右上、左下、右下。
fn quad_rect(quad: &str) -> Option<(f32, f32, f32, f32)> {
    let mut values = quad.split_whitespace().filter_map(|v| v.parse::<f32>().ok());
    let points: Vec<f32> = values.by_ref().take(8).collect();
    if points.len() < 8 {
        return None;
    }
    let xs = [points[0], points[2], points[4], points[6]];
    let ys = [points[1], points[3], points[5], points[7]];
    let x0 = xs.iter().copied().fold(f32::INFINITY, f32::min);
    let x1 = xs.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    let y0 = ys.iter().copied().fold(f32::INFINITY, f32::min);
    let y1 = ys.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    Some((x0, y0, x1, y1))
}

/// 解析 XML 字符实体（`&#x4e09;`、`&amp;` 等）。
fn decode_entity(value: &str) -> Option<char> {
    if let Some(rest) = value
        .strip_prefix("&#x")
        .or_else(|| value.strip_prefix("&#X"))
    {
        return u32::from_str_radix(rest.trim_end_matches(';'), 16)
            .ok()
            .and_then(char::from_u32);
    }
    if let Some(rest) = value.strip_prefix("&#") {
        return rest
            .trim_end_matches(';')
            .parse::<u32>()
            .ok()
            .and_then(char::from_u32);
    }
    match value {
        "&amp;" => Some('&'),
        "&lt;" => Some('<'),
        "&gt;" => Some('>'),
        "&quot;" => Some('"'),
        "&apos;" => Some('\''),
        other => other.chars().next(),
    }
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
    fn aligns_pages_on_form_feed() {
        let chapters = page_chapters("第一页内容\n\u{c}第二页内容", 2);
        assert_eq!(chapters.len(), 2);
        assert_eq!(chapters[0].title, "第 1 页");
        assert_eq!(chapters[1].text, "第二页内容");
    }

    #[test]
    fn drops_trailing_empty_separator() {
        let chapters = page_chapters("第一页\n\u{c}第二页\u{c}", 2);
        assert_eq!(chapters.len(), 2);
        assert_eq!(chapters[1].text, "第二页");
    }

    #[test]
    fn pads_missing_page_text() {
        let chapters = page_chapters("只有一页文本", 3);
        assert_eq!(chapters.len(), 3);
        assert_eq!(chapters[0].text, "只有一页文本");
        assert_eq!(chapters[2].text, "");
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
        assert!(book.is_pdf);
        assert!(book
            .chapters
            .iter()
            .any(|chapter| chapter.text.contains("Hello book")));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn parses_stext_layout() {
        let xml = r#"<page id="page1" width="612" height="792">
<line bbox="56 62 120 74">
<font size="12"><char quad="56.8 62.486 70.8 62.486 56.8 73.378 70.8 73.378" x="56.8" y="62.486" c="&#x4e09;"/>
<char quad="70.8 62.486 84.8 62.486 70.8 73.378 84.8 73.378" x="70.8" y="62.486" c="&#x56fd;"/>
<char quad="84.8 62.486 88.8 62.486 84.8 73.378 88.8 73.378" x="84.8" y="62.486" c="&quot;"/></font>
</line>
<line bbox="56 80 120 92">
<font size="12"><char quad="56 80 60 80 56 91 60 91" x="56" y="80" c="A"/></font>
</line>
</page>"#;
        let page = parse_stext(xml).expect("parse");
        assert_eq!(page.width, 612.0);
        assert_eq!(page.height, 792.0);
        assert_eq!(page.lines.len(), 2);
        assert_eq!(page.lines[0].chars.len(), 3);
        assert_eq!(page.lines[0].chars[0].c, '三');
        assert_eq!(page.lines[0].chars[2].c, '"');
        let rect = page.lines[0].chars[0].rect;
        assert!((rect.0 - 56.8).abs() < 0.01 && (rect.3 - 73.378).abs() < 0.01);
        assert_eq!(page.lines[1].chars[0].c, 'A');
    }

    #[test]
    fn reads_first_page_size() {
        let dir = std::env::temp_dir().join(format!(
            "sepiola-pdf-size-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).expect("mkdir");
        let path = dir.join("book.pdf");
        std::fs::write(&path, crate::book::build_test_pdf(&["Size check"])).expect("write");
        // 没有 mutool 的环境跳过。
        if page_count(&path).is_err() {
            std::fs::remove_dir_all(&dir).ok();
            return;
        }
        let (width, height) = page_size(&path).expect("page size");
        assert!((width - 612.0).abs() < 1.0);
        assert!((height - 792.0).abs() < 1.0);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn extracts_page_text_layout_from_real_pdf() {
        let dir = std::env::temp_dir().join(format!(
            "sepiola-pdf-stext-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).expect("mkdir");
        let path = dir.join("book.pdf");
        std::fs::write(&path, crate::book::build_test_pdf(&["Layout check"])).expect("write");
        // 没有 mutool 的环境跳过。
        if page_count(&path).is_err() {
            std::fs::remove_dir_all(&dir).ok();
            return;
        }
        let layout = page_text_layout(&path, 1).expect("stext");
        assert!(layout.width > 0.0 && layout.height > 0.0);
        let text: String = layout
            .lines
            .iter()
            .flat_map(|line| line.chars.iter().map(|character| character.c))
            .collect();
        assert!(text.contains("Layout"), "text: {text}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn renders_page_to_png_with_disk_cache() {
        let dir = std::env::temp_dir().join(format!(
            "sepiola-pdf-render-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).expect("mkdir");
        let path = dir.join("book.pdf");
        std::fs::write(&path, crate::book::build_test_pdf(&["Render me"])).expect("write");
        // 页数读取失败说明环境里没有 mutool，渲染测试无从谈起。
        let Ok(pages) = page_count(&path) else {
            std::fs::remove_dir_all(&dir).ok();
            return;
        };
        assert!(pages >= 1);
        let cache = dir.join("cache");
        let bytes = render_page_png(&path, 1, 96, &cache).expect("render");
        assert_eq!(&bytes[..4], &[0x89, b'P', b'N', b'G']);
        // 第二次直接命中缓存，内容一致。
        let again = render_page_png(&path, 1, 96, &cache).expect("cached");
        assert_eq!(bytes, again);
        std::fs::remove_dir_all(&dir).ok();
    }
}
