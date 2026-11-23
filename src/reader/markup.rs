//! HTML/XHTML → 带样式的纯文本：保留段落与标题换行，解码实体，
//! 并记录标题、粗体、斜体、等宽代码的字节区间，以及脚注与内部链接。

use std::collections::BTreeMap;

use quick_xml::events::{BytesStart, Event};

use super::{StyleSpan, TextStyle};

const BLOCK_TAGS: &[&str] = &[
    "p", "div", "br", "hr", "li", "ul", "ol", "tr", "table", "section", "article", "header",
    "footer", "blockquote", "pre", "h1", "h2", "h3", "h4", "h5", "h6",
];

/// 脚注容器常见的标签。
const NOTE_TAGS: &[&str] = &["aside", "div", "section", "li", "p", "blockquote"];

/// 一篇 XHTML 文档的解析结果。
#[derive(Clone, Debug, Default)]
pub struct StyledDocument {
    pub text: String,
    pub spans: Vec<StyleSpan>,
    /// 文档内的链接（href 尚未解析成章节归属）。
    pub links: Vec<RawLink>,
    /// 文档内的插图（href 尚未解析成 zip 内路径）。
    pub images: Vec<RawImage>,
    /// 脚注：元素 id → 注释正文（脚注正文已从 `text` 中移除）。
    pub footnotes: BTreeMap<String, String>,
    /// 锚点：元素 id（小写）→ 在 `text` 中的字节偏移，首现优先。
    /// MOBI 转制的 EPUB 用 `#fileposNNN` 锚点在同一文件内分隔章节。
    pub anchors: BTreeMap<String, usize>,
}

/// 文档内的一幅图片引用。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RawImage {
    /// 在正文中的插入位置（字节偏移）。
    pub at: usize,
    /// `<img src>` 或 `<image xlink:href>` 的原始地址。
    pub src: String,
    /// 声明的显示宽度（CSS 像素）。
    pub width: Option<f32>,
    /// 声明的显示高度（CSS 像素）。
    pub height: Option<f32>,
    /// 声明的相对宽度（`width:40%` → 0.4）。
    pub width_percent: Option<f32>,
    /// 行内图：与正文同一行，不单独占一个段落。
    pub inline: bool,
}

/// 文档内的一条链接。
#[derive(Clone, Debug, PartialEq)]
pub struct RawLink {
    pub start: usize,
    pub end: usize,
    pub href: String,
    /// 是否为脚注引用（`epub:type="noteref"` 等）。
    pub footnote: bool,
}

pub fn html_to_text(raw: &str) -> String {
    html_to_text_styled(raw).0
}

/// 解析 HTML：返回纯文本与其中的样式区间（区间互不重叠，按出现顺序排列）。
pub fn html_to_text_styled(raw: &str) -> (String, Vec<StyleSpan>) {
    let document = parse_document(raw);
    (document.text, document.spans)
}

/// 解析 HTML：文本、样式区间、链接与脚注。
pub fn parse_document(raw: &str) -> StyledDocument {
    let mut writer = StyledWriter::default();
    let mut state = StyleState::default();
    let mut footnotes = BTreeMap::new();
    let mut links: Vec<RawLink> = Vec::new();
    let mut capture: Option<Capture> = None;
    let mut open_link: Option<(usize, String, bool)> = None;
    let mut images: Vec<RawImage> = Vec::new();
    let mut anchors: BTreeMap<String, usize> = BTreeMap::new();
    let mut footnote_seq = 0usize;
    let mut reader = quick_xml::Reader::from_str(raw);
    reader.config_mut().trim_text(false);
    let mut skip_depth = 0usize;
    loop {
        match reader.read_event() {
            Ok(Event::Start(event)) => {
                let name = local_name(&event.name().as_ref().to_ascii_lowercase());
                if name == "script" || name == "style" {
                    skip_depth += 1;
                    continue;
                }
                if skip_depth > 0 {
                    continue;
                }
                if let Some(mut cap) = capture.take() {
                    if cap.tag == name {
                        cap.depth += 1;
                    }
                    capture = Some(cap);
                    continue;
                }
                let attrs = Attributes::parse(&event);
                if let Some(id) = &attrs.id {
                    anchors
                        .entry(id.to_ascii_lowercase())
                        .or_insert(writer.len());
                }
                if name == "img" || name == "image" {
                    // 图片按当前正文长度记录位置；渲染时据此与文字交替排版。
                    if let Some(image) = raw_image(&attrs, writer.len(), writer.line_has_text()) {
                        images.push(image);
                    }
                }
                if is_footnote_element(&name, &attrs) {
                    footnote_seq += 1;
                    let id = attrs
                        .id
                        .clone()
                        .unwrap_or_else(|| format!("__fn{footnote_seq}"));
                    capture = Some(Capture {
                        tag: name,
                        depth: 1,
                        id,
                        text: String::new(),
                        pending_space: false,
                    });
                    continue;
                }
                if is_block(&name) {
                    writer.push_break();
                }
                state.open(&name);
                if name == "a" {
                    if let Some(href) = attrs.href.clone() {
                        open_link = Some((writer.len(), href, is_noteref(&attrs)));
                    }
                }
            }
            Ok(Event::End(event)) => {
                let name = local_name(&event.name().as_ref().to_ascii_lowercase());
                if name == "script" || name == "style" {
                    skip_depth = skip_depth.saturating_sub(1);
                    continue;
                }
                if skip_depth > 0 {
                    continue;
                }
                if let Some(mut cap) = capture.take() {
                    if cap.tag == name {
                        cap.depth -= 1;
                        if cap.depth == 0 {
                            let text = cap.text.trim().to_string();
                            if !text.is_empty() {
                                footnotes.insert(cap.id, text);
                            }
                            continue;
                        }
                    }
                    capture = Some(cap);
                    continue;
                }
                if name == "a" {
                    close_link(&mut open_link, &mut links, writer.len());
                }
                state.close(&name);
                if is_block(&name) {
                    writer.push_break();
                }
            }
            Ok(Event::Empty(event)) => {
                let name = local_name(&event.name().as_ref().to_ascii_lowercase());
                if skip_depth > 0 || capture.is_some() {
                    continue;
                }
                let attrs = Attributes::parse(&event);
                if let Some(id) = &attrs.id {
                    anchors
                        .entry(id.to_ascii_lowercase())
                        .or_insert(writer.len());
                }
                if name == "img" || name == "image" {
                    if let Some(image) = raw_image(&attrs, writer.len(), writer.line_has_text()) {
                        images.push(image);
                    }
                    continue;
                }
                if is_block(&name) {
                    writer.push_break();
                }
            }
            Ok(Event::Text(text)) => {
                if skip_depth > 0 {
                    continue;
                }
                let value = text
                    .unescape()
                    .map(|value| value.into_owned())
                    .unwrap_or_else(|_| decode_entities(&String::from_utf8_lossy(&text)));
                if let Some(cap) = capture.as_mut() {
                    push_capture(cap, &value);
                } else {
                    writer.push(&value, state.style());
                }
            }
            Ok(Event::CData(data)) => {
                if skip_depth > 0 {
                    continue;
                }
                let value = String::from_utf8_lossy(&data);
                if let Some(cap) = capture.as_mut() {
                    push_capture(cap, &value);
                } else {
                    writer.push(&value, state.style());
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    close_link(&mut open_link, &mut links, writer.len());
    let (text, spans) = writer.finish();
    links.retain_mut(|link| {
        link.end = link.end.min(text.len());
        link.start < link.end
            && text.is_char_boundary(link.start)
            && text.is_char_boundary(link.end)
    });
    StyledDocument {
        text,
        spans,
        links,
        images,
        footnotes,
        anchors,
    }
}

fn close_link(open: &mut Option<(usize, String, bool)>, links: &mut Vec<RawLink>, end: usize) {
    let Some((start, href, footnote)) = open.take() else {
        return;
    };
    if end > start {
        links.push(RawLink {
            start,
            end,
            href,
            footnote,
        });
    }
}

/// 脚注捕获状态：整段脚注正文从章节正文中移出，单独保存。
struct Capture {
    tag: String,
    depth: usize,
    id: String,
    text: String,
    pending_space: bool,
}

fn push_capture(cap: &mut Capture, value: &str) {
    for ch in value.chars() {
        if ch.is_whitespace() {
            cap.pending_space = true;
            continue;
        }
        if cap.pending_space && !cap.text.is_empty() {
            cap.text.push(' ');
        }
        cap.pending_space = false;
        cap.text.push(ch);
    }
}

/// `<a>` / 脚注容器 / `<img>` 上用到的属性。
#[derive(Default)]
struct Attributes {
    href: Option<String>,
    src: Option<String>,
    id: Option<String>,
    /// `epub:type`（local name 为 `type`）。
    kinds: Vec<String>,
    role: Option<String>,
    class: Option<String>,
    /// `<img>` 的宽高声明（属性形式）。
    width: Option<String>,
    height: Option<String>,
    /// `style="width:…;display:…"` 原文。
    style: Option<String>,
}

impl Attributes {
    fn parse(event: &BytesStart<'_>) -> Self {
        let mut attrs = Self::default();
        for attr in event.attributes().with_checks(false).flatten() {
            let Ok(value) = attr.unescape_value() else {
                continue;
            };
            match attr.key.local_name().as_ref() {
                b"href" => attrs.href = Some(value.into_owned()),
                b"src" => attrs.src = Some(value.into_owned()),
                b"id" => attrs.id = Some(value.into_owned()),
                b"type" => attrs.kinds.push(value.to_ascii_lowercase()),
                b"role" => attrs.role = Some(value.to_ascii_lowercase()),
                b"class" => attrs.class = Some(value.to_string()),
                b"width" | b"data-width" => attrs.width = Some(value.into_owned()),
                b"height" | b"data-height" => attrs.height = Some(value.into_owned()),
                b"style" => attrs.style = Some(value.into_owned()),
                _ => {}
            }
        }
        attrs
    }

    /// 声明尺寸：`style` 优先于宽高属性；返回 (px 宽, px 高, 相对宽度)。
    fn image_size(&self) -> (Option<f32>, Option<f32>, Option<f32>) {
        let mut width = None;
        let mut height = None;
        let mut percent = None;
        if let Some(style) = &self.style {
            for decl in style.split(';') {
                let Some((key, value)) = decl.split_once(':') else {
                    continue;
                };
                match key.trim().to_ascii_lowercase().as_str() {
                    "width" => (width, percent) = parse_length(value),
                    "height" => (height, _) = parse_length(value),
                    _ => {}
                }
            }
        }
        if width.is_none() && percent.is_none() {
            (width, percent) = parse_length(self.width.as_deref().unwrap_or_default());
        }
        if height.is_none() {
            height = parse_length(self.height.as_deref().unwrap_or_default()).0;
        }
        (width, height, percent)
    }

    /// `style` 里的 `display` 值（小写，如 `inline`、`block`、`none`）。
    fn display(&self) -> Option<String> {
        self.style.as_deref()?.split(';').find_map(|decl| {
            let (key, value) = decl.split_once(':')?;
            (key.trim().eq_ignore_ascii_case("display"))
                .then(|| value.trim().to_ascii_lowercase())
        })
    }
}

/// 解析 `width`/`height` 值：支持 `px`、无单位像素与百分比。
/// 返回 (CSS 像素, 相对宽度 0..=1)。
fn parse_length(value: &str) -> (Option<f32>, Option<f32>) {
    let value = value
        .trim()
        .trim_end_matches([';', '/'])
        .trim()
        .to_ascii_lowercase();
    if let Some(rest) = value.strip_suffix('%') {
        return (
            None,
            rest.trim().parse::<f32>().ok().map(|p| (p / 100.0).clamp(0.0, 1.0)),
        );
    }
    let rest = value.strip_suffix("px").unwrap_or(&value).trim();
    (rest.parse::<f32>().ok(), None)
}

/// 记录一幅图片；`display:none` 的图片（跟踪像素、装饰）直接丢弃。
fn raw_image(attrs: &Attributes, at: usize, line_has_text: bool) -> Option<RawImage> {
    let src = attrs.src.clone().or_else(|| attrs.href.clone())?;
    let display = attrs.display();
    if display.as_deref() == Some("none") {
        return None;
    }
    let (width, height, width_percent) = attrs.image_size();
    // 显式 display 决定是否行内；否则图片前面同一行已有文字时视为行内。
    let inline = match display.as_deref() {
        Some(value) => value.starts_with("inline") || value == "run-in",
        None => line_has_text,
    };
    Some(RawImage {
        at,
        src,
        width,
        height,
        width_percent,
        inline,
    })
}

fn is_noteref(attrs: &Attributes) -> bool {
    attrs
        .kinds
        .iter()
        .any(|kind| kind == "noteref" || kind == "footnoteref")
        || attrs.role.as_deref() == Some("doc-noteref")
        || attrs.class.as_deref().is_some_and(|class| {
            class
                .split_whitespace()
                .any(|token| matches!(token, "noteref" | "footnote-ref" | "fnref"))
        })
}

fn is_footnote_element(name: &str, attrs: &Attributes) -> bool {
    if !NOTE_TAGS.contains(&name) {
        return false;
    }
    let kind = attrs.kinds.iter().any(|kind| {
        matches!(
            kind.as_str(),
            "footnote" | "endnote" | "rearnote" | "sidenote"
        )
    });
    kind
        || attrs.role.as_deref() == Some("doc-footnote")
        || attrs.class.as_deref().is_some_and(|class| {
            class.split_whitespace().any(|token| {
                matches!(
                    token,
                    "footnote" | "footnotes" | "endnote" | "endnotes" | "rearnote"
                )
            })
        })
}

/// 当前生效的标签样式（用计数处理嵌套）。
#[derive(Default)]
struct StyleState {
    heading: u8,
    bold: usize,
    italic: usize,
    mono: usize,
}

impl StyleState {
    fn open(&mut self, name: &str) {
        if let Some(level) = heading_level(name) {
            self.heading = level;
            self.bold += 1;
            return;
        }
        match name {
            "b" | "strong" => self.bold += 1,
            "i" | "em" | "cite" => self.italic += 1,
            "code" | "pre" | "kbd" | "samp" | "tt" => self.mono += 1,
            _ => {}
        }
    }

    fn close(&mut self, name: &str) {
        if let Some(level) = heading_level(name) {
            if self.heading == level {
                self.heading = 0;
            }
            self.bold = self.bold.saturating_sub(1);
            return;
        }
        match name {
            "b" | "strong" => self.bold = self.bold.saturating_sub(1),
            "i" | "em" | "cite" => self.italic = self.italic.saturating_sub(1),
            "code" | "pre" | "kbd" | "samp" | "tt" => self.mono = self.mono.saturating_sub(1),
            _ => {}
        }
    }

    fn style(&self) -> TextStyle {
        TextStyle {
            bold: self.bold > 0,
            italic: self.italic > 0,
            mono: self.mono > 0,
            heading: self.heading,
        }
    }
}

/// 边写文本边记录样式区间；空白折叠规则与旧版 `html_to_text` 一致。
#[derive(Default)]
struct StyledWriter {
    text: String,
    spans: Vec<StyleSpan>,
    pending_space: bool,
}

impl StyledWriter {
    fn len(&self) -> usize {
        self.text.len()
    }

    /// 当前行是否已有可见文字（用于判断插图是否与文字同行）。
    fn line_has_text(&self) -> bool {
        self.text
            .rsplit('\n')
            .next()
            .is_some_and(|line| !line.trim().is_empty())
    }

    fn push(&mut self, value: &str, style: TextStyle) {
        for ch in value.chars() {
            if ch.is_whitespace() {
                self.pending_space = true;
                continue;
            }
            if self.pending_space {
                self.pending_space = false;
                let last = self.text.chars().last();
                if last.is_some() && last != Some('\n') && last != Some(' ') {
                    self.push_char(' ', style);
                }
            }
            self.push_char(ch, style);
        }
    }

    fn push_break(&mut self) {
        self.pending_space = false;
        self.trim_trailing_spaces();
        if !self.text.is_empty() && !self.text.ends_with('\n') {
            self.push_char('\n', TextStyle::default());
        }
    }

    fn push_char(&mut self, ch: char, style: TextStyle) {
        let start = self.text.len();
        self.text.push(ch);
        let end = self.text.len();
        if let Some(last) = self.spans.last_mut() {
            if last.end == start && last.style == style {
                last.end = end;
                return;
            }
        }
        if !style.is_plain() {
            self.spans.push(StyleSpan { start, end, style });
        }
    }

    fn trim_trailing_spaces(&mut self) {
        let trimmed = self.text.trim_end_matches(' ').len();
        if trimmed < self.text.len() {
            self.truncate(trimmed);
        }
    }

    fn truncate(&mut self, len: usize) {
        self.text.truncate(len);
        self.spans.retain(|span| span.start < len);
        for span in &mut self.spans {
            span.end = span.end.min(len);
        }
    }

    fn finish(mut self) -> (String, Vec<StyleSpan>) {
        let trimmed = self.text.trim_end().len();
        if trimmed < self.text.len() {
            self.truncate(trimmed);
        }
        (self.text, self.spans)
    }
}

fn heading_level(name: &str) -> Option<u8> {
    let level = name.strip_prefix('h')?;
    if level.len() != 1 {
        return None;
    }
    let digit = level.as_bytes()[0];
    (b'1'..=b'6').contains(&digit).then_some(digit - b'0')
}

fn local_name(name: &[u8]) -> String {
    let text = String::from_utf8_lossy(name);
    text.rsplit(':').next().unwrap_or_default().to_string()
}

fn is_block(name: &str) -> bool {
    BLOCK_TAGS.contains(&name)
}

/// quick-xml 无法识别的 HTML 实体（如 `&nbsp;`）兜底替换。
fn decode_entities(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(start) = rest.find('&') {
        out.push_str(&rest[..start]);
        let tail = &rest[start..];
        let Some(end) = tail.find(';') else {
            out.push_str(tail);
            return out;
        };
        let entity = &tail[1..end];
        let replacement = match entity {
            "nbsp" => Some(" "),
            "amp" => Some("&"),
            "lt" => Some("<"),
            "gt" => Some(">"),
            "quot" => Some("\""),
            "apos" | "#39" => Some("'"),
            "mdash" => Some("—"),
            "hellip" => Some("…"),
            "ldquo" => Some("“"),
            "rdquo" => Some("”"),
            _ => None,
        };
        if let Some(value) = replacement {
            out.push_str(value);
        } else if let Some(ch) = numeric_entity(entity) {
            out.push(ch);
        } else {
            out.push_str(&tail[..=end]);
        }
        rest = &tail[end + 1..];
    }
    out.push_str(rest);
    out
}

fn numeric_entity(entity: &str) -> Option<char> {
    let value = if let Some(hex) = entity.strip_prefix("#x").or_else(|| entity.strip_prefix("#X")) {
        u32::from_str_radix(hex, 16).ok()?
    } else if let Some(decimal) = entity.strip_prefix('#') {
        decimal.parse().ok()?
    } else {
        return None;
    };
    char::from_u32(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slice<'a>(text: &'a str, span: &StyleSpan) -> &'a str {
        &text[span.start..span.end]
    }

    #[test]
    fn preserves_paragraphs_and_headings() {
        let text = html_to_text(
            "<?xml version=\"1.0\"?><html xmlns=\"http://www.w3.org/1999/xhtml\"><body>\
             <h1>第一章</h1><p>他推开门。</p><p>外面在下雨&nbsp;。</p>\
             <script>var x = 1;</script></body></html>",
        );
        assert_eq!(text, "第一章\n他推开门。\n外面在下雨 。");
    }

    #[test]
    fn records_image_positions() {
        let document = parse_document(
            "<p>插图前</p><p><img src=\"../images/figure1.png\" alt=\"图\"/></p>\
             <svg xmlns:xlink=\"http://www.w3.org/1999/xlink\">\
             <image xlink:href=\"cover.jpeg\"/></svg><p>插图后</p>",
        );
        assert_eq!(
            document
                .images
                .iter()
                .map(|image| image.src.as_str())
                .collect::<Vec<_>>(),
            vec!["../images/figure1.png", "cover.jpeg"]
        );
        // 位置是正文里的字节偏移：第一张图在「插图前」之后，第二张在其后。
        let first = document.images[0].at;
        let second = document.images[1].at;
        assert!(first <= second);
        assert!(document.text[first..].starts_with("插图后"));
        assert!(document.text[..first].contains("插图前"));
    }

    #[test]
    fn reads_image_sizes_and_inline_display() {
        let document = parse_document(
            "<p>行内<img src=\"a.png\" width=\"19\" height=\"20\"/>图</p>\
             <p><img src=\"b.png\" width=\"60%\" style=\"display:block\"/></p>\
             <p>文字<img src=\"c.png\" style=\"height:12px\"/></p>\
             <p><img src=\"d.png\" style=\"display:none\"/></p>",
        );
        // display:none 的图片（跟踪像素、装饰）不记录。
        assert_eq!(document.images.len(), 3);
        let first = &document.images[0];
        assert_eq!((first.width, first.height), (Some(19.0), Some(20.0)));
        assert!(first.inline);
        let second = &document.images[1];
        assert_eq!(second.width_percent, Some(0.6));
        assert!(!second.inline);
        let third = &document.images[2];
        assert_eq!(third.height, Some(12.0));
        assert!(third.inline);
    }

    #[test]
    fn records_heading_and_inline_styles() {
        let (text, spans) = html_to_text_styled(
            "<h2>卷首</h2><p>普通<b>加粗</b>与<i>斜体</i>和<code>code()</code>。</p>",
        );
        let styles: Vec<(&str, TextStyle)> = spans
            .iter()
            .map(|span| (slice(&text, span), span.style))
            .collect();
        assert_eq!(styles[0].0, "卷首");
        assert_eq!((styles[0].1.heading, styles[0].1.bold), (2, true));
        assert!(styles.iter().any(|(s, style)| *s == "加粗" && style.bold));
        assert!(styles.iter().any(|(s, style)| *s == "斜体" && style.italic));
        assert!(
            styles
                .iter()
                .any(|(s, style)| *s == "code()" && style.mono && style.heading == 0)
        );
        assert!(!styles.iter().any(|(_, style)| style.heading != 0 && style.heading != 2));
    }

    #[test]
    fn extracts_footnotes_and_links() {
        let document = parse_document(
            "<p>正文<a epub:type=\"noteref\" href=\"#fn1\">1</a>，另见\
             <a href=\"ch2.xhtml\">第二章</a>。</p>\
             <aside epub:type=\"footnote\" id=\"fn1\"><p>这是注释。</p></aside>",
        );
        assert_eq!(document.text, "正文1，另见第二章。");
        assert_eq!(
            document.footnotes.get("fn1").map(String::as_str),
            Some("这是注释。")
        );
        assert_eq!(document.links.len(), 2);
        assert!(document.links[0].footnote);
        assert_eq!(
            &document.text[document.links[0].start..document.links[0].end],
            "1"
        );
        assert_eq!(document.links[1].href, "ch2.xhtml");
        assert!(!document.links[1].footnote);
    }

    #[test]
    fn records_anchor_offsets() {
        let document = parse_document(
            "<p>前</p><div id=\"filepos100\">X后</div><p id=\"filepos100\">dup</p>",
        );
        let offset = *document.anchors.get("filepos100").expect("anchor");
        // 首现优先：偏移指向第一个 id 元素的内容。
        assert!(document.text[offset..].starts_with('X'));
    }

    #[test]
    fn decodes_numeric_and_named_entities() {
        assert_eq!(decode_entities("a&amp;b&#33;&#x4e2d;"), "a&b!中");
    }

    #[test]
    fn strips_indentation_whitespace() {
        assert_eq!(html_to_text("<p>  hello\n   world  </p>"), "hello world");
    }
}
