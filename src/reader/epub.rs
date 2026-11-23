//! EPUB 阅读：container → OPF → manifest/spine，按书脊顺序取正文，TOC 取章节标题。

use std::collections::{BTreeMap, HashMap};
use std::io::{Read, Seek};
use std::path::Path;
use std::sync::Arc;

use quick_xml::events::{BytesStart, Event};

use super::markup::{parse_document, RawImage, StyledDocument};
use super::{chapter_number, BookDocument, Chapter, ChapterImage, LinkSpan, LinkTarget, StyleSpan};

/// 单篇 XHTML 的解压上限，避免异常压缩包耗尽内存。
const MAX_DOC_BYTES: u64 = 32 * 1024 * 1024;

/// 单张插图的解压上限。
const MAX_IMAGE_BYTES: u64 = 24 * 1024 * 1024;

struct ManifestItem {
    id: String,
    href: String,
    media_type: String,
    properties: String,
}

#[derive(Default)]
struct Package {
    title: Option<String>,
    author: Option<String>,
    items: Vec<ManifestItem>,
    spine: Vec<String>,
    toc_id: Option<String>,
}

pub fn parse(path: &Path) -> Result<BookDocument, String> {
    let file = std::fs::File::open(path).map_err(|error| format!("读取 {} 失败：{error}", path.display()))?;
    let mut zip =
        zip::ZipArchive::new(file).map_err(|error| format!("不是有效的 EPUB（zip）：{error}"))?;
    let container = read_entry(&mut zip, "META-INF/container.xml")
        .ok_or_else(|| "EPUB 缺少 META-INF/container.xml".to_string())?;
    let opf_path = rootfile_path(&container).ok_or_else(|| "EPUB 容器未声明 OPF 路径".to_string())?;
    let opf = read_entry(&mut zip, &opf_path)
        .ok_or_else(|| format!("EPUB 缺少 OPF：{opf_path}"))?;
    let package = parse_opf(&opf);
    let base_dir = opf_path.rsplit_once('/').map(|(dir, _)| dir).unwrap_or_default();

    let toc = load_toc(&mut zip, &package, base_dir);
    let by_id: HashMap<&str, &ManifestItem> = package
        .items
        .iter()
        .map(|item| (item.id.as_str(), item))
        .collect();
    let mut docs: Vec<(String, StyledDocument)> = Vec::new();
    for idref in &package.spine {
        let Some(item) = by_id.get(idref.as_str()) else { continue };
        if !is_html(&item.media_type, &item.href) {
            continue;
        }
        let path = join_href(base_dir, &strip_fragment(&item.href));
        let Some(raw) = read_entry(&mut zip, &path) else { continue };
        docs.push((path, parse_document(&raw)));
    }

    // 书脊缺失或不含正文时退回到全部 XHTML，按名称排序。
    if docs.is_empty() {
        let mut names: Vec<String> = package
            .items
            .iter()
            .filter(|item| is_html(&item.media_type, &item.href))
            .map(|item| join_href(base_dir, &strip_fragment(&item.href)))
            .collect();
        names.sort();
        names.dedup();
        for name in names {
            let Some(raw) = read_entry(&mut zip, &name) else { continue };
            docs.push((name, parse_document(&raw)));
        }
    }

    // 读出每篇文档引用的插图字节与尺寸，供渲染层按位置插入。
    let mut images_by_path: HashMap<String, Vec<ChapterImage>> = HashMap::new();
    for (path, doc) in &docs {
        if doc.images.is_empty() {
            continue;
        }
        let images = load_images(&mut zip, path, &doc.images);
        if !images.is_empty() {
            images_by_path.insert(path.clone(), images);
        }
    }

    let chapters = build_chapters(docs, &toc, &images_by_path);
    if chapters.is_empty() {
        return Err("EPUB 中没有可读正文".to_string());
    }
    Ok(BookDocument {
        title: package
            .title
            .filter(|title| !title.trim().is_empty())
            .or_else(|| path.file_stem().map(|stem| stem.to_string_lossy().into_owned())),
        author: package.author,
        chapters,
        is_pdf: false,
    })
}

/// 目录里的一条：文档路径、可选锚点 fragment、标题与层级（顶层为 0）。
struct TocEntry {
    path: String,
    fragment: Option<String>,
    label: String,
    /// 目录中的嵌套深度；普通 EPUB 的「节」都在章节之下，不能据此切章。
    depth: usize,
}

/// 章节正文短于该字符数时视为「残段」（如 MOBI 的 "Chapter N" 标题页），
/// 若紧随的章节标题没有编号则并入其中。
const MIN_STUB_CHARS: usize = 400;

/// 按 TOC 锚点切分出来的一段正文，坐标已换算到段内。
struct Segment {
    doc_index: usize,
    /// 在 `docs[doc_index].text` 中的原始区间，用于跨章链接映射与残段合并判断。
    start: usize,
    end: usize,
    title: String,
    text: String,
    spans: Vec<StyleSpan>,
    images: Vec<ChapterImage>,
    /// 段内坐标 (start, end, href, 来源文档下标)。
    links: Vec<(usize, usize, String, usize)>,
    /// 合并后可能覆盖多个原始区间：(doc_index, start, end)。
    provenance: Vec<(usize, usize, usize)>,
}

/// 把解析好的文档组装成章节：同一文档内多个 TOC 锚点条目时按锚点拆分正文
/// （MOBI 转制 EPUB 的常见形态），否则维持一篇文档一章；
/// 并把脚注/跨章节链接解析成点击目标。
fn build_chapters(
    docs: Vec<(String, StyledDocument)>,
    toc: &[TocEntry],
    images_by_path: &HashMap<String, Vec<ChapterImage>>,
) -> Vec<Chapter> {
    let mut entries_by_path: HashMap<&str, Vec<&TocEntry>> = HashMap::new();
    for entry in toc {
        entries_by_path
            .entry(entry.path.as_str())
            .or_default()
            .push(entry);
    }

    let mut segments: Vec<Segment> = Vec::new();
    for (doc_index, (path, doc)) in docs.iter().enumerate() {
        if doc.text.trim().is_empty() {
            continue;
        }
        let entries = entries_by_path
            .get(path.as_str())
            .cloned()
            .unwrap_or_default();
        // 候选切分点：(偏移, 目录层级, 标题)。
        let mut candidates: Vec<(usize, usize, String)> = Vec::new();
        let mut head_labels: Vec<String> = Vec::new();
        for entry in &entries {
            let offset = entry.fragment.as_ref().and_then(|fragment| {
                doc.anchors
                    .get(&fragment.to_ascii_lowercase())
                    .copied()
            });
            match offset {
                Some(offset) if offset > 0 && offset < doc.text.len() => {
                    candidates.push((offset, entry.depth, entry.label.clone()));
                }
                Some(_) => head_labels.push(entry.label.clone()),
                None => {
                    if entry.fragment.is_none() {
                        head_labels.push(entry.label.clone());
                    }
                    // fragment 解析不到锚点的条目忽略。
                }
            }
        }
        // 只在最上层条目处切分：普通 EPUB 的目录把「节」也列在章之下，
        // 全按锚点切会把一章拆成许多节。深层条目只有在紧跟上一刀、
        // 构成残段（MOBI 的 "Chapter N" + 标题成对条目）时才作为切分点。
        candidates.sort_by_key(|(offset, _, _)| *offset);
        // 层级按本文档在目录中的全部条目判断：若目录里本文档有更上层条目
        // （通常指向文件开头、不带锚点），它就是一章，内层锚点都是小节。
        let min_depth = entries.iter().map(|entry| entry.depth).min().unwrap_or(0);
        let mut cuts: Vec<(usize, String)> = Vec::new();
        for (offset, depth, label) in candidates {
            if cuts.iter().any(|(existing, _)| *existing == offset) {
                continue;
            }
            if depth > min_depth {
                let previous_end = cuts.last().map(|(existing, _)| *existing).unwrap_or(0);
                let gap = doc
                    .text
                    .get(previous_end..offset)
                    .map(|text| text.chars().count())
                    .unwrap_or(usize::MAX);
                if gap >= MIN_STUB_CHARS {
                    continue;
                }
            }
            cuts.push((offset, label));
        }
        if cuts.len() < 2 {
            // 旧行为：整篇文档一章，标题取该路径第一个目录标签。
            let title = entries
                .first()
                .map(|entry| entry.label.clone())
                .or_else(|| first_line_title(&doc.text))
                .unwrap_or_else(|| format!("第 {} 节", segments.len() + 1));
            segments.push(make_segment(doc, doc_index, 0, doc.text.len(), title, images_by_path, path));
            continue;
        }
        cuts.sort_by_key(|(offset, _)| *offset);
        let title0 = head_title(&head_labels)
            .or_else(|| first_line_title(&doc.text[..cuts[0].0]))
            .unwrap_or_else(|| format!("第 {} 节", segments.len() + 1));
        segments.push(make_segment(doc, doc_index, 0, cuts[0].0, title0, images_by_path, path));
        for (index, (offset, label)) in cuts.iter().enumerate() {
            let end = cuts.get(index + 1).map(|(existing, _)| *existing).unwrap_or(doc.text.len());
            segments.push(make_segment(doc, doc_index, *offset, end, label.clone(), images_by_path, path));
        }
    }

    // 残段合并：短编号章（"Chapter N"）+ 紧随的非编号标题 → "Chapter N: 标题"。
    let mut merged: Vec<Segment> = Vec::new();
    let mut current: Option<Segment> = None;
    for segment in segments {
        match current.take() {
            None => current = Some(segment),
            Some(previous) => {
                if should_merge(&previous, &segment, &docs) {
                    current = Some(merge_segments(previous, segment));
                } else {
                    merged.push(previous);
                    current = Some(segment);
                }
            }
        }
    }
    if let Some(segment) = current {
        merged.push(segment);
    }

    let mut chapters: Vec<Chapter> = merged
        .iter()
        .map(|segment| Chapter {
            title: segment.title.clone(),
            text: segment.text.clone(),
            spans: segment.spans.clone(),
            links: Vec::new(),
            images: segment.images.clone(),
        })
        .collect();

    // 跨章链接映射：path → 文档下标 → 覆盖目标偏移的段。
    let footnotes: HashMap<&str, &BTreeMap<String, String>> = docs
        .iter()
        .map(|(path, doc)| (path.as_str(), &doc.footnotes))
        .collect();
    let mut path_to_doc: HashMap<&str, usize> = HashMap::new();
    let mut provenance: Vec<(usize, usize, usize, usize)> = Vec::new();
    for (chapter_index, segment) in merged.iter().enumerate() {
        for (doc_index, start, end) in &segment.provenance {
            provenance.push((*doc_index, *start, *end, chapter_index));
        }
    }
    for (doc_index, (path, _)) in docs.iter().enumerate() {
        path_to_doc.entry(path.as_str()).or_insert(doc_index);
    }
    for (index, segment) in merged.iter().enumerate() {
        let mut links = Vec::new();
        for (start, end, href, source_doc) in &segment.links {
            let (target_path, fragment) = resolve_href(&docs[*source_doc].0, href);
            if let Some(note) = fragment
                .as_deref()
                .and_then(|id| footnotes.get(target_path.as_str()).and_then(|map| map.get(id)))
            {
                if !note.trim().is_empty() {
                    links.push(LinkSpan {
                        start: *start,
                        end: *end,
                        target: LinkTarget::Footnote(note.clone()),
                    });
                    continue;
                }
            }
            let Some(&target_doc) = path_to_doc.get(target_path.as_str()) else {
                continue;
            };
            let offset = fragment
                .as_ref()
                .and_then(|fragment| {
                    docs[target_doc]
                        .1
                        .anchors
                        .get(&fragment.to_ascii_lowercase())
                        .copied()
                })
                .unwrap_or(0);
            let target = provenance
                .iter()
                .find(|(doc, from, to, _)| *doc == target_doc && *from <= offset && offset < *to)
                .or_else(|| {
                    provenance
                        .iter()
                        .find(|(doc, _, _, _)| *doc == target_doc)
                })
                .map(|(_, _, _, chapter)| *chapter);
            if let Some(chapter) = target {
                if chapter != index {
                    links.push(LinkSpan {
                        start: *start,
                        end: *end,
                        target: LinkTarget::Chapter(chapter),
                    });
                }
            }
        }
        chapters[index].links = links;
    }
    chapters
}

/// 段 0 标题：目录里无锚点（或锚点位于文件开头）的标签；
/// 若首个带编号、次个不带编号则合并（MOBI 成对条目落在同一位置的情形）。
fn head_title(labels: &[String]) -> Option<String> {
    let first = labels.first()?;
    if labels.len() > 1
        && chapter_number(first).is_some()
        && chapter_number(&labels[1]).is_none()
    {
        return Some(format!("{}: {}", first, labels[1]));
    }
    Some(first.clone())
}

/// 把文档区间裁剪成一个段，spans/images/links 换算到段内坐标。
fn make_segment(
    doc: &StyledDocument,
    doc_index: usize,
    start: usize,
    end: usize,
    title: String,
    images_by_path: &HashMap<String, Vec<ChapterImage>>,
    path: &str,
) -> Segment {
    let spans: Vec<StyleSpan> = doc
        .spans
        .iter()
        .filter_map(|span| {
            let from = span.start.max(start);
            let to = span.end.min(end);
            (from < to).then(|| StyleSpan {
                start: from - start,
                end: to - start,
                style: span.style,
            })
        })
        .collect();
    let images: Vec<ChapterImage> = images_by_path
        .get(path)
        .map(|list| {
            list.iter()
                .filter(|image| image.at >= start && image.at < end)
                .cloned()
                .map(|mut image| {
                    image.at -= start;
                    image
                })
                .collect()
        })
        .unwrap_or_default();
    let links: Vec<(usize, usize, String, usize)> = doc
        .links
        .iter()
        .filter_map(|link| {
            let from = link.start.max(start);
            let to = link.end.min(end);
            (from < to).then(|| (from - start, to - start, link.href.clone(), doc_index))
        })
        .collect();
    Segment {
        doc_index,
        start,
        end,
        title,
        text: doc.text[start..end].to_string(),
        spans,
        images,
        links,
        provenance: vec![(doc_index, start, end)],
    }
}

/// 残段 a 是否应并入紧随的段 b。
fn should_merge(a: &Segment, b: &Segment, docs: &[(String, StyledDocument)]) -> bool {
    if a.text.chars().count() >= MIN_STUB_CHARS {
        return false;
    }
    if chapter_number(&a.title).is_none() || chapter_number(&b.title).is_some() {
        return false;
    }
    if a.doc_index == b.doc_index {
        return a.end == b.start;
    }
    // 跨文档：a 是其文档最后一段，b 是后续文档的第一段。
    a.doc_index < b.doc_index
        && a.end == docs[a.doc_index].1.text.len()
        && b.start == 0
}

/// 合并相邻段：标题 "a: b"，正文拼接，b 的坐标平移。
fn merge_segments(mut a: Segment, b: Segment) -> Segment {
    let same_doc = a.doc_index == b.doc_index;
    let join = if same_doc { "" } else { "\n" };
    a.title = format!("{}: {}", a.title, b.title);
    let shift = a.text.len() + join.len();
    a.text.push_str(join);
    a.text.push_str(&b.text);
    for mut span in b.spans {
        span.start += shift;
        span.end += shift;
        a.spans.push(span);
    }
    for mut image in b.images {
        image.at += shift;
        a.images.push(image);
    }
    for (start, end, href, source) in b.links {
        a.links.push((start + shift, end + shift, href, source));
    }
    a.provenance.extend(b.provenance);
    a.end = b.end;
    a
}

/// 把文档内 href 解析成（zip 内路径, fragment）。
fn resolve_href(doc_path: &str, href: &str) -> (String, Option<String>) {
    let href = href.trim();
    let (path, fragment) = match href.split_once('#') {
        Some((path, fragment)) => {
            let fragment = fragment.split('?').next().unwrap_or(fragment);
            (path, (!fragment.is_empty()).then(|| percent_decode(fragment)))
        }
        None => (href, None),
    };
    let path = path.split('?').next().unwrap_or(path);
    let target = if path.is_empty() {
        doc_path.to_string()
    } else {
        let dir = doc_path.rsplit_once('/').map(|(dir, _)| dir).unwrap_or_default();
        join_href(dir, path)
    };
    (target, fragment)
}

fn is_html(media_type: &str, href: &str) -> bool {
    media_type.contains("xhtml")
        || media_type.contains("html")
        || href.to_ascii_lowercase().ends_with(".xhtml")
        || href.to_ascii_lowercase().ends_with(".html")
        || href.to_ascii_lowercase().ends_with(".htm")
}

fn read_entry<R: Read + std::io::Seek>(zip: &mut zip::ZipArchive<R>, name: &str) -> Option<String> {
    let mut entry = zip.by_name(name).ok()?;
    if entry.size() > MAX_DOC_BYTES {
        return None;
    }
    let mut bytes = Vec::with_capacity(entry.size() as usize);
    entry.read_to_end(&mut bytes).ok()?;
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

/// 按文档里记录的位置读出插图字节与像素尺寸；读不到或超限的图片直接跳过。
fn load_images<R: Read + Seek>(
    zip: &mut zip::ZipArchive<R>,
    doc_path: &str,
    raws: &[RawImage],
) -> Vec<ChapterImage> {
    let mut images = Vec::new();
    for raw in raws {
        let (target, _) = resolve_href(doc_path, &raw.src);
        if target.is_empty() || target.ends_with('/') {
            continue;
        }
        let Ok(mut entry) = zip.by_name(&target) else {
            continue;
        };
        if entry.size() > MAX_IMAGE_BYTES {
            continue;
        }
        let mut bytes = Vec::with_capacity(entry.size() as usize);
        if entry.read_to_end(&mut bytes).is_err() || bytes.is_empty() {
            continue;
        }
        let (width, height) = image_dimensions(&bytes);
        images.push(ChapterImage {
            at: raw.at,
            width,
            height,
            declared_width: raw.width,
            declared_height: raw.height,
            width_percent: raw.width_percent,
            inline: raw.inline,
            data: Arc::new(bytes),
        });
    }
    images.sort_by_key(|image| image.at);
    images
}

/// 从图片字节读取像素尺寸；格式不受支持时返回 (None, None)。
fn image_dimensions(bytes: &[u8]) -> (Option<u32>, Option<u32>) {
    let Ok(reader) = image::io::Reader::new(std::io::Cursor::new(bytes)).with_guessed_format()
    else {
        return (None, None);
    };
    match reader.into_dimensions() {
        Ok((width, height)) => (Some(width), Some(height)),
        Err(_) => (None, None),
    }
}

fn local_name(name: &[u8]) -> String {
    String::from_utf8_lossy(name)
        .rsplit(':')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase()
}

fn rootfile_path(container: &str) -> Option<String> {
    let mut reader = quick_xml::Reader::from_str(container);
    reader.config_mut().trim_text(true);
    loop {
        match reader.read_event() {
            Ok(Event::Start(event)) | Ok(Event::Empty(event))
                if local_name(event.name().as_ref()) == "rootfile" =>
            {
                return attr(&event, b"full-path");
            }
            Ok(Event::Eof) | Err(_) => return None,
            _ => {}
        }
    }
}

fn parse_opf(opf: &str) -> Package {
    let mut package = Package::default();
    let mut reader = quick_xml::Reader::from_str(opf);
    reader.config_mut().trim_text(true);
    let mut field: Option<&'static str> = None;
    loop {
        match reader.read_event() {
            Ok(Event::Start(event)) => {
                let name = local_name(event.name().as_ref());
                match name.as_str() {
                    "title" => field = Some("title"),
                    "creator" => field = Some("author"),
                    "item" => package.items.push(ManifestItem {
                        id: attr(&event, b"id").unwrap_or_default(),
                        href: attr(&event, b"href").unwrap_or_default(),
                        media_type: attr(&event, b"media-type").unwrap_or_default(),
                        properties: attr(&event, b"properties").unwrap_or_default(),
                    }),
                    "spine" => package.toc_id = attr(&event, b"toc"),
                    "itemref" => {
                        if let Some(idref) = attr(&event, b"idref") {
                            package.spine.push(idref);
                        }
                    }
                    _ => field = None,
                }
            }
            Ok(Event::Empty(event)) => {
                let name = local_name(event.name().as_ref());
                match name.as_str() {
                    "item" => package.items.push(ManifestItem {
                        id: attr(&event, b"id").unwrap_or_default(),
                        href: attr(&event, b"href").unwrap_or_default(),
                        media_type: attr(&event, b"media-type").unwrap_or_default(),
                        properties: attr(&event, b"properties").unwrap_or_default(),
                    }),
                    "itemref" => {
                        if let Some(idref) = attr(&event, b"idref") {
                            package.spine.push(idref);
                        }
                    }
                    _ => {}
                }
                field = None;
            }
            Ok(Event::Text(text)) => {
                if let Some(key) = field {
                    let Ok(value) = text.unescape() else { continue };
                    let value = value.trim();
                    if value.is_empty() {
                        continue;
                    }
                    match key {
                        "title" => package.title.get_or_insert_with(|| value.to_string()),
                        _ => package.author.get_or_insert_with(|| value.to_string()),
                    };
                }
            }
            Ok(Event::End(event)) => {
                if matches!(local_name(event.name().as_ref()).as_str(), "title" | "creator") {
                    field = None;
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    package
}

/// TOC 条目列表（文档顺序）；EPUB3 nav 优先，其次 EPUB2 NCX。
fn load_toc<R: Read + std::io::Seek>(
    zip: &mut zip::ZipArchive<R>,
    package: &Package,
    base_dir: &str,
) -> Vec<TocEntry> {
    let nav = package
        .items
        .iter()
        .find(|item| item.properties.split_whitespace().any(|value| value == "nav"));
    if let Some(item) = nav {
        let path = join_href(base_dir, &strip_fragment(&item.href));
        if let Some(raw) = read_entry(zip, &path) {
            // nav 内部的链接相对 nav 文件自身目录，而非 OPF 目录。
            let toc_dir = path
                .rsplit_once('/')
                .map(|(dir, _)| dir)
                .unwrap_or_default();
            let toc = parse_nav(&raw, &toc_dir);
            if !toc.is_empty() {
                return toc;
            }
        }
    }
    if let Some(toc_id) = &package.toc_id {
        if let Some(item) = package.items.iter().find(|item| &item.id == toc_id) {
            let path = join_href(base_dir, &strip_fragment(&item.href));
            if let Some(raw) = read_entry(zip, &path) {
                let toc_dir = path
                    .rsplit_once('/')
                    .map(|(dir, _)| dir)
                    .unwrap_or_default();
                return parse_ncx(&raw, &toc_dir);
            }
        }
    }
    Vec::new()
}

/// 把 href 拆成（路径, 解码后的 fragment）。
fn split_fragment(href: &str) -> (String, Option<String>) {
    match href.split_once('#') {
        Some((path, fragment)) => {
            let fragment = fragment.split('?').next().unwrap_or(fragment);
            (
                path.to_string(),
                (!fragment.is_empty()).then(|| percent_decode(fragment)),
            )
        }
        None => (
            href.split('?').next().unwrap_or(href).to_string(),
            None,
        ),
    }
}

fn parse_nav(raw: &str, base_dir: &str) -> Vec<TocEntry> {
    // (是否目录 nav, 条目)；目录之外还有 landmarks / page-list 等 nav，
    // 它们的链接（尤其是 page-list 的逐页锚点）不能当章节切分点。
    let mut entries: Vec<(bool, TocEntry)> = Vec::new();
    let mut reader = quick_xml::Reader::from_str(raw);
    reader.config_mut().trim_text(true);
    let mut in_nav = false;
    let mut nav_is_toc = false;
    let mut ol_depth = 0usize;
    let mut href: Option<String> = None;
    let mut label = String::new();
    loop {
        match reader.read_event() {
            Ok(Event::Start(event)) => match local_name(event.name().as_ref()).as_str() {
                "nav" => {
                    in_nav = true;
                    nav_is_toc = is_toc_nav(&event);
                }
                "ol" if in_nav => ol_depth += 1,
                "a" if in_nav => {
                    href = attr(&event, b"href");
                    label.clear();
                }
                _ => {}
            },
            Ok(Event::Text(text)) if in_nav && href.is_some() => {
                if let Ok(value) = text.unescape() {
                    label.push_str(value.trim());
                }
            }
            Ok(Event::End(event)) => match local_name(event.name().as_ref()).as_str() {
                "a" => {
                    if let Some(href) = href.take() {
                        if !label.trim().is_empty() {
                            let (path, fragment) = split_fragment(&href);
                            entries.push((
                                nav_is_toc,
                                TocEntry {
                                    path: join_href(base_dir, &path),
                                    fragment,
                                    label: label.trim().to_string(),
                                    depth: ol_depth.saturating_sub(1),
                                },
                            ));
                        }
                    }
                }
                "ol" if in_nav => ol_depth = ol_depth.saturating_sub(1),
                "nav" => {
                    in_nav = false;
                    nav_is_toc = false;
                }
                _ => {}
            },
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    // 有目录 nav 时只用它；没有（非规范 EPUB）才退回全部 nav。
    let has_toc = entries.iter().any(|(is_toc, _)| *is_toc);
    entries
        .into_iter()
        .filter(|(is_toc, _)| !has_toc || *is_toc)
        .map(|(_, entry)| entry)
        .collect()
}

/// nav 是否声明为目录（`epub:type="toc"` 或 `role="doc-toc"`）。
fn is_toc_nav(event: &BytesStart<'_>) -> bool {
    attr(event, b"type")
        .is_some_and(|value| value.split_whitespace().any(|part| part == "toc"))
        || attr(event, b"role").is_some_and(|value| value.split_whitespace().any(|part| part == "doc-toc"))
}

fn parse_ncx(raw: &str, base_dir: &str) -> Vec<TocEntry> {
    let mut entries: Vec<TocEntry> = Vec::new();
    let mut reader = quick_xml::Reader::from_str(raw);
    reader.config_mut().trim_text(true);
    let mut in_label = false;
    let mut label = String::new();
    // 当前打开的 navPoint 数；其内 content 的层级 = nav_point_depth - 1。
    let mut nav_point_depth = 0usize;
    loop {
        match reader.read_event() {
            Ok(Event::Start(event)) => match local_name(event.name().as_ref()).as_str() {
                "navpoint" => nav_point_depth += 1,
                "navlabel" => {
                    in_label = true;
                    label.clear();
                }
                "content" => {
                    if let Some(src) = attr(&event, b"src") {
                        let depth = nav_point_depth.saturating_sub(1);
                        push_ncx_entry(&mut entries, &mut label, &src, base_dir, depth);
                    }
                }
                _ => {}
            },
            Ok(Event::Empty(event)) => {
                if local_name(event.name().as_ref()) == "content" {
                    if let Some(src) = attr(&event, b"src") {
                        let depth = nav_point_depth.saturating_sub(1);
                        push_ncx_entry(&mut entries, &mut label, &src, base_dir, depth);
                    }
                }
            }
            Ok(Event::Text(text)) if in_label => {
                if let Ok(value) = text.unescape() {
                    label.push_str(value.trim());
                }
            }
            Ok(Event::End(event)) => match local_name(event.name().as_ref()).as_str() {
                "navlabel" => in_label = false,
                "navpoint" => nav_point_depth = nav_point_depth.saturating_sub(1),
                _ => {}
            },
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    entries
}

/// NCX 的 content 紧跟其所属 navLabel；嵌套时子级各自成对，
/// 按文档顺序输出即 playOrder 顺序。
fn push_ncx_entry(
    entries: &mut Vec<TocEntry>,
    label: &mut String,
    src: &str,
    base_dir: &str,
    depth: usize,
) {
    if label.trim().is_empty() {
        return;
    }
    let (path, fragment) = split_fragment(src);
    entries.push(TocEntry {
        path: join_href(base_dir, &path),
        fragment,
        label: std::mem::take(label).trim().to_string(),
        depth,
    });
}

fn first_line_title(text: &str) -> Option<String> {
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && line.chars().count() <= 40)
        .map(|line| line.to_string())
}

fn attr(event: &BytesStart<'_>, key: &[u8]) -> Option<String> {
    event
        .attributes()
        .flatten()
        .find(|attribute| attribute.key.local_name().as_ref() == key)
        .and_then(|attribute| attribute.unescape_value().ok())
        .map(|value| value.into_owned())
}

fn strip_fragment(href: &str) -> String {
    let href = href.split('#').next().unwrap_or_default();
    href.split('?').next().unwrap_or_default().to_string()
}

/// 解析相对 OPF 目录的 href，并做百分号解码与 `..` 规整。
fn join_href(base_dir: &str, href: &str) -> String {
    let href = percent_decode(href);
    let mut parts: Vec<&str> = if base_dir.is_empty() {
        Vec::new()
    } else {
        base_dir.split('/').collect()
    };
    for segment in href.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            value => parts.push(value),
        }
    }
    parts.join("/")
}

fn percent_decode(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            let hex = &raw[index + 1..index + 3];
            if let Ok(value) = u8::from_str_radix(hex, 16) {
                out.push(value);
                index += 3;
                continue;
            }
        }
        out.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    fn build_epub(path: &Path) {
        let file = std::fs::File::create(path).expect("create");
        let mut zip = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default();
        let add = |zip: &mut zip::ZipWriter<std::fs::File>, name: &str, data: &str| {
            zip.start_file(name, options).expect("start");
            zip.write_all(data.as_bytes()).expect("write");
        };
        add(&mut zip, "mimetype", "application/epub+zip");
        add(
            &mut zip,
            "META-INF/container.xml",
            r#"<?xml version="1.0"?><container><rootfiles>
<rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/>
</rootfiles></container>"#,
        );
        add(
            &mut zip,
            "OEBPS/content.opf",
            r#"<?xml version="1.0"?><package xmlns:dc="http://purl.org/dc/elements/1.1/">
<metadata><dc:title>活着</dc:title><dc:creator>余华</dc:creator></metadata>
<manifest>
<item id="nav" href="nav.xhtml" media-type="application/xhtml+xml" properties="nav"/>
<item id="c1" href="text/ch1.xhtml" media-type="application/xhtml+xml"/>
<item id="c2" href="text/ch2.xhtml" media-type="application/xhtml+xml"/>
</manifest>
<spine><itemref idref="c1"/><itemref idref="c2"/></spine></package>"#,
        );
        add(
            &mut zip,
            "OEBPS/nav.xhtml",
            r#"<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops"><body>
<nav epub:type="toc"><ol>
<li><a href="text/ch1.xhtml">第一章 初见</a></li>
<li><a href="text/ch2.xhtml#top">第二章 离别</a></li>
</ol></nav></body></html>"#,
        );
        add(
            &mut zip,
            "OEBPS/text/ch1.xhtml",
            "<html><body><h1>第一章</h1>\
             <p>他推开门<a epub:type=\"noteref\" href=\"#fn1\">1</a>，然后离开。</p>\
             <p><a href=\"ch2.xhtml\">下一章</a></p>\
             <aside epub:type=\"footnote\" id=\"fn1\"><p>注释：门很旧。</p></aside>\
             </body></html>",
        );
        add(
            &mut zip,
            "OEBPS/text/ch2.xhtml",
            "<html><body><h1>第二章</h1><p>她转身离开。</p></body></html>",
        );
        zip.finish().expect("finish");
    }

    #[test]
    fn reads_spine_and_nav_titles() {
        let dir = std::env::temp_dir().join(format!(
            "sepiola-reader-epub-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).expect("mkdir");
        let path = dir.join("book.epub");
        build_epub(&path);
        let book = parse(&path).expect("epub");
        assert_eq!(book.title.as_deref(), Some("活着"));
        assert_eq!(book.author.as_deref(), Some("余华"));
        assert_eq!(book.chapters.len(), 2);
        assert_eq!(book.chapters[0].title, "第一章 初见");
        assert_eq!(book.chapters[1].title, "第二章 离别");
        assert!(book.chapters[0].text.contains("他推开门"));
        // 脚注正文从章节正文中移出，链接解析成脚注/跨章节目标。
        assert!(!book.chapters[0].text.contains("门很旧"));
        let links = &book.chapters[0].links;
        assert_eq!(links.len(), 2);
        assert!(matches!(
            &links[0].target,
            LinkTarget::Footnote(note) if note.contains("门很旧")
        ));
        assert!(matches!(links[1].target, LinkTarget::Chapter(1)));
        assert_eq!(
            &book.chapters[0].text[links[0].start..links[0].end],
            "1"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn resolves_relative_hrefs() {
        assert_eq!(join_href("OEBPS", "text/a%20b.xhtml"), "OEBPS/text/a b.xhtml");
        assert_eq!(join_href("OEBPS/text", "../images/a.png"), "OEBPS/images/a.png");
    }

    /// MOBI 转制 EPUB：多个章节挤在同一 HTML，靠 `#filepos` 锚点分隔，
    /// NCX 用嵌套的 "Chapter N" + 标题成对条目。
    #[test]
    fn splits_chapters_by_anchors() {
        let dir = std::env::temp_dir().join(format!(
            "sepiola-reader-mobi-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).expect("mkdir");
        let path = dir.join("book.epub");
        let file = std::fs::File::create(&path).expect("create");
        let mut zip = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default();
        let add = |zip: &mut zip::ZipWriter<std::fs::File>, name: &str, data: &str| {
            zip.start_file(name, options).expect("start");
            zip.write_all(data.as_bytes()).expect("write");
        };
        add(&mut zip, "mimetype", "application/epub+zip");
        add(
            &mut zip,
            "META-INF/container.xml",
            r#"<?xml version="1.0"?><container><rootfiles>
<rootfile full-path="content.opf" media-type="application/oebps-package+xml"/>
</rootfiles></container>"#,
        );
        add(
            &mut zip,
            "content.opf",
            r#"<?xml version="1.0"?><package xmlns:dc="http://purl.org/dc/elements/1.1/">
<metadata><dc:title>Word Origins</dc:title></metadata>
<manifest>
<item id="t1" href="text1.html" media-type="application/xhtml+xml"/>
<item id="t2" href="text2.html" media-type="application/xhtml+xml"/>
<item id="ncx" href="toc.ncx" media-type="application/x-dtbncx+xml"/>
</manifest>
<spine toc="ncx"><itemref idref="t1"/><itemref idref="t2"/></spine></package>"#,
        );
        add(
            &mut zip,
            "toc.ncx",
            r#"<?xml version="1.0"?><ncx>
<docTitle><text>Word Origins</text></docTitle>
<navMap>
<navPoint id="1" playOrder="1"><navLabel><text>Chapter One</text></navLabel>
  <content src="text1.html#filepos100"/>
  <navPoint id="2" playOrder="2"><navLabel><text>The Object of Etymology</text></navLabel>
    <content src="text1.html#filepos120"/></navPoint>
</navPoint>
<navPoint id="3" playOrder="3"><navLabel><text>Chapter Two</text></navLabel>
  <content src="text1.html#filepos300"/>
  <navPoint id="4" playOrder="4"><navLabel><text>Sound Symbolism</text></navLabel>
    <content src="text2.html"/></navPoint>
</navPoint>
</navMap></ncx>"#,
        );
        add(
            &mut zip,
            "text1.html",
            "<html><body>\
             <h1>Front</h1><p>front matter paragraph.</p>\
             <h1 id=\"filepos100\">Chapter One</h1>\
             <h2 id=\"filepos120\">The Object of Etymology</h2>\
             <p>body of chapter one.</p>\
             <h1 id=\"filepos300\">Chapter Two</h1>\
             </body></html>",
        );
        add(
            &mut zip,
            "text2.html",
            "<html><body><p>body of chapter two.</p>\
             <p><a href=\"text1.html#filepos120\">see ch1</a></p></body></html>",
        );
        zip.finish().expect("finish");

        let book = parse(&path).expect("epub");
        assert_eq!(book.chapters.len(), 3);
        assert_eq!(book.chapters[0].title, "Front");
        assert_eq!(
            book.chapters[1].title,
            "Chapter One: The Object of Etymology"
        );
        assert_eq!(chapter_number(&book.chapters[1].title), Some(1));
        assert!(book.chapters[1].text.contains("body of chapter one"));
        assert!(!book.chapters[1].text.contains("front matter"));
        assert_eq!(book.chapters[2].title, "Chapter Two: Sound Symbolism");
        // 跨文档链接按锚点偏移映射回第一章。
        let links = &book.chapters[2].links;
        assert_eq!(links.len(), 1);
        assert!(matches!(links[0].target, LinkTarget::Chapter(1)));
        std::fs::remove_dir_all(&dir).ok();
    }

    fn temp_dir(prefix: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "{prefix}-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).expect("mkdir");
        dir
    }

    fn write_epub(files: &[(&str, &str)]) -> (std::path::PathBuf, std::path::PathBuf) {
        let dir = temp_dir("sepiola-reader-epub-case");
        let path = dir.join("book.epub");
        let file = std::fs::File::create(&path).expect("create");
        let mut zip = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default();
        for (name, data) in files {
            zip.start_file(*name, options).expect("start");
            zip.write_all(data.as_bytes()).expect("write");
        }
        zip.finish().expect("finish");
        (dir, path)
    }

    const CONTAINER: &str = r#"<?xml version="1.0"?><container><rootfiles>
<rootfile full-path="content.opf" media-type="application/oebps-package+xml"/>
</rootfiles></container>"#;

    /// 普通 EPUB：目录按「文件 → 小节」嵌套，小节锚点不应把一章切成多章。
    #[test]
    fn nested_toc_sections_stay_in_one_chapter() {
        let long_text = "正文段落内容。".repeat(80);
        let (dir, path) = write_epub(&[
            ("mimetype", "application/epub+zip"),
            ("META-INF/container.xml", CONTAINER),
            (
                "content.opf",
                r#"<?xml version="1.0"?><package><metadata><dc:title>Nested</dc:title></metadata>
<manifest>
<item id="c1" href="ch1.html" media-type="application/xhtml+xml"/>
<item id="ncx" href="toc.ncx" media-type="application/x-dtbncx+xml"/>
</manifest>
<spine toc="ncx"><itemref idref="c1"/></spine></package>"#,
            ),
            (
                "toc.ncx",
                r#"<?xml version="1.0"?><ncx><navMap>
<navPoint id="1"><navLabel><text>Chapter One</text></navLabel><content src="ch1.html"/>
  <navPoint id="2"><navLabel><text>Section Two</text></navLabel><content src="ch1.html#sec2"/></navPoint>
  <navPoint id="3"><navLabel><text>Section Three</text></navLabel><content src="ch1.html#sec3"/></navPoint>
</navPoint></navMap></ncx>"#,
            ),
            (
                "ch1.html",
                &format!(
                    "<html><body><h1 id=\"sec1\">Chapter One</h1><p>{long_text}</p>\
                     <h2 id=\"sec2\">Section Two</h2><p>section body.</p>\
                     <h2 id=\"sec3\">Section Three</h2><p>last body.</p></body></html>"
                ),
            ),
        ]);
        let book = parse(&path).expect("epub");
        assert_eq!(book.chapters.len(), 1);
        assert_eq!(book.chapters[0].title, "Chapter One");
        assert!(book.chapters[0].text.contains("section body."));
        assert!(book.chapters[0].text.contains("last body."));
        std::fs::remove_dir_all(&dir).ok();
    }

    /// EPUB3 nav 里的 page-list 是纸质页码导航，不能当章节切分点。
    #[test]
    fn nav_page_list_does_not_split_chapters() {
        let (dir, path) = write_epub(&[
            ("mimetype", "application/epub+zip"),
            ("META-INF/container.xml", CONTAINER),
            (
                "content.opf",
                r#"<?xml version="1.0"?><package><metadata><dc:title>Paged</dc:title></metadata>
<manifest>
<item id="nav" href="nav.xhtml" media-type="application/xhtml+xml" properties="nav"/>
<item id="c1" href="ch1.html" media-type="application/xhtml+xml"/>
<item id="c2" href="ch2.html" media-type="application/xhtml+xml"/>
</manifest>
<spine><itemref idref="c1"/><itemref idref="c2"/></spine></package>"#,
            ),
            (
                "nav.xhtml",
                r#"<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops"><body>
<nav epub:type="toc"><ol>
<li><a href="ch1.html">Chapter One</a></li>
<li><a href="ch2.html">Chapter Two</a></li>
</ol></nav>
<nav epub:type="page-list"><ol>
<li><a href="ch1.html#p1">1</a></li>
<li><a href="ch1.html#p2">2</a></li>
</ol></nav>
</body></html>"#,
            ),
            (
                "ch1.html",
                "<html><body><h1>Chapter One</h1><p id=\"p1\">page one.</p><p id=\"p2\">page two.</p></body></html>",
            ),
            (
                "ch2.html",
                "<html><body><h1>Chapter Two</h1><p>the end.</p></body></html>",
            ),
        ]);
        let book = parse(&path).expect("epub");
        assert_eq!(book.chapters.len(), 2);
        assert_eq!(book.chapters[0].title, "Chapter One");
        assert_eq!(book.chapters[1].title, "Chapter Two");
        std::fs::remove_dir_all(&dir).ok();
    }
}
