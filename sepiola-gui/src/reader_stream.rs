//! 阅读正文：跨章连续滚动、滑动窗口与选区捕获（M45）。

use std::collections::HashMap;
use std::sync::Arc;

use eframe::egui;
use egui::emath::GuiRounding;
use sepiola::reader::analysis_store::{self, BookAnalysis};
use sepiola::reader::analyze::{Annotation, AnnotationKind};
use sepiola::reader::highlight::Highlight;
use sepiola::reader::pdf::PdfTextPage;
use sepiola::reader::{
    BookDocument, Chapter, ChapterImage, LinkSpan, LinkTarget, ReaderFont, StyleSpan,
};

use crate::reader::{PdfSelect, Selection};
use crate::state::SepiolaApp;

/// 距离底部小于该值时追加下一章。
const APPEND_THRESHOLD: f32 = 320.0;
/// 滚动到窗口顶部（偏移小于该值）时回补上一章，每次只补一章。
const PREPEND_EDGE: f32 = 24.0;
/// 顶部章节完全离开可视区超过该高度（屏数）时丢弃。
const DROP_BUFFER: f32 = 1.0;
/// 可视区下方保留的缓冲（屏数）；更远的章节会被裁掉。
const BOTTOM_BUFFER: f32 = 1.0;
/// 从未测量过的章高兜底值。
const DEFAULT_CHAPTER_HEIGHT: f32 = 800.0;
/// 标题字号相对正文的增量（h1..h6）。
const HEADING_SCALE: [f32; 6] = [10.0, 8.0, 6.0, 4.0, 3.0, 2.0];
/// 注册到字体表里的粗体/斜体字族名（见 `assets::install_cjk_font`）。
const BOLD_FAMILY: &str = "reader-bold";
const SERIF_BOLD_FAMILY: &str = "reader-serif-bold";
const ITALIC_FAMILY: &str = "reader-italic";

/// 阅读字体对应的字体族；系统缺少相应字体或用户选择了自定义字体时回退/切换。
pub fn reader_font_family(ui: &egui::Ui, font: ReaderFont, custom: bool) -> egui::FontFamily {
    let named = |name: &str| egui::FontFamily::Name(name.into());
    let available = |name: &str| {
        ui.fonts_mut(|fonts| fonts.families())
            .contains(&egui::FontFamily::Name(name.into()))
    };
    if custom && available("reader-custom") {
        return named("reader-custom");
    }
    match font {
        ReaderFont::Sans => egui::FontFamily::Proportional,
        ReaderFont::Mono => egui::FontFamily::Monospace,
        ReaderFont::Serif if available("reader-serif") => named("reader-serif"),
        ReaderFont::Kai if available("reader-kai") => named("reader-kai"),
        ReaderFont::Serif | ReaderFont::Kai if available("reader-serif") => named("reader-serif"),
        ReaderFont::Serif | ReaderFont::Kai => egui::FontFamily::Proportional,
    }
}

/// 分析条目的强调色（正文彩标与图例共用）。
pub fn annotation_color(kind: AnnotationKind) -> egui::Color32 {
    match kind {
        AnnotationKind::Phrase => egui::Color32::from_rgb(64, 126, 214),
        AnnotationKind::Expression => egui::Color32::from_rgb(52, 150, 96),
        AnnotationKind::Grammar => egui::Color32::from_rgb(214, 138, 46),
        AnnotationKind::Pattern => egui::Color32::from_rgb(136, 92, 200),
        AnnotationKind::Term => egui::Color32::from_rgb(28, 142, 156),
        AnnotationKind::ProperNoun => egui::Color32::from_rgb(198, 78, 118),
    }
}

fn annotation_fill(kind: AnnotationKind) -> egui::Color32 {
    let [r, g, b, _] = annotation_color(kind).to_array();
    egui::Color32::from_rgba_unmultiplied(r, g, b, 40)
}

/// 按样式区间、链接与 AI 彩标把正文排成 `LayoutJob`；预排版与渲染共用，保证高度一致。
/// `base..base + len` 指定本章正文里本次要排版的字节范围（插图会把正文切成多段）。
fn styled_job(
    ui: &egui::Ui,
    text: &str,
    base: usize,
    len: usize,
    spans: &[StyleSpan],
    links: &[LinkSpan],
    annotations: &[Annotation],
    highlights: &[Highlight],
    font_size: f32,
    family: &egui::FontFamily,
    line_spacing: f32,
    width: f32,
) -> egui::text::LayoutJob {
    let color = ui.visuals().text_color();
    let link_color = ui.visuals().hyperlink_color;
    let spacing = line_spacing.max(0.5);
    let base_font = egui::FontId::new(font_size, family.clone());
    let base_line =
        ui.fonts_mut(|fonts| fonts.row_height(&base_font)) + ui.spacing().extra_text_line_spacing;
    let families = ui.fonts_mut(|fonts| fonts.families());
    // 宋体/楷体的粗体用衬线粗体，避免正文里出现混排的无衬线粗体。
    let bold_name = match family {
        egui::FontFamily::Name(name)
            if name.as_ref() == "reader-serif" || name.as_ref() == "reader-kai" =>
        {
            SERIF_BOLD_FAMILY
        }
        let _ = viewport_changed;
        _ => BOLD_FAMILY,
    };
    let bold_family = egui::FontFamily::Name(bold_name.into());
    let italic_family = egui::FontFamily::Name(ITALIC_FAMILY.into());
    let has_bold = families.contains(&bold_family);
    let has_italic = families.contains(&italic_family);

    let mut job = egui::text::LayoutJob::default();
    job.wrap.max_width = width;
    job.keep_trailing_whitespace = true;

    // 样式区间与链接各自可能部分覆盖文本，先切出所有边界再逐段排版。
    let begin = base.min(text.len());
    let finish = (base + len).min(text.len()).max(begin);
    let mut cuts: Vec<usize> = vec![begin, finish];
    for span in spans {
        cuts.push(span.start.clamp(begin, finish));
        cuts.push(span.end.clamp(begin, finish));
    }
    for link in links {
        cuts.push(link.start.clamp(begin, finish));
        cuts.push(link.end.clamp(begin, finish));
    }
    for annotation in annotations {
        cuts.push(annotation.start.clamp(begin, finish));
        cuts.push(annotation.end.clamp(begin, finish));
    }
    for highlight in highlights {
        cuts.push(highlight.start.clamp(begin, finish));
        cuts.push(highlight.end.clamp(begin, finish));
    }
    cuts.retain(|cut| text.is_char_boundary(*cut));
    cuts.sort_unstable();
    cuts.dedup();

    for pair in cuts.windows(2) {
        let (start, end) = (pair[0], pair[1]);
        if start >= end {
            continue;
        }
        let style = spans
            .iter()
            .find(|span| span.start <= start && end <= span.end)
            .map(|span| span.style)
            .unwrap_or_default();
        let is_link = links
            .iter()
            .any(|link| link.start <= start && end <= link.end);
        let annotation = annotations
            .iter()
            .find(|annotation| annotation.start <= start && end <= annotation.end);
        let highlight = highlights
            .iter()
            .find(|highlight| highlight.start <= start && end <= highlight.end);
        let bonus = style
            .heading
            .checked_sub(1)
            .and_then(|index| HEADING_SCALE.get(index as usize))
            .copied()
            .unwrap_or(0.0);
        let font_family = if style.mono {
            egui::FontFamily::Monospace
        } else if style.bold && has_bold {
            bold_family.clone()
        } else if style.italic && has_italic {
            italic_family.clone()
        } else {
            family.clone()
        };
        let format = egui::text::TextFormat {
            font_id: egui::FontId::new(font_size + bonus, font_family),
            color: if is_link { link_color } else { color },
            background: highlight
                .map(|highlight| {
                    let [r, g, b] = highlight.color;
                    egui::Color32::from_rgba_unmultiplied(r, g, b, 110)
                })
                .or_else(|| annotation.map(|annotation| annotation_fill(annotation.kind)))
                .unwrap_or(egui::Color32::TRANSPARENT),
            line_height: Some((base_line + bonus) * spacing),
            // 没有斜体字面时用倾斜合成。
            italics: style.italic && !has_italic,
            underline: if is_link {
                egui::Stroke::new(1.0, link_color)
            } else if let Some(annotation) = annotation {
                egui::Stroke::new(1.0, annotation_color(annotation.kind))
            } else {
                egui::Stroke::NONE
            },
            ..Default::default()
        };
        job.append(&text[start..end], 0.0, format);
    }
    job
}

/// 点击位置（字符序号）落在哪个链接区间上。
fn link_at_char(chapter: &Chapter, char_index: usize) -> Option<&LinkTarget> {
    let byte = chapter
        .text
        .char_indices()
        .nth(char_index)
        .map(|(offset, _)| offset)
        .unwrap_or(chapter.text.len());
    chapter
        .links
        .iter()
        .find(|link| link.start <= byte && byte < link.end)
        .map(|link| &link.target)
}

/// 字符序号 → 字节偏移（越界时取文本末尾）。
fn char_offset(text: &str, char_index: usize) -> usize {
    text.char_indices()
        .nth(char_index)
        .map(|(offset, _)| offset)
        .unwrap_or(text.len())
}

/// 字节位置是否落在某个 AI 彩标区间内。
fn annotation_at_byte<'a>(
    annotations: &'a [Annotation],
    chapter: usize,
    byte: usize,
) -> Option<&'a Annotation> {
    annotations.iter().find(|annotation| {
        annotation.chapter == chapter && annotation.start <= byte && byte < annotation.end
    })
}

/// 某章已缓存的彩标；正文指纹不一致（已改动）时视为没有缓存。
fn cached_annotations(
    book: &BookAnalysis,
    chapter: &Chapter,
    index: usize,
    enabled: bool,
) -> Vec<Annotation> {
    if !enabled {
        return Vec::new();
    }
    book.entry(index, analysis_store::fingerprint(&chapter.text))
        .map(|entry| entry.annotations.clone())
        .unwrap_or_default()
}

/// 按正文实际使用的字体与换行宽度预排版，得到章节块高度（标题 + 间距 + 正文）。
/// 复刻 `TextEdit` 的默认排版参数，用于回补章节时在同一帧内校正滚动偏移。
fn measure_chapter_height(
    ui: &egui::Ui,
    chapter: &Chapter,
    annotations: &[Annotation],
    font_size: f32,
    family: egui::FontFamily,
    line_spacing: f32,
    paragraph_spacing: f32,
    width: f32,
    pdf_page: Option<&PdfPageView>,
) -> f32 {
    // PDF 页面渲染模式：块高就是整页位图的高度（未渲染时按比例估算）。
    if let Some(pdf) = pdf_page {
        return width / pdf.aspect.max(0.2) + ui.spacing().item_spacing.y;
    }
    let text_color = ui.visuals().text_color();
    let spacing = line_spacing.max(0.5);
    let font_id = egui::FontId::new(font_size, family.clone());
    let line_height =
        (ui.fonts_mut(|f| f.row_height(&font_id)) + ui.spacing().extra_text_line_spacing) * spacing;
    let item_spacing = ui.spacing().item_spacing.y;
    // `TextEdit` 分配的高度不低于 `interact_size.y`（单行短文本会被抬高到该高度）。
    let interact_height = ui.spacing().interact_size.y;
    // 正文按插图切成若干段，逐段排版后累加高度（每段都至少保留一行）。
    let mut body_height = 0.0f32;
    // 上一段正文最后一行的 (右边缘, 行顶偏移, 控件高度)，用于判断行内插图能否挤在同一行。
    let mut last_line: Option<(f32, f32, f32)> = None;
    // 行内图右侧已续排的文字字节数，轮到下一段正文时跳过。
    let mut skip = 0usize;
    let items = chapter_items(chapter);
    for i in 0..items.len() {
        match &items[i] {
            ChapterItem::Text(range) => {
                let start = (range.start + skip).min(range.end);
                skip = 0;
                if start >= range.end {
                    continue;
                }
                let job = styled_job(
                    ui,
                    &chapter.text,
                    start,
                    range.end - start,
                    &chapter.spans,
                    &chapter.links,
                    annotations,
                    &[],
                    font_size,
                    &family,
                    line_spacing,
                    width,
                );
                // 段间距通过下移 galley 行实现，保持单一 TextEdit，可跨段落选择。
                let galley =
                    with_paragraph_spacing(ui.fonts_mut(|f| f.layout_job(job)), paragraph_spacing);
                let text_height = galley.size().y.max(line_height).max(interact_height);
                body_height += text_height + item_spacing;
                last_line = galley
                    .rows
                    .last()
                    .map(|row| (row.pos.x + row.row.size.x, row.pos.y, text_height));
            }
            ChapterItem::Image(index) => {
                let index = *index;
                let image = &chapter.images[index];
                if image.width.is_none() || image.height.is_none() {
                    // 尺寸未知（如 SVG）时渲染成一行占位文字。
                    body_height += line_height + item_spacing;
                    last_line = None;
                    continue;
                }
                let size = image_display_size(image, width, line_height, ui.ctx());
                // 行内图挤在上一行末尾时不占新行，只需补足超出该行的部分。
                if let Some((right, row_top, text_height)) = last_line {
                    if image.inline && right + INLINE_GAP + size.x <= width {
                        let mut band_bottom = row_top + size.y;
                        let mut piece_right = None;
                        // 图片右边的空白继续排后续正文，让文字绕在插图两侧。
                        if let Some(ChapterItem::Text(next)) = items.get(i + 1) {
                            let piece_x = right + INLINE_GAP + size.x;
                            let piece_width = width - piece_x;
                            if let Some((_, skip_bytes, piece_height, piece_row_width)) =
                                inline_follow(
                                    ui,
                                    chapter,
                                    annotations,
                                    next.start,
                                    next.len(),
                                    piece_width,
                                    font_size,
                                    &family,
                                    line_spacing,
                                    interact_height,
                                )
                            {
                                skip = skip_bytes;
                                band_bottom = band_bottom.max(row_top + piece_height);
                                piece_right = Some(piece_x + piece_row_width);
                            }
                        }
                        let accounted = text_height.max(band_bottom);
                        body_height += accounted - text_height;
                        // 续排片段这一行还能继续贴下一幅行内图。
                        last_line = piece_right.map(|x| (x, row_top, accounted));
                        continue;
                    }
                }
                body_height += size.y + IMAGE_PADDING * 2.0 + item_spacing;
                last_line = None;
            }
        }
    }

    let title_job = egui::text::LayoutJob::simple(
        chapter.title.clone(),
        egui::FontId::new(font_size + 2.0, family),
        text_color,
        width,
    );
    let title_height = ui.fonts_mut(|f| f.layout_job(title_job)).size().y;

    // 块高 = 标题 + item spacing + 4.0 + 正文（每段已计入自身的 item spacing）。
    title_height + 4.0 + body_height + item_spacing
}

/// 章节渲染项：一段正文，或一幅插图（`Chapter::images` 的下标）。
enum ChapterItem {
    Text(std::ops::Range<usize>),
    Image(usize),
}

/// 插图上下留白。
const IMAGE_PADDING: f32 = 6.0;
/// 行内插图与前面文字的间距。
const INLINE_GAP: f32 = 4.0;

/// 在已排版的 galley 上为每个新段落的首行加入段间距。
/// 只下移行的纵向位置、不改动文本内容，因此仍是单个 TextEdit，可跨段落选择。
fn with_paragraph_spacing(
    galley: std::sync::Arc<egui::Galley>,
    paragraph_spacing: f32,
) -> std::sync::Arc<egui::Galley> {
    if paragraph_spacing <= 0.0 || galley.rows.len() < 2 {
        return galley;
    }
    let mut galley = std::sync::Arc::unwrap_or_clone(galley);
    let mut shift = 0.0f32;
    let mut starts_paragraph = false;
    for row in &mut galley.rows {
        if starts_paragraph {
            shift += paragraph_spacing;
        }
        row.pos.y += shift;
        // 当前行以换行结束时，下一行就是新段落的首行。
        starts_paragraph = row.ends_with_newline;
    }
    galley.rect.max.y += shift;
    galley.mesh_bounds.max.y += shift;
    std::sync::Arc::new(galley)
}

/// 把章节正文按插图位置切成「正文段 / 插图」的交替序列。
/// 块级图片插到所在行的末尾，避免把一行文字从中间截断；行内图片保留原位置，
/// 渲染时会贴在上一行文字末尾。
fn chapter_items(chapter: &Chapter) -> Vec<ChapterItem> {
    let mut items = Vec::new();
    let mut cursor = 0usize;
    for (index, image) in chapter.images.iter().enumerate() {
        let mut at = image.at.min(chapter.text.len());
        while at < chapter.text.len() && !chapter.text.is_char_boundary(at) {
            at += 1;
        }
        if !image.inline {
            at = match chapter.text[at..].find('\n') {
                Some(offset) => at + offset + 1,
                None => chapter.text.len(),
            };
        }
        if at > cursor {
            items.push(ChapterItem::Text(cursor..at));
            cursor = at;
        }
        items.push(ChapterItem::Image(index));
    }
    if cursor < chapter.text.len() {
        items.push(ChapterItem::Text(cursor..chapter.text.len()));
    }
    items
}

/// 插图在正文宽度内的显示尺寸：优先使用书里声明的宽高，再按可用宽度与高度上限缩放。
fn image_display_size(
    image: &ChapterImage,
    available_width: f32,
    line_height: f32,
    ctx: &egui::Context,
) -> egui::Vec2 {
    let max_width = available_width.max(1.0);
    let (Some(width), Some(height)) = (image.width, image.height) else {
        return egui::Vec2::new(max_width.min(480.0), 120.0);
    };
    if width == 0 || height == 0 {
        return egui::Vec2::new(max_width.min(480.0), 120.0);
    }
    let aspect = width as f32 / height as f32;
    // 声明尺寸优先；只声明了一边时按原始比例补出另一边。
    let mut draw_width = image.declared_width.unwrap_or(width as f32);
    let mut draw_height = image.declared_height.unwrap_or(draw_width / aspect);
    if image.declared_width.is_none() && image.declared_height.is_none() {
        draw_width = width as f32;
        draw_height = height as f32;
    } else if image.declared_width.is_none() {
        draw_width = draw_height * aspect;
    }
    if let Some(percent) = image.width_percent {
        draw_width = max_width * percent;
        draw_height = draw_width / aspect;
    }
    if draw_width > max_width {
        draw_height *= max_width / draw_width;
        draw_width = max_width;
    }
    // 高度上限：行内图最多一行高，保证与文字同行、不把正文挤开；整段插图不超过 70% 视口。
    let max_height = if image.inline {
        line_height
    } else {
        ctx.content_rect().height() * 0.7
    };
    if draw_height > max_height {
        draw_width *= max_height / draw_height;
        draw_height = max_height;
    }
    egui::Vec2::new(draw_width.max(1.0), draw_height.max(1.0))
}

/// 行内图右侧续排时，剩余宽度小于该值就不再往图片右边塞字。
const MIN_INLINE_PIECE_WIDTH: f32 = 24.0;

/// 计算行内图右侧这一行能排下的文字：返回（片段字节数, 后续正文需跳过的字节数, 片段高度, 片段首行宽度）。
/// 片段即从 `start` 开始、能在 `piece_width` 内排成一行的字符；剩余正文另起一段。
fn inline_follow(
    ui: &egui::Ui,
    chapter: &Chapter,
    annotations: &[Annotation],
    start: usize,
    len: usize,
    piece_width: f32,
    font_size: f32,
    family: &egui::FontFamily,
    line_spacing: f32,
    interact_height: f32,
) -> Option<(usize, usize, f32, f32)> {
    if len == 0 || piece_width < MIN_INLINE_PIECE_WIDTH {
        return None;
    }
    let job = styled_job(
        ui,
        &chapter.text,
        start,
        len,
        &chapter.spans,
        &chapter.links,
        annotations,
        &[],
        font_size,
        family,
        line_spacing,
        piece_width,
    );
    let galley = ui.fonts_mut(|f| f.layout_job(job));
    let row = galley.rows.first()?;
    let chars: Vec<(usize, char)> = chapter.text[start..start + len].char_indices().collect();
    // 首行的字符数；行尾空白不带到片段里，避免末尾多出空隙。
    let mut taken = row.glyphs.len().min(chars.len());
    while taken > 0 && chars[taken - 1].1.is_whitespace() {
        taken -= 1;
    }
    if taken == 0 {
        return None;
    }
    let piece_bytes = chars[taken - 1].0 + chars[taken - 1].1.len_utf8();
    // 剩余正文从下一个非空白字符开始（空行/空格由分段自身承担）。
    let mut next = taken;
    while next < chars.len() && chars[next].1.is_whitespace() {
        next += 1;
    }
    let skip_bytes = chars.get(next).map(|(offset, _)| *offset).unwrap_or(len);
    Some((
        piece_bytes,
        skip_bytes,
        row.size.y.max(interact_height),
        row.size.x,
    ))
}

/// 图片纹理缓存：首次渲染时解码，失败也记下来，避免每帧重复尝试。
/// `max_width` 是解码后允许的最大像素宽度（PDF 页按屏幕像素渲染，需要更大的上限）。
fn image_texture(
    textures: &mut HashMap<(usize, usize), Option<egui::TextureHandle>>,
    ctx: &egui::Context,
    chapter: usize,
    index: usize,
    data: &std::sync::Arc<Vec<u8>>,
    max_width: u32,
) -> Option<egui::TextureHandle> {
    if let Some(cached) = textures.get(&(chapter, index)) {
        return cached.clone();
    }
    let texture = decode_texture(
        ctx,
        chapter,
        index,
        data,
        max_width,
        egui::TextureOptions::LINEAR,
    );
    textures.insert((chapter, index), texture.clone());
    texture
}

/// PDF 页位图的纹理：按屏幕物理像素 1:1 渲染，放大缩小时都保持像素边缘清晰。
fn pdf_page_texture(
    textures: &mut HashMap<(usize, usize), Option<egui::TextureHandle>>,
    ctx: &egui::Context,
    page: usize,
    data: &std::sync::Arc<Vec<u8>>,
) -> Option<egui::TextureHandle> {
    let options = egui::TextureOptions {
        magnification: egui::TextureFilter::Nearest,
        minification: egui::TextureFilter::Nearest,
        mipmap_mode: None,
        ..Default::default()
    };
    if let Some(cached) = textures.get(&(page, 0)) {
        return cached.clone();
    }
    let texture = decode_texture(ctx, page, 0, data, PDF_TEXTURE_MAX_WIDTH, options);
    textures.insert((page, 0), texture.clone());
    texture
}

fn decode_texture(
    ctx: &egui::Context,
    chapter: usize,
    index: usize,
    data: &[u8],
    max_width: u32,
    options: egui::TextureOptions,
) -> Option<egui::TextureHandle> {
    let decoded = image::load_from_memory(data).ok()?;
    // 超大插图先等比缩小，避免纹理占用过多显存。
    let decoded = if decoded.width() > max_width {
        let scale = max_width as f32 / decoded.width() as f32;
        let height = (decoded.height() as f32 * scale).round().max(1.0) as u32;
        decoded.resize(max_width, height, image::imageops::FilterType::Triangle)
    } else {
        decoded
    };
    let rgba = decoded.to_rgba8();
    let size = [rgba.width() as usize, rgba.height() as usize];
    let color = egui::ColorImage::from_rgba_unmultiplied(size, rgba.as_raw());
    Some(ctx.load_texture(format!("reader_image_{chapter}_{index}"), color, options))
}

/// 单张插图解码后的最大宽度（像素）。
const MAX_TEXTURE_WIDTH: u32 = 1600;
/// PDF 页纹理的最大宽度：按屏幕物理像素 1:1 渲染，限制上限以控制显存。
const PDF_TEXTURE_MAX_WIDTH: u32 = 4096;

/// 按页面实际点尺寸和屏幕物理像素计算清晰渲染 dpi。
fn adaptive_pdf_dpi(width: f32, pixels_per_point: f32, page_pt_width: f32) -> u32 {
    let needed = width * pixels_per_point / (page_pt_width / 72.0);
    let texture_cap = PDF_TEXTURE_MAX_WIDTH as f32 * 72.0 / page_pt_width;
    needed.min(texture_cap).clamp(96.0, 480.0).ceil() as u32
}

fn pdf_dpi_viewport_changed(
    previous: Option<(f32, f32)>,
    width: f32,
    pixels_per_point: f32,
) -> bool {
    previous.is_none_or(|(previous_width, previous_ppp)| {
        (width - previous_width).abs() > 0.5 || (pixels_per_point - previous_ppp).abs() > 0.01
    })
}

fn pdf_dpi_update_needed(
    previous: Option<(f32, f32)>,
    width: f32,
    pixels_per_point: f32,
    interaction_active: bool,
) -> bool {
    !interaction_active && pdf_dpi_viewport_changed(previous, width, pixels_per_point)
}

fn pdf_page_aspect(
    dims: Option<(u32, u32)>,
    page_aspect: Option<f32>,
    fallback: f32,
    text: Option<&PdfTextPage>,
) -> f32 {
    // 文字层尺寸（页面点，与 dpi 无关）最稳；其次是该页学到过的宽高比，
    // 这样位图被清理/重新渲染时占位尺寸不变，页面不会先放大再缩回。
    text.filter(|page| page.width > 0.0 && page.height > 0.0)
        .map(|page| page.width / page.height)
        .or(page_aspect)
        .or_else(|| {
            dims.and_then(|(width, height)| {
                (width > 0 && height > 0).then_some(width as f32 / height as f32)
            })
        })
        .unwrap_or(fallback)
        .max(0.2)
}

/// PDF 页面渲染窗口外保留的页数（纹理与 PNG 数据）。
const PDF_KEEP: usize = 2;
/// 首个页面渲染完成前的兜底宽高比（宽/高），按 A4 估算。
const DEFAULT_PDF_ASPECT: f32 = 1.0 / 1.4142;

fn pointer_over_toolbar(pointer: Option<egui::Pos2>, buttons: &[egui::Rect]) -> bool {
    pointer.is_some_and(|pos| buttons.iter().any(|rect| rect.contains(pos)))
}

fn reader_scroll_bar_visibility(page_mode: bool) -> egui::scroll_area::ScrollBarVisibility {
    if page_mode {
        egui::scroll_area::ScrollBarVisibility::AlwaysVisible
    } else {
        egui::scroll_area::ScrollBarVisibility::VisibleWhenNeeded
    }
}

/// PDF 页的渲染信息：数据就绪则贴图，否则按宽高比占位。
pub struct PdfPageView {
    /// 该页的 PNG 数据（后台渲染完成后才有）。
    pub data: Option<Arc<Vec<u8>>>,
    /// 已知像素尺寸；未知时按 `aspect` 占位。
    pub dims: Option<(u32, u32)>,
    /// 该页首次渲染时学到的宽高比（宽/高）。页面数据被清理或换 dpi 后仍然保留，
    /// 保证同一页的显示尺寸不随位图是否就绪而变化。
    pub page_aspect: Option<f32>,
    /// 兜底宽高比（宽/高），来自首个成功页。
    pub aspect: f32,
}

/// PDF 页面渲染：整页位图保留原始版面；位图之上叠加 stext 文字层，
/// 拖动即可按原字符位置框选文字（与真实 PDF 阅读器一致）。
fn pdf_page_block(
    ui: &mut egui::Ui,
    index: usize,
    pdf: PdfPageView,
    textures: &mut HashMap<(usize, usize), Option<egui::TextureHandle>>,
    text: Option<&PdfTextPage>,
    select: &mut Option<PdfSelect>,
    found: &mut Option<Selection>,
    deselected: &mut bool,
    toolbar_buttons: &[egui::Rect],
) -> ChapterRender {
    let width = ui.available_width();
    let PdfPageView {
        data,
        dims,
        page_aspect,
        aspect,
    } = pdf;
    let aspect = pdf_page_aspect(dims, page_aspect, aspect, text);
    let mut page_rect = None;
    let has_data = data.is_some();
    if let Some(data) = data {
        if let Some(texture) = pdf_page_texture(textures, ui.ctx(), index, &data) {
            let size = egui::Vec2::new(width, width / aspect);
            let (allocated, _) = ui.allocate_exact_size(size, egui::Sense::hover());
            // 位图对齐到物理像素网格：滚动偏移通常是小数，贴在非整数像素上时
            // 整页会被线性过滤重采样，文字看起来发虚。
            let rect = allocated.round_to_pixels(ui.ctx().pixels_per_point());
            ui.painter().image(
                texture.id(),
                rect,
                egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                egui::Color32::WHITE,
            );
            page_rect = Some(rect);
        }
    }
    let Some(rect) = page_rect else {
        // 占位：按已知或估算的宽高比留白，渲染完成后布局自动更新。
        let height = width / aspect;
        let (rect, _) =
            ui.allocate_exact_size(egui::Vec2::new(width, height), egui::Sense::hover());
        ui.painter()
            .rect_filled(rect, 4.0, ui.visuals().faint_bg_color);
        let label = if has_data {
            "页面图片解码失败"
        } else {
            "页面渲染中…"
        };
        ui.painter().text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            label,
            egui::FontId::proportional(14.0),
            ui.visuals().weak_text_color(),
        );
        return ChapterRender {
            segments: Vec::new(),
        };
    };

    if let Some(page) = text {
        if !page.lines.is_empty() && page.width > 0.0 {
            // 把指针位置换算成页面坐标（pt）。
            let page_pos = |pos: egui::Pos2| -> (f32, f32) {
                let scale = rect.width() / page.width.max(1.0);
                ((pos.x - rect.left()) / scale, (pos.y - rect.top()) / scale)
            };
            // 严格命中：指针必须真的落在某一行文字上（含少量行间/行内容差）。
            // 页边留白、段落之间的大块空白、分栏缝隙都不算命中，
            // 避免点空白却选中离得最近的文字。
            let to_flat = |pos: egui::Pos2| -> Option<usize> {
                let (px, py) = page_pos(pos);
                if px < 0.0 || px > page.width || py < 0.0 || py > page.height {
                    return None;
                }
                let mut hit: Option<(f32, usize)> = None;
                for (line_index, line) in page.lines.iter().enumerate() {
                    let height = (line.rect.3 - line.rect.1).max(1.0);
                    // 只容许上下 35% 行高的偏差；更大的空白一律不命中。
                    if py < line.rect.1 - height * 0.35 || py > line.rect.3 + height * 0.35 {
                        continue;
                    }
                    let distance = if py < line.rect.1 {
                        line.rect.1 - py
                    } else if py > line.rect.3 {
                        py - line.rect.3
                    } else {
                        0.0
                    };
                    if hit.map_or(true, |(found, _)| distance < found) {
                        hit = Some((distance, line_index));
                    }
                }
                let (_, line_index) = hit?;
                let line = &page.lines[line_index];
                let height = (line.rect.3 - line.rect.1).max(1.0);
                // 行首/行尾之外的横向空白同样不算命中。
                if px < line.rect.0 - height * 0.5 || px > line.rect.2 + height * 0.5 {
                    return None;
                }
                let mut offset = 0;
                for prefix in &page.lines[..line_index] {
                    offset += prefix.chars.len() + 1;
                }
                if line.chars.is_empty() {
                    return Some(offset);
                }
                let mut char_index = 0;
                let mut best_distance = f32::INFINITY;
                for (position, character) in line.chars.iter().enumerate() {
                    // 按到字符矩形外缘的距离算，落在字符内距离为 0。
                    let distance = if px < character.rect.0 {
                        character.rect.0 - px
                    } else if px > character.rect.2 {
                        px - character.rect.2
                    } else {
                        0.0
                    };
                    if distance < best_distance {
                        best_distance = distance;
                        char_index = position;
                    }
                }
                // 分栏缝隙、制表位等行内大段空白也不命中。
                if best_distance > height * 0.75 {
                    return None;
                }
                Some(offset + char_index)
            };
            // 拖动扩选用的宽松命中：允许指针移到行外/页边，取最近的字符。
            // 这样从文字上起拖后拖到行尾下方仍能扩到最后一个字；只在拖动过程中使用。
            let to_flat_nearest = |pos: egui::Pos2| -> Option<usize> {
                let (px, py) = page_pos(pos);
                let px = px.clamp(0.0, page.width);
                let py = py.clamp(0.0, page.height.max(1.0));
                let mut best: Option<(f32, usize)> = None;
                for (line_index, line) in page.lines.iter().enumerate() {
                    let distance = if py < line.rect.1 {
                        line.rect.1 - py
                    } else if py > line.rect.3 {
                        py - line.rect.3
                    } else {
                        0.0
                    };
                    if best.map_or(true, |(found, _)| distance < found) {
                        best = Some((distance, line_index));
                    }
                }
                let (_, line_index) = best?;
                let line = &page.lines[line_index];
                let mut offset = 0;
                for prefix in &page.lines[..line_index] {
                    offset += prefix.chars.len() + 1;
                }
                if line.chars.is_empty() {
                    return Some(offset);
                }
                let mut char_index = 0;
                let mut best_distance = f32::INFINITY;
                for (position, character) in line.chars.iter().enumerate() {
                    let middle = (character.rect.0 + character.rect.2) * 0.5;
                    let distance = (px - middle).abs();
                    if distance < best_distance {
                        best_distance = distance;
                        char_index = position;
                    }
                }
                Some(offset + char_index)
            };

            let id = ui.make_persistent_id(("pdf_page_text", index));
            let response = ui.interact(rect, id, egui::Sense::click_and_drag());
            let pointer = ui.input(|input| input.pointer.interact_pos());
            // 指针落在选区工具条上时，按下与拖动都归工具条，
            // 不能再重锚点页面选区（否则点「词典」之类的按钮会清掉/挪动选区）。
            let over_toolbar = pointer_over_toolbar(pointer, toolbar_buttons);
            let pressed = ui.input(|input| input.pointer.any_pressed()) && !over_toolbar;
            // 用「按下是否发生在页面上」判定拖动归属：指针移出页面也继续扩选，
            // 且不会把滚动条/其它控件的拖动误算进来。
            let carrying = response.is_pointer_button_down_on() && !over_toolbar;
            if carrying {
                let position = pointer.and_then(to_flat);
                if pressed && response.contains_pointer() && !response.double_clicked() {
                    // 新的一次按下：重新起锚点；点在空白处则直接取消选区
                    // （不能只依赖松手时的点击判定，拖动会被当成 drag 而不触发）。
                    *select = position.map(|position| PdfSelect {
                        page: index,
                        anchor: position,
                        cursor: position,
                        dragging: true,
                    });
                    if position.is_none() {
                        *deselected = true;
                    }
                } else if let Some(current) =
                    select.as_mut().filter(|current| current.page == index)
                {
                    if current.dragging {
                        // 拖动中允许指针离开文字（行尾下方、页边），取最近字符继续扩选。
                        if let Some(position) = pointer.and_then(to_flat_nearest) {
                            current.cursor = position;
                        }
                    }
                }
            }
            // 双击选词：按字符类别（字母数字 / CJK）向外扩展。
            if response.double_clicked() && !over_toolbar {
                if let Some(position) = pointer.and_then(to_flat) {
                    let (start, end) = word_bounds(page, position);
                    *select = Some(PdfSelect {
                        page: index,
                        anchor: start,
                        cursor: end.saturating_sub(1).max(start),
                        dragging: false,
                    });
                }
            }

            if let Some(current) = select
                .as_ref()
                .copied()
                .filter(|current| current.page == index)
            {
                // 起锚点与当前点相同表示「单击未拖动」，不是选区；
                // 双击选词（`dragging == false`）即使只有一个字也算有效选区。
                let has_range = !current.dragging || current.anchor != current.cursor;
                let (start, end) = if current.anchor <= current.cursor {
                    (current.anchor, current.cursor)
                } else {
                    (current.cursor, current.anchor)
                };
                // end 是闭区间右端，转成开区间。
                let end = (end + 1).min(page_flat_len(page));
                if has_range && end > start {
                    let scale = rect.width() / page.width.max(1.0);
                    let mut highlight = None::<egui::Rect>;
                    // 逐行、逐段收集高亮矩形：跨行选区不能用一个大矩形，
                    // 否则行间空白、分栏缝隙也会被涂上，看起来像选中了空白。
                    let mut runs: Vec<egui::Rect> = Vec::new();
                    let mut collected = String::new();
                    let mut has_picked_line = false;
                    let mut flat = 0;
                    for line in &page.lines {
                        let line_gap = (line.rect.3 - line.rect.1).max(1.0) * 1.5 * scale;
                        let mut run = None::<egui::Rect>;
                        let mut line_text = String::new();
                        for character in &line.chars {
                            if flat >= start && flat < end {
                                line_text.push(character.c);
                                let (x0, y0, x1, y1) = character.rect;
                                let char_rect = egui::Rect::from_min_max(
                                    egui::pos2(rect.left() + x0 * scale, rect.top() + y0 * scale),
                                    egui::pos2(rect.left() + x1 * scale, rect.top() + y1 * scale),
                                );
                                if let Some(existing) = run {
                                    // 同一行内出现大段空白（分栏、制表位）时另起一段。
                                    if char_rect.left() - existing.right() > line_gap {
                                        runs.push(existing);
                                        run = Some(char_rect);
                                    } else {
                                        run = Some(existing.union(char_rect));
                                    }
                                } else {
                                    run = Some(char_rect);
                                }
                            }
                            flat += 1;
                        }
                        if let Some(run) = run {
                            runs.push(run);
                            if has_picked_line {
                                collected.push('\n');
                            }
                            collected.push_str(&line_text);
                            has_picked_line = true;
                        }
                        flat += 1;
                    }
                    for run in &runs {
                        highlight = Some(match highlight {
                            Some(merged) => merged.union(*run),
                            None => *run,
                        });
                        ui.painter().rect_filled(
                            run.expand(1.0),
                            2.0,
                            ui.visuals().selection.bg_fill.gamma_multiply(0.55),
                        );
                    }
                    let picked = collected.trim().to_string();
                    if !picked.is_empty() {
                        *found = Some(Selection {
                            text: picked,
                            id,
                            rect: highlight.unwrap_or(rect),
                            highlight: None,
                        });
                    }
                }
            }
        }
    }

    ChapterRender {
        segments: Vec::new(),
    }
}

/// 拼平文字（逐行文字 + 换行）的字符总数。
fn page_flat_len(page: &PdfTextPage) -> usize {
    page.lines
        .iter()
        .map(|line| line.chars.len() + 1)
        .sum::<usize>()
        .saturating_sub(1)
}

/// 双击选词：在拼平文字里向两侧扩展到同一个「词」的边界，返回半开区间。
fn word_bounds(page: &PdfTextPage, position: usize) -> (usize, usize) {
    let mut chars: Vec<char> = Vec::new();
    for line in &page.lines {
        chars.extend(line.chars.iter().map(|character| character.c));
        chars.push('\n');
    }
    let is_word = |c: char| c.is_alphanumeric() || c == '\u{200c}';
    let position = position.min(chars.len().saturating_sub(1));
    if chars.get(position).is_none_or(|c| !is_word(*c)) {
        return (position, position + 1);
    }
    let mut start = position;
    while start > 0 && chars.get(start - 1).is_some_and(|c| is_word(*c)) {
        start -= 1;
    }
    let mut end = position + 1;
    while chars.get(end).is_some_and(|c| is_word(*c)) {
        end += 1;
    }
    (start, end)
}

/// 一章的渲染结果。
struct ChapterRender {
    /// 各正文段的控件信息（插图把正文切成了多段）。
    segments: Vec<SegmentRender>,
}

/// 一个正文段的渲染结果；`char_base` 是该段首字符在章节全文中的字符序号。
struct SegmentRender {
    id: egui::Id,
    char_base: usize,
    char_len: usize,
    output: egui::text_edit::TextEditOutput,
}

/// 渲染一章的标题、正文与插图，返回各正文段的控件信息。
fn chapter_block(
    ui: &mut egui::Ui,
    index: usize,
    chapter: &Chapter,
    annotations: &[Annotation],
    highlights: &[Highlight],
    font_size: f32,
    family: egui::FontFamily,
    line_spacing: f32,
    paragraph_spacing: f32,
    textures: &mut HashMap<(usize, usize), Option<egui::TextureHandle>>,
) -> ChapterRender {
    ui.add(
        egui::Label::new(
            egui::RichText::new(chapter.title.as_str())
                .size(font_size + 2.0)
                .strong(),
        )
        .wrap(),
    );
    ui.add_space(4.0);
    let width = ui.available_width();
    let spans = chapter.spans.as_slice();
    let links = chapter.links.as_slice();
    let font_id = egui::FontId::new(font_size, family.clone());
    let line_height = (ui.fonts_mut(|f| f.row_height(&font_id))
        + ui.spacing().extra_text_line_spacing)
        * line_spacing.max(0.5);
    let mut rendered = Vec::new();
    let mut char_base = 0usize;
    // 紧邻上一项的文字段下标；插图之后清空，避免行内图贴到更早的行上。
    let mut last_text: Option<usize> = None;
    // 行内图右侧已续排的字节数，轮到下一段正文时跳过。
    let mut skip = 0usize;
    let interact_height = ui.spacing().interact_size.y;
    let column_right = ui.cursor().left() + width;
    let items = chapter_items(chapter);
    for item_index in 0..items.len() {
        match &items[item_index] {
            ChapterItem::Text(range) => {
                let start = (range.start + skip).min(range.end);
                skip = 0;
                // 被续排到图片右侧（含空白）的字符不再出现在本段，补齐字符序号。
                char_base += chapter.text[range.start..start].chars().count();
                if start >= range.end {
                    continue;
                }
                // 段间距通过下移 galley 行实现，保持单一 TextEdit，可跨段落选择。
                let id = ui.make_persistent_id(("reader_text", index, item_index));
                let base = start;
                let len = range.end - start;
                let mut text: &str = &chapter.text[start..range.end];
                let mut layouter =
                    |ui: &egui::Ui, buffer: &dyn egui::TextBuffer, wrap_width: f32| {
                        let job = styled_job(
                            ui,
                            &chapter.text,
                            base,
                            buffer.as_str().len().min(len),
                            spans,
                            links,
                            annotations,
                            highlights,
                            font_size,
                            &family,
                            line_spacing,
                            wrap_width,
                        );
                        let galley = ui.fonts_mut(|fonts| fonts.layout_job(job));
                        with_paragraph_spacing(galley, paragraph_spacing)
                    };
                let output = egui::TextEdit::multiline(&mut text)
                    .id(id)
                    .font(font_id.clone())
                    .frame(egui::Frame::NONE)
                    .desired_width(width)
                    // 默认最少 4 行；正文高度由 galley 决定，限定为 1 行。
                    .desired_rows(1)
                    .margin(egui::Margin::symmetric(0, 0))
                    .layouter(&mut layouter)
                    .show(ui);
                let char_len = chapter.text[start..range.end].chars().count();
                rendered.push(SegmentRender {
                    id,
                    char_base,
                    char_len,
                    output,
                });
                char_base += char_len;
                last_text = Some(rendered.len() - 1);
            }
            ChapterItem::Image(image_index) => {
                let image_index = *image_index;
                let image = &chapter.images[image_index];
                if image.width.is_none() || image.height.is_none() {
                    // SVG 等无法直接解码的格式：留一行占位，避免正文里出现空洞。
                    ui.weak("（图片无法显示）");
                    last_text = None;
                    continue;
                }
                let size = image_display_size(image, width, line_height, ui.ctx());
                let Some(texture) = image_texture(
                    textures,
                    ui.ctx(),
                    index,
                    image_index,
                    &image.data,
                    MAX_TEXTURE_WIDTH,
                ) else {
                    ui.weak("（图片无法显示）");
                    last_text = None;
                    continue;
                };
                // 行内图优先贴在上一行文字末尾，不单独占一行，减少对阅读的打断。
                let anchor = last_text.and_then(|segment_index| {
                    let segment = &rendered[segment_index];
                    let row = segment.output.galley.rows.last()?;
                    let left = segment.output.galley_pos + row.pos.to_vec2();
                    let right = left.x + row.row.size.x;
                    (image.inline && right + INLINE_GAP + size.x <= column_right)
                        .then_some((right + INLINE_GAP, left.y))
                });
                match anchor {
                    Some((x, y)) => {
                        last_text = None;
                        let rect = egui::Rect::from_min_size(egui::pos2(x, y), size);
                        // `put` 会把游标推进到图片底部之后；图比这一行矮时会被拉回上方，
                        // 这里再补上差值，保证后续正文不会压到文字上。
                        let before = ui.cursor().top();
                        ui.put(rect, egui::Image::new(&texture).fit_to_exact_size(size));
                        let mut line_bottom = ui.cursor().top();
                        if line_bottom < before {
                            ui.add_space(before - line_bottom);
                            line_bottom = ui.cursor().top();
                        }
                        // 图片右侧的空白继续排后续正文，文字就绕在插图两侧。
                        if let Some(ChapterItem::Text(next)) = items.get(item_index + 1) {
                            let piece_x = rect.right() + INLINE_GAP;
                            let piece_width = column_right - piece_x;
                            if let Some((piece_bytes, skip_bytes, piece_height, _)) = inline_follow(
                                ui,
                                chapter,
                                annotations,
                                next.start,
                                next.len(),
                                piece_width,
                                font_size,
                                &family,
                                line_spacing,
                                interact_height,
                            ) {
                                let piece_rect = egui::Rect::from_min_size(
                                    egui::pos2(piece_x, y),
                                    egui::vec2(piece_width, piece_height),
                                );
                                let id = ui.make_persistent_id((
                                    "reader_text_inline",
                                    index,
                                    item_index,
                                ));
                                let base = next.start;
                                let mut piece: &str = &chapter.text[base..base + piece_bytes];
                                let mut layouter =
                                    |ui: &egui::Ui,
                                     buffer: &dyn egui::TextBuffer,
                                     wrap_width: f32| {
                                        let job = styled_job(
                                            ui,
                                            &chapter.text,
                                            base,
                                            buffer.as_str().len().min(piece_bytes),
                                            spans,
                                            links,
                                            annotations,
                                            highlights,
                                            font_size,
                                            &family,
                                            line_spacing,
                                            wrap_width,
                                        );
                                        ui.fonts_mut(|fonts| fonts.layout_job(job))
                                    };
                                let inner = ui.scope_builder(
                                    egui::UiBuilder::new().max_rect(piece_rect),
                                    |ui| {
                                        egui::TextEdit::multiline(&mut piece)
                                            .id(id)
                                            .font(font_id.clone())
                                            .frame(egui::Frame::NONE)
                                            .desired_width(piece_width)
                                            .desired_rows(1)
                                            .margin(egui::Margin::symmetric(0, 0))
                                            .layouter(&mut layouter)
                                            .show(ui)
                                    },
                                );
                                let piece_chars =
                                    chapter.text[base..base + piece_bytes].chars().count();
                                rendered.push(SegmentRender {
                                    id,
                                    char_base,
                                    char_len: piece_chars,
                                    output: inner.inner,
                                });
                                skip = skip_bytes;
                                // 下一幅行内图可以继续贴在这段续排文字的末尾。
                                last_text = Some(rendered.len() - 1);
                                let after_piece = ui.cursor().top();
                                if after_piece < line_bottom {
                                    ui.add_space(line_bottom - after_piece);
                                }
                            }
                        }
                    }
                    None => {
                        ui.add_space(IMAGE_PADDING);
                        ui.add(egui::Image::new(&texture).fit_to_exact_size(size));
                        ui.add_space(IMAGE_PADDING);
                        last_text = None;
                    }
                }
            }
        }
    }
    ChapterRender { segments: rendered }
}

/// 滑动窗口的计算结果。
struct WindowPlan {
    start: usize,
    end: usize,
    offset: f32,
    /// 本帧回补的章节及其估计高度（用于渲染后校正偏移）。
    prepended: Option<(usize, f32)>,
}

/// 计算本帧要挂载的章节区间：丢弃顶部、回补顶部、裁掉底部，并给出校正后的滚动偏移。
fn plan_window(
    total: usize,
    mut start: usize,
    mut end: usize,
    mut offset: f32,
    viewport_height: f32,
    heights: &std::collections::HashMap<usize, f32>,
    fallback_height: f32,
    separator_height: f32,
    allow_prepend: bool,
) -> WindowPlan {
    if total == 0 {
        return WindowPlan {
            start: 0,
            end: 0,
            offset,
            prepended: None,
        };
    }
    start = start.min(total - 1);
    end = end.clamp(start, total - 1);

    // 顶部章节离可视区至少两屏时才丢弃，保留一屏以上的回滚余量。
    while start < end {
        let Some(&height) = heights.get(&start) else {
            break;
        };
        let block = height + separator_height;
        if height <= 1.0 || offset - block < viewport_height * (1.0 + DROP_BUFFER) {
            break;
        }
        offset -= block;
        start += 1;
    }

    // 滚到窗口顶部时回补上一章；未知章高先用均值估计。
    // 首帧还没有实测正文宽度（预排版会失准），先不回补，等下一帧再补。
    let mut prepended = None;
    if allow_prepend && start > 0 && offset < PREPEND_EDGE {
        // 未知章高优先借用相邻章节的高度，其次才用全局均值。
        let height = heights
            .get(&(start - 1))
            .or_else(|| heights.get(&start))
            .copied()
            .unwrap_or(fallback_height);
        start -= 1;
        offset += height + separator_height;
        prepended = Some((start, height));
    }

    // 向上阅读时裁掉远在可视区下方的章节，避免窗口无限变长（只在章高已知时判断）。
    let bottom_limit = offset + viewport_height * (1.0 + BOTTOM_BUFFER);
    if end > start {
        let mut cursor_y = 0.0;
        let mut keep_end = start;
        let mut reached = false;
        for index in start..=end {
            let Some(&height) = heights.get(&index) else {
                break;
            };
            if cursor_y > bottom_limit {
                reached = true;
                break;
            }
            cursor_y += height + separator_height;
            keep_end = index;
        }
        if reached && keep_end < end {
            end = keep_end;
        }
    }

    // 内容不足一屏多时向下补足：总高度不够时 ScrollArea 会把滚动偏移夹小，
    // 跳到短章节就会停在原处，看起来“下一章无效”。
    let mut covered: f32 = (start..=end)
        .map(|index| heights.get(&index).copied().unwrap_or(fallback_height) + separator_height)
        .sum();
    while covered <= bottom_limit && end + 1 < total {
        end += 1;
        covered += heights.get(&end).copied().unwrap_or(fallback_height) + separator_height;
    }

    WindowPlan {
        start,
        end,
        offset,
        prepended,
    }
}

/// 当前可见章节：取章节块顶部不超过可视区顶部（带少量容差）的最后一章。
/// 容差用于回补上一章后目标章节恰好贴在可视区顶部时的浮点误差。
fn visible_chapter(start: usize, tops: &[(usize, f32)], viewport_top: f32) -> usize {
    tops.iter()
        .filter(|(_, top)| *top <= viewport_top + 2.0)
        .last()
        .map(|(index, _)| *index)
        .unwrap_or(start)
}

impl SepiolaApp {
    pub fn reader_stream(&mut self, ui: &mut egui::Ui, document: &Arc<BookDocument>) {
        let total = document.chapters.len();
        if total == 0 {
            return;
        }
        let viewport_height = ui.available_height().max(1.0);
        let width = ui.available_width();
        // 宽度变化会让已测高度全部失效，需要重新测量。
        if (width - self.library.reader.stream_width).abs() > 0.5 {
            self.library.reader.stream_width = width;
            self.library.reader.chapter_heights.clear();
            self.library.reader.stream_inner_width = 0.0;
        }
        let page_mode = self.library.reader.pdf_page_mode();
        let pixels_per_point = ui.ctx().pixels_per_point();
        let viewport_changed = pdf_dpi_viewport_changed(
            self.library.reader.pdf_dpi_viewport,
            width,
            pixels_per_point,
        );
        let interaction_active =
            self.library.reader.selection.is_some() || ui.input(|input| input.pointer.any_down());
        let dpi_update_needed = pdf_dpi_update_needed(
            self.library.reader.pdf_dpi_viewport,
            width,
            pixels_per_point,
            interaction_active,
        );
        // 选区或指针交互期间暂缓换分辨率，避免工具条点击时的瞬时布局变化触发整页重渲染。
        // 交互结束后仍会按实际视口尺寸更新。
        if page_mode && width > 100.0 && dpi_update_needed {
            self.library.reader.pdf_dpi_viewport = Some((width, pixels_per_point));
            // 页面点尺寸只认实际渲染结果。`mutool info` 的首页 mediabox 可能
            // 属于封面等异常页（和正文尺寸差很多），用它算 dpi 会把正文页
            // 渲染得过小、再被放大显示，字就发虚。
            if let Some((page_pt_width, _)) =
                self.library.reader.pdf_page_pt.filter(|(pt, _)| *pt > 1.0)
            {
                // 按页在屏幕上实际占用的物理像素反推渲染分辨率，1:1 贴图。
                let target = adaptive_pdf_dpi(width, pixels_per_point, page_pt_width);
                let current = self.library.reader.pdf_dpi;
                // 2% 余量只过滤滚动条出现等 1~2px 的抖动。
                let threshold = 0.02;
                if target != current && (target as f32 / current as f32 - 1.0).abs() > threshold {
                    self.library.reader.set_pdf_dpi(target);
                }
            }
        }

        let font_size = self.library.reader.font_size;
        let custom_font = self.library.reader.custom_font.is_some();
        let family = reader_font_family(ui, self.library.reader.font, custom_font);
        let line_spacing = self.library.reader.line_spacing;
        let paragraph_spacing = self.library.reader.paragraph_spacing;
        // 只取当前渲染窗口内各章的缓存分析结果，避免把整本书的彩标都传进来。
        let show_annotations = self.library.reader.show_annotations;

        let mut offset_override = self.library.reader.pending_offset.take();
        let offset_before = offset_override.unwrap_or(self.library.reader.stream_offset);
        // 首帧还没实测正文换行宽度，预排版高度会失准，先允许渲染、下一帧再回补。
        let measured_width = self.library.reader.stream_inner_width;
        let has_width = measured_width > 1.0;
        // 回补目标章高未知时，先按正文同样的字体与换行宽度预排版，
        // 这样本帧就能用真实高度调整偏移，避免下一帧校正造成的位移。
        let start_hint = self.library.reader.stream_start.min(total - 1);
        if has_width
            && start_hint > 0
            && offset_before < PREPEND_EDGE
            && !self
                .library
                .reader
                .chapter_heights
                .contains_key(&(start_hint - 1))
        {
            let target = &document.chapters[start_hint - 1];
            let annotations = cached_annotations(
                &self.library.reader.book_analysis,
                target,
                start_hint - 1,
                show_annotations,
            );
            let pdf_page = page_mode.then(|| PdfPageView {
                data: None,
                dims: None,
                page_aspect: self
                    .library
                    .reader
                    .pdf_page_aspect
                    .get(&(start_hint - 1))
                    .copied(),
                aspect: self.library.reader.pdf_aspect.unwrap_or(DEFAULT_PDF_ASPECT),
            });
            let height = measure_chapter_height(
                ui,
                target,
                &annotations,
                font_size,
                family.clone(),
                line_spacing,
                paragraph_spacing,
                measured_width,
                pdf_page.as_ref(),
            );
            self.library
                .reader
                .chapter_heights
                .insert(start_hint - 1, height);
        }

        let plan = plan_window(
            total,
            self.library.reader.stream_start,
            self.library.reader.stream_end,
            offset_before,
            viewport_height,
            &self.library.reader.chapter_heights,
            self.average_chapter_height(),
            self.library.reader.separator_height,
            has_width,
        );
        if plan.offset != offset_before {
            offset_override = Some(plan.offset);
        }
        let start = plan.start;
        let end = plan.end;
        let prepended = plan.prepended;
        // 回补被推迟到首帧之后，需要再刷一帧补上。
        let prepend_deferred =
            !has_width && self.library.reader.stream_start > 0 && offset_before < PREPEND_EDGE;
        if prepended.is_some() || prepend_deferred {
            ui.ctx().request_repaint();
        }
        self.library.reader.stream_start = start;
        self.library.reader.stream_end = end;

        // PDF 页面渲染：为当前窗口（前后各多预取一页）发起渲染请求，
        // 并清理窗口之外的纹理与页面数据，避免长书占满内存。
        if page_mode {
            let reader = &mut self.library.reader;
            let request_lo = start.saturating_sub(1);
            let request_hi = (end + 1).min(total - 1);
            for page in request_lo..=request_hi {
                // 页已按当前 dpi 渲染过才算就绪；换分辨率后旧位图仍显示，
                // 但这里会重新请求新 dpi 的版本，到达后无缝替换。
                let rendered = reader.pdf_rendered_dpi.get(&page).copied();
                let text_known = !reader.pdf_has_text || reader.pdf_text.contains_key(&page);
                if (rendered == Some(reader.pdf_dpi) && text_known)
                    || reader.pdf_pending.contains_key(&page)
                {
                    continue;
                }
                if let Some(sender) = reader.pdf_sender.as_ref() {
                    // 章节序号从 0 起，PDF 页码从 1 起；文字层只在缺的时候取。
                    let need_text = reader.pdf_has_text && !reader.pdf_text.contains_key(&page);
                    let dpi = reader.pdf_dpi;
                    if sender.send((page, dpi, need_text)).is_ok() {
                        reader.pdf_pending.insert(page, dpi);
                    }
                }
            }
            let keep_lo = start.saturating_sub(PDF_KEEP);
            let keep_hi = end.saturating_add(PDF_KEEP);
            reader
                .image_textures
                .retain(|(chapter, _), _| *chapter >= keep_lo && *chapter <= keep_hi);
            reader
                .pdf_pages
                .retain(|page, _| *page >= keep_lo && *page <= keep_hi);
            reader
                .pdf_sizes
                .retain(|page, _| *page >= keep_lo && *page <= keep_hi);
            reader
                .pdf_rendered_dpi
                .retain(|page, _| *page >= keep_lo && *page <= keep_hi);
            reader
                .pdf_pending
                .retain(|page, _| *page >= keep_lo && *page <= keep_hi);
            if !reader.pdf_pending.is_empty() {
                ui.ctx()
                    .request_repaint_after(std::time::Duration::from_millis(100));
            }
            reader
                .pdf_text
                .retain(|page, _| *page >= keep_lo && *page <= keep_hi);
            // 选区所在页被清理后，选区也失效（连带关掉工具条）。
            if reader
                .pdf_select
                .is_some_and(|current| current.page < keep_lo || current.page > keep_hi)
            {
                reader.pdf_select = None;
                reader.selection = None;
                reader.toolbar_button_rects.clear();
            }
        }

        let annotations_by_chapter: HashMap<usize, Vec<Annotation>> = if show_annotations {
            (start..=end)
                .filter_map(|index| {
                    let chapter = document.chapters.get(index)?;
                    let annotations = cached_annotations(
                        &self.library.reader.book_analysis,
                        chapter,
                        index,
                        true,
                    );
                    (!annotations.is_empty()).then_some((index, annotations))
                })
                .collect()
        } else {
            HashMap::new()
        };

        let generation = self.library.reader.stream_generation;
        let mut found: Option<Selection> = None;
        // 位图页面上按到了空白处：立即收起选区（不等松手时的点击判定）。
        let mut deselected = false;
        let mut tops: Vec<(usize, f32)> = Vec::new();
        // 各正文段的排版结果（章节、段首字符序号、段字符数、屏幕左上角、galley），
        // 用于在帧末换算「中线处的字符锚点」，以及在恢复时滚动到锚点。
        let mut anchors: Vec<(usize, usize, usize, egui::Pos2, Arc<egui::Galley>)> = Vec::new();
        let mut measured: Vec<(usize, f32)> = Vec::new();
        let mut separator_seen = 0.0_f32;
        // 点中脚注/跨章链接时的目标（章节文本按需跳转或弹出脚注）。
        let mut clicked_link: Option<LinkTarget> = None;
        // 点中 AI 彩标时弹出的条目。
        let mut clicked_annotation: Option<Annotation> = None;

        let mut scroll = egui::ScrollArea::vertical()
            .id_salt(("reader_stream", generation))
            .auto_shrink([false, false])
            // PDF 页面按可用宽度缩放；动态显示滚动条会改变内容宽度，
            // 进而让页面在滚动条出现/收起时短暂放大或缩小。
            .scroll_bar_visibility(reader_scroll_bar_visibility(page_mode))
            // 正文里的拖动用于选字，不能被滚动区当成“拖动滚动”
            // （触屏/触摸板尤其明显，会表现为边选边滚）。
            .scroll_source(egui::scroll_area::ScrollSource {
                drag: egui::scroll_area::DragScroll::Never,
                ..Default::default()
            });
        if let Some(offset) = offset_override {
            scroll = scroll.vertical_scroll_offset(offset);
        }
        let output = scroll.show(ui, |ui| {
            for index in start..=end {
                let block_top = ui.cursor().top();
                let chapter = &document.chapters[index];
                let annotations: &[Annotation] = annotations_by_chapter
                    .get(&index)
                    .map(Vec::as_slice)
                    .unwrap_or(&[]);
                // 用户高亮按章过滤后传入（同一章可能有多段）。
                let highlights: Vec<Highlight> = self
                    .library
                    .reader
                    .highlights
                    .items
                    .iter()
                    .filter(|highlight| highlight.chapter == index)
                    .copied()
                    .collect();
                let render = if page_mode {
                    let (view, page_text, toolbar_button_rects) = {
                        let reader = &self.library.reader;
                        (
                            PdfPageView {
                                data: reader.pdf_pages.get(&index).cloned().flatten(),
                                dims: reader.pdf_sizes.get(&index).copied(),
                                page_aspect: reader.pdf_page_aspect.get(&index).copied(),
                                aspect: reader.pdf_aspect.unwrap_or(DEFAULT_PDF_ASPECT),
                            },
                            reader.pdf_text.get(&index).cloned().flatten(),
                            reader.toolbar_button_rects.clone(),
                        )
                    };
                    pdf_page_block(
                        ui,
                        index,
                        view,
                        &mut self.library.reader.image_textures,
                        page_text.as_deref(),
                        &mut self.library.reader.pdf_select,
                        &mut found,
                        &mut deselected,
                        &toolbar_button_rects,
                    )
                } else {
                    chapter_block(
                        ui,
                        index,
                        chapter,
                        annotations,
                        &highlights,
                        font_size,
                        family.clone(),
                        line_spacing,
                        paragraph_spacing,
                        &mut self.library.reader.image_textures,
                    )
                };
                // 恢复阅读位置：把锚点那一行滚到视口顶部（只做一次）。
                if self.library.reader.restore_anchor.is_some()
                    && index == self.library.reader.position_chapter
                {
                    let anchor = self.library.reader.restore_anchor.unwrap_or_default();
                    let target = render
                        .segments
                        .iter()
                        .find(|segment| anchor < segment.char_base + segment.char_len)
                        .or_else(|| render.segments.last());
                    if let Some(segment) = target {
                        let local = anchor
                            .saturating_sub(segment.char_base)
                            .min(segment.char_len.saturating_sub(1));
                        let rect = segment
                            .output
                            .galley
                            .pos_from_cursor(egui::text::CCursor::new(local))
                            .translate(segment.output.galley_pos.to_vec2());
                        ui.scroll_to_rect(rect, Some(egui::Align::Min));
                        ui.ctx().request_repaint();
                    }
                    // 渲染不出来（空章）也清掉，避免每帧重复尝试。
                    self.library.reader.restore_anchor = None;
                }
                let segments = render.segments;
                for segment in &segments {
                    anchors.push((
                        index,
                        segment.char_base,
                        segment.char_len,
                        segment.output.galley_pos,
                        segment.output.galley.clone(),
                    ));
                }
                // 记录章节块（标题）顶部，而不是正文顶部：回补上一章后正文顶部会落到
                // 可视区下方，用正文顶部判断会把当前章节误判成上一章。
                tops.push((index, block_top));
                // 插图把正文切成多段，选区与点击都按段内偏移加段首字符序号换算。
                let chapter_chars = chapter.text.chars().count();
                let mut candidate: Option<(bool, Selection)> = None;
                for segment in &segments {
                    let text_output = &segment.output;
                    if text_output.response.response.clicked() {
                        if let Some(range) = text_output.cursor_range {
                            let char_index = segment.char_base
                                + range.primary.index.0.min(range.secondary.index.0);
                            let byte = char_offset(&chapter.text, char_index);
                            if let Some(annotation) = annotation_at_byte(annotations, index, byte) {
                                clicked_annotation = Some(annotation.clone());
                            } else if let Some(target) = link_at_char(chapter, char_index) {
                                clicked_link = Some(target.clone());
                            }
                        }
                    }
                    if let Some(range) = text_output.cursor_range {
                        let first = range.primary.index.0.min(range.secondary.index.0);
                        let last = range.primary.index.0.max(range.secondary.index.0);
                        if last > first {
                            let picked: String = chapter
                                .text
                                .chars()
                                .skip(segment.char_base + first)
                                .take(last - first)
                                .collect();
                            if !picked.trim().is_empty() {
                                // 记录整个选区的屏幕包围盒（首末光标所在行），工具条贴在它上/下方。
                                let head = text_output.galley.pos_from_cursor(range.primary);
                                let tail = text_output.galley.pos_from_cursor(range.secondary);
                                let offset = text_output.galley_pos.to_vec2();
                                let focused = text_output.response.response.has_focus();
                                let selection = Selection {
                                    text: picked,
                                    id: segment.id,
                                    rect: head.union(tail).translate(offset),
                                    highlight: Some((
                                        index,
                                        segment.char_base + first,
                                        segment.char_base + last,
                                    )),
                                };
                                // 多段各自保留旧的选区状态时，优先采纳当前聚焦的那段。
                                let keep_existing = candidate.is_some() && !focused;
                                if !keep_existing {
                                    candidate = Some((focused, selection));
                                }
                            }
                        }
                    }
                    // 从分析结果跳转过来：把选区落到条目上并滚动到它所在的行。
                    if let Some((target, start_byte, end_byte)) = self.library.reader.pending_item {
                        if target == index {
                            let start_char = chapter.text[..start_byte.min(chapter.text.len())]
                                .chars()
                                .count();
                            let end_char = chapter.text[..end_byte.min(chapter.text.len())]
                                .chars()
                                .count();
                            let segment_end = segment.char_base + segment.char_len;
                            // 起点落在段尾的边界上时属于下一段，避免产生空选区；
                            // 章末位置仍然由最后一段承接。
                            let in_segment = start_char >= segment.char_base
                                && (start_char < segment_end || start_char == chapter_chars);
                            if in_segment {
                                let first = start_char - segment.char_base;
                                let last = end_char
                                    .saturating_sub(segment.char_base)
                                    .clamp(first, segment.char_len);
                                let mut state =
                                    egui::text_edit::TextEditState::load(ui.ctx(), segment.id)
                                        .unwrap_or_default();
                                state
                                    .cursor
                                    .set_char_range(Some(egui::text::CCursorRange::two(
                                        egui::text::CCursor::new(first),
                                        egui::text::CCursor::new(last),
                                    )));
                                state.store(ui.ctx(), segment.id);
                                let head = text_output
                                    .galley
                                    .pos_from_cursor(egui::text::CCursor::new(first));
                                let rect = head.translate(text_output.galley_pos.to_vec2());
                                ui.scroll_to_rect(rect, Some(egui::Align::Center));
                                self.library.reader.pending_item = None;
                                ui.ctx().request_repaint();
                            }
                        }
                    }
                }
                if let Some((_, selection)) = candidate {
                    found = Some(selection);
                }
                // 章高不含分隔线；分隔线单独计量，丢弃/回补时再累加。
                measured.push((index, (ui.cursor().top() - block_top).max(1.0)));
                if index < end {
                    let separator_top = ui.cursor().top();
                    ui.add_space(14.0);
                    ui.separator();
                    ui.add_space(6.0);
                    separator_seen = (ui.cursor().top() - separator_top).max(0.0);
                }
            }
            ui.add_space(28.0);
        });

        for (index, height) in measured {
            self.library.reader.chapter_heights.insert(index, height);
        }
        if let Some(annotation) = clicked_annotation {
            self.library.reader.annotation_detail = Some(annotation);
        }
        if separator_seen > 0.0 {
            self.library.reader.separator_height = separator_seen;
        }
        self.library.reader.stream_inner_width = output.inner_rect.width();
        self.library.reader.stream_offset = output.state.clamped_offset().y;

        // 回补时用的是估计高度，渲染后用实际高度把内容位置校回来。
        if let Some((index, estimate)) = prepended {
            if let Some(&actual) = self.library.reader.chapter_heights.get(&index) {
                let delta = actual - estimate;
                if delta.abs() > 0.5 {
                    let corrected = self.library.reader.stream_offset + delta;
                    self.library.reader.stream_offset = corrected;
                    // 下一帧强制写回，否则 ScrollArea 内部状态会覆盖校正结果。
                    self.library.reader.pending_offset = Some(corrected);
                    ui.ctx().request_repaint();
                }
            }
        }

        // 按在页面空白处：选区与工具条立即收起（拖动松手不会产生 click 事件）。
        if deselected {
            self.library.reader.selection = None;
            self.library.reader.toolbar_button_rects.clear();
        }

        let viewport_top = output.inner_rect.top();
        // 用屏幕中线判断「正在读哪一章」：视口顶部残留上一章结尾时不应算上一章，
        // 否则读到新章前半屏，进度却一直记在旧章（56 被存成 55 的根因）。
        let visible = visible_chapter(
            start,
            &tops,
            viewport_top + output.inner_rect.height() * 0.5,
        );
        // 回补章节的高度是估计值，本帧的块顶位置可能偏；下一帧会按实测高度校正，
        // 此刻不要用它改当前章节，避免把进度记到上一章。
        let correcting = self.library.reader.pending_offset.is_some();
        // 位置锚点：视口顶部那一行文字。恢复时把它滚回视口顶部，与保存完全对称；
        // 顶部很可能还是上一章的结尾，所以锚点章节与显示章节是两回事。
        let top_chapter = visible_chapter(start, &tops, viewport_top);
        let anchor = anchors
            .iter()
            .filter(|(chapter, ..)| *chapter == top_chapter)
            .filter(|(_, _, _, pos, galley)| {
                pos.y <= viewport_top && pos.y + galley.size().y > viewport_top
            })
            .max_by_key(|(_, base, ..)| *base)
            .map(|(_, base, _, pos, galley)| {
                let y = (viewport_top - pos.y).clamp(0.0, galley.size().y);
                *base + galley.cursor_from_pos(egui::vec2(0.0, y)).index.0
            })
            // 顶部落在标题/插图/章节间隔里：退回该章第一段的首字符。
            .or_else(|| {
                anchors
                    .iter()
                    .filter(|(chapter, ..)| *chapter == top_chapter)
                    .min_by_key(|(_, base, ..)| *base)
                    .map(|(_, base, ..)| *base)
            });
        if !correcting {
            // 章内偏移：视口顶端到锚点章节块顶的距离（tops 是屏幕坐标，不能与
            // stream_offset 直接相减）。恢复时以同一基准写回，保证保存/恢复对称。
            let local = tops
                .iter()
                .find(|(index, _)| *index == top_chapter)
                .map(|(_, top)| (viewport_top - top).max(0.0))
                .unwrap_or_default();
            // 算不出锚点（顶部在标题/间隔里）时，同章沿用旧值，换章则作废。
            let previous_chapter = self.library.reader.position_chapter;
            let current_anchor = match anchor {
                Some(anchor) => Some(anchor),
                None if previous_chapter == top_chapter => self.library.reader.anchor_char,
                None => None,
            };
            let position_changed = match self.library.reader.last_saved_position {
                Some((chapter, offset, saved_anchor, display)) => {
                    chapter != top_chapter
                        || (offset - local).abs() > 0.5
                        || saved_anchor != current_anchor
                        || display != visible
                }
                None => true,
            };
            self.library.reader.chapter_offset = local;
            self.library.reader.anchor_char = current_anchor;
            self.library.reader.position_chapter = top_chapter;
            // 显示用的章节跟随屏幕中线（顶栏、目录高亮、分析入口都用它）。
            self.library.reader.chapter = visible;
            // 滚动停稳（松手且没有滚动惯性）后落盘，避免窗口关闭时丢失位移。
            let settled = ui.input(|input| {
                !input.pointer.any_down() && input.smooth_scroll_delta.y.abs() < 0.5
            });
            if !settled {
                // 滚动还在进行：稍后再检查一次，保证停稳后一定落盘。
                ui.ctx()
                    .request_repaint_after(std::time::Duration::from_millis(150));
            } else if position_changed {
                self.save_reader_position();
            }
        }

        // 点击工具条本身不算“点击别处”，否则按钮还没处理就被清掉；
        // 点中正文时即便浮层压在上面也要关闭。位图页面的文字选区是自定义的，
        // 点到页面以外同样要关闭。拖动过程中（按住不放）绝不关闭，
        // 否则刚起的锚点会被清掉，表现为“拖到一半就选不中了”。
        let overlay_selection = self.library.reader.pdf_select.is_some();
        let dismiss = (found.is_none() || overlay_selection)
            && !ui.input(|input| input.pointer.primary_down())
            && {
                let clicked = ui.input(|input| input.pointer.any_click());
                let pointer = ui.input(|input| input.pointer.interact_pos());
                let on_toolbar =
                    pointer_over_toolbar(pointer, &self.library.reader.toolbar_button_rects);
                ui.input(|input| input.key_pressed(egui::Key::Escape)) || (clicked && !on_toolbar)
            };

        if let Some(selection) = found {
            // 每帧更新锚点：滚动时工具条要跟着选区走，否则会停在不相关的文字上挡住鼠标。
            self.library.reader.selection = Some(selection);
        } else if dismiss {
            self.library.reader.selection = None;
            self.library.reader.toolbar_button_rects.clear();
            self.library.reader.pdf_select = None;
        }

        let remaining =
            output.content_size.y - (output.state.clamped_offset().y + output.inner_rect.height());
        if remaining < APPEND_THRESHOLD && end + 1 < total {
            self.library.reader.stream_end = end + 1;
            ui.ctx().request_repaint();
        }

        if self.library.reader.selection.is_some() {
            self.selection_toolbar(ui);
        }

        match clicked_link {
            Some(LinkTarget::Chapter(index)) => self.goto_chapter(index),
            Some(LinkTarget::Footnote(text)) => {
                self.library.reader.footnote = Some(text);
                self.library.reader.selection = None;
                self.library.reader.toolbar_button_rects.clear();
            }
            None => {}
        }
    }

    fn average_chapter_height(&self) -> f32 {
        let heights = &self.library.reader.chapter_heights;
        if heights.is_empty() {
            DEFAULT_CHAPTER_HEIGHT
        } else {
            heights.values().sum::<f32>() / heights.len() as f32
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sepiola::reader::TextStyle;
    use std::collections::HashMap;

    fn heights(entries: &[(usize, f32)]) -> HashMap<usize, f32> {
        entries.iter().copied().collect()
    }

    #[test]
    fn pointer_over_toolbar_requires_toolbar_hit() {
        let toolbar = egui::Rect::from_min_size(egui::pos2(100.0, 100.0), egui::vec2(300.0, 40.0));
        let buttons = [toolbar];

        assert!(pointer_over_toolbar(
            Some(egui::pos2(150.0, 120.0)),
            &buttons
        ));
        assert!(!pointer_over_toolbar(
            Some(egui::pos2(50.0, 120.0)),
            &buttons
        ));
        assert!(!pointer_over_toolbar(None, &buttons));
        assert!(!pointer_over_toolbar(Some(egui::pos2(150.0, 120.0)), &[]));
    }

    #[test]
    fn pdf_dpi_viewport_changes_only_for_layout_or_scale_changes() {
        let previous = Some((600.0, 1.5));

        assert!(!pdf_dpi_viewport_changed(previous, 600.25, 1.505));
        assert!(pdf_dpi_viewport_changed(previous, 640.0, 1.5));
        assert!(pdf_dpi_viewport_changed(previous, 600.0, 2.0));
    }

    #[test]
    fn pdf_dpi_update_waits_until_selection_interaction_ends() {
        let previous = Some((600.0, 1.5));

        assert!(!pdf_dpi_update_needed(previous, 640.0, 1.5, true));
        assert!(!pdf_dpi_update_needed(previous, 600.0, 1.5, false));
        assert!(pdf_dpi_update_needed(previous, 640.0, 1.5, false));
    }

    #[test]
    fn pdf_page_aspect_uses_stable_text_page_dimensions() {
        let text = PdfTextPage {
            width: 612.5,
            height: 792.0,
            lines: Vec::new(),
        };
        let low_dpi = pdf_page_aspect(Some((817, 1056)), None, 0.75, Some(&text));
        let high_dpi = pdf_page_aspect(Some((1276, 1650)), None, 0.75, Some(&text));

        assert_eq!(low_dpi, high_dpi);
        assert!((low_dpi - (612.5 / 792.0)).abs() < f32::EPSILON);
    }

    #[test]
    fn pdf_page_keeps_its_aspect_while_the_bitmap_is_missing() {
        // 位图被清理（或等待重新渲染）时，占位尺寸必须还是这一页自己的宽高比，
        // 不能退回别的页（比如封面）的比例，否则页面会先放大再缩回。
        let learned = 612.0 / 792.0;
        let placeholder = pdf_page_aspect(None, Some(learned), 1.0, None);
        let with_bitmap = pdf_page_aspect(Some((1224, 1584)), Some(learned), 1.0, None);

        assert_eq!(placeholder, with_bitmap);
        assert!((placeholder - learned).abs() < 1e-6);
    }

    #[test]
    fn pdf_scroll_area_width_stays_stable_when_content_height_changes() {
        let ctx = egui::Context::default();
        let frame = |content_height: f32, time: f64| {
            let mut input = egui::RawInput::default();
            input.screen_rect = Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(600.0, 400.0),
            ));
            input.time = Some(time);
            let mut inner_width = 0.0;
            let mut output = ctx.run_ui(input, |ui| {
                let result = egui::ScrollArea::vertical()
                    .id_salt("pdf_scroll_width_regression")
                    .auto_shrink([false, false])
                    .scroll_bar_visibility(reader_scroll_bar_visibility(true))
                    .show(ui, |ui| ui.add_space(content_height));
                inner_width = result.inner_rect.width();
            });
            output.textures_delta.clear();
            inner_width
        };

        let mut time = 0.0;
        let mut widths_with_overflow = Vec::new();
        for _ in 0..10 {
            widths_with_overflow.push(frame(1000.0, time));
            time += 0.1;
        }
        let mut width_without_overflow = 0.0;
        for _ in 0..20 {
            width_without_overflow = frame(100.0, time);
            time += 0.1;
        }

        assert!(
            widths_with_overflow
                .iter()
                .all(|width| (*width - widths_with_overflow[0]).abs() <= 0.5),
            "PDF content width changed while the scroll area initialized: {widths_with_overflow:?}"
        );
        assert_eq!(
            widths_with_overflow[0], width_without_overflow,
            "content height changes must not change PDF page width"
        );
    }

    #[test]
    fn pdf_toolbar_dictionary_keeps_page_geometry_stable() {
        let ctx = egui::Context::default();
        let mut app = SepiolaApp::new(&eframe::CreationContext::_new_kittest(ctx.clone()));
        app.library.root.clear();
        app.library.reader = crate::reader::ReaderState::default();
        let document = Arc::new(BookDocument {
            is_pdf: true,
            chapters: vec![Chapter {
                text: "abcdefghij".into(),
                ..Chapter::default()
            }],
            ..BookDocument::default()
        });
        let reader = &mut app.library.reader;
        reader.rel = Some("test.pdf".into());
        reader.title = "Database Architecture".into();
        reader.pdf_path = Some("test.pdf".into());
        reader.pdf_has_text = true;
        reader.document = Some(document);
        reader.pdf_page_pt = Some((414.0, 639.0));
        reader.pdf_page_aspect.insert(0, 414.0 / 639.0);
        reader.dictionary = sepiola::config::ReaderDictionarySettings {
            url: "invalid".into(),
        };
        reader.pdf_rendered_dpi.insert(0, reader.pdf_dpi);
        let mut png = Vec::new();
        image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
            414,
            639,
            image::Rgba([255, 255, 255, 255]),
        ))
        .write_to(
            &mut std::io::Cursor::new(&mut png),
            image::ImageOutputFormat::Png,
        )
        .expect("encode png");
        reader.pdf_pages.insert(0, Some(Arc::new(png)));
        reader.pdf_text.insert(
            0,
            Some(Arc::new(PdfTextPage {
                width: 414.0,
                height: 639.0,
                lines: vec![sepiola::reader::pdf::PdfTextLine {
                    rect: (10.0, 20.0, 90.0, 30.0),
                    chars: "abcdefghij"
                        .chars()
                        .enumerate()
                        .map(|(index, c)| {
                            let left = 10.0 + index as f32 * 8.0;
                            sepiola::reader::pdf::PdfChar {
                                c,
                                rect: (left, 20.0, left + 6.0, 30.0),
                            }
                        })
                        .collect(),
                }],
            })),
        );
        let mut time = 0.0;
        let reader_viewport = egui::ViewportId::from_hash_of("reader_window");
        let mut frame = |app: &mut SepiolaApp, events: Vec<egui::Event>| {
            let mut input = egui::RawInput::default();
            input.screen_rect = Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(900.0, 720.0),
            ));
            input.viewport_id = reader_viewport;
            input.viewports.insert(
                reader_viewport,
                egui::ViewportInfo {
                    parent: Some(egui::ViewportId::ROOT),
                    native_pixels_per_point: Some(1.0),
                    ..Default::default()
                },
            );
            input.time = Some(time);
            input.events = events;
            time += 0.1;
            let mut output = ctx.run_ui(input, |ui| {
                egui::CentralPanel::default().show(ui, |ui| app.reader_ui(ui));
            });
            let texture = app.library.reader.image_textures[&(0, 0)]
                .as_ref()
                .expect("page texture")
                .id();
            let page_rect = output
                .shapes
                .iter()
                .find_map(|clipped| match &clipped.shape {
                    egui::Shape::Mesh(mesh) if mesh.texture_id == texture => {
                        Some(mesh.calc_bounds())
                    }
                    _ => None,
                })
                .expect("painted PDF page");
            let repaint_delay = output.viewport_output[&reader_viewport].repaint_delay;
            output.textures_delta.clear();
            (page_rect, repaint_delay)
        };
        for _ in 0..10 {
            frame(&mut app, Vec::new());
        }
        let (before, _) = frame(&mut app, Vec::new());
        let page_scale = before.width() / 414.0;
        let text_point = |index: usize| {
            egui::pos2(
                before.left() + (13.0 + index as f32 * 8.0) * page_scale,
                before.top() + 25.0 * page_scale,
            )
        };
        let press = |point| {
            vec![
                egui::Event::PointerMoved(point),
                egui::Event::PointerButton {
                    pos: point,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
            ]
        };
        frame(&mut app, press(text_point(1)));
        frame(&mut app, vec![egui::Event::PointerMoved(text_point(5))]);
        let (after_selection, _) = frame(
            &mut app,
            vec![egui::Event::PointerButton {
                pos: text_point(5),
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            }],
        );
        assert_eq!(after_selection, before);
        assert!(
            app.library.reader.selection.is_some(),
            "dragging over PDF text should create a toolbar selection"
        );
        let button = app.library.reader.toolbar_button_rects[5].center();
        let event = |pressed| egui::Event::PointerButton {
            pos: button,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        let (sender, receiver) = std::sync::mpsc::channel();
        let reader = &mut app.library.reader;
        reader.pdf_pending.insert(0, reader.pdf_dpi);
        reader.pdf_receiver = Some(receiver);
        let (_, repaint_delay) = frame(&mut app, Vec::new());
        assert!(
            repaint_delay <= std::time::Duration::from_millis(100),
            "pending PDF renders must be polled without another user interaction"
        );
        let dpi_before_toolbar = app.library.reader.pdf_dpi;
        app.library.reader.pdf_dpi_viewport = Some((0.0, 1.0));
        let (pressed, _) = frame(
            &mut app,
            vec![egui::Event::PointerMoved(button), event(true)],
        );
        assert_eq!(
            app.library.reader.pdf_dpi, dpi_before_toolbar,
            "PDF DPI must not change while a toolbar interaction is active"
        );
        let mut rerendered_png = Vec::new();
        image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
            414,
            640,
            image::Rgba([255, 255, 255, 255]),
        ))
        .write_to(
            &mut std::io::Cursor::new(&mut rerendered_png),
            image::ImageOutputFormat::Png,
        )
        .expect("encode rerendered png");
        sender
            .send(Ok(crate::reader::PdfPageRender {
                page: 0,
                dpi: app.library.reader.pdf_dpi,
                width: 414,
                height: 640,
                png: rerendered_png,
                text: None,
            }))
            .expect("send render result");
        app.poll_pdf_pages();
        assert!(
            app.library.reader.pdf_text[&0].is_some(),
            "a bitmap-only rerender must preserve the text layer"
        );
        assert_eq!(app.library.reader.pdf_page_aspect[&0], 414.0 / 639.0);
        let (released, _) = frame(&mut app, vec![event(false)]);
        assert!(
            app.library.reader.selection.is_none(),
            "dictionary button must be clicked"
        );
        assert_eq!(
            app.library.reader.pdf_dpi, dpi_before_toolbar,
            "clicking the toolbar must not trigger a PDF rerender"
        );
        assert!(app.library.reader.notice.starts_with("词典地址无效"));
        let (with_notice, _) = frame(&mut app, Vec::new());
        app.library.reader.notice_until = None;
        let (after_notice, _) = frame(&mut app, Vec::new());

        for rect in [pressed, released, with_notice, after_notice] {
            assert_eq!(
                rect, before,
                "PDF page geometry changed after toolbar click"
            );
        }
        app.library
            .reader
            .pdf_pending
            .insert(0, app.library.reader.pdf_dpi);
        drop(sender);
        app.poll_pdf_pages();
        assert!(app.library.reader.pdf_pending.is_empty());
        assert!(app.library.reader.pdf_receiver.is_none());
        assert_eq!(app.library.reader.notice, "PDF 页面渲染线程已退出");
    }

    #[test]
    fn pdf_page_allows_repeated_text_selection() {
        let mut png = Vec::new();
        image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
            100,
            100,
            image::Rgba([255, 255, 255, 255]),
        ))
        .write_to(
            &mut std::io::Cursor::new(&mut png),
            image::ImageOutputFormat::Png,
        )
        .expect("encode png");
        let data = Arc::new(png);
        let page = PdfTextPage {
            width: 100.0,
            height: 100.0,
            lines: vec![sepiola::reader::pdf::PdfTextLine {
                rect: (10.0, 20.0, 90.0, 30.0),
                chars: "abcdefghij"
                    .chars()
                    .enumerate()
                    .map(|(index, c)| {
                        let left = 10.0 + index as f32 * 8.0;
                        sepiola::reader::pdf::PdfChar {
                            c,
                            rect: (left, 20.0, left + 6.0, 30.0),
                        }
                    })
                    .collect(),
            }],
        };
        let ctx = egui::Context::default();
        let mut textures = HashMap::new();
        let mut select = None;
        let mut time = 0.0;
        let mut toolbar_active = false;
        let mut toolbar_button_rects = Vec::new();
        let mut run_frame = |events: Vec<egui::Event>| {
            let mut input = egui::RawInput::default();
            input.screen_rect = Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(600.0, 800.0),
            ));
            input.time = Some(time);
            input.events = events;
            time += 0.1;

            let mut selected_text = None;
            let mut page_geometry = (egui::Pos2::ZERO, 0.0);
            let mut copy_clicked = false;
            let mut copy_button_rect = None;
            let mut toolbar_frame_rect = None;
            let mut output = ctx.run_ui(input, |ui| {
                let page_origin = ui.cursor().min;
                let page_width = ui.available_width();
                let mut found = None;
                let mut deselected = false;
                pdf_page_block(
                    ui,
                    0,
                    PdfPageView {
                        data: Some(data.clone()),
                        dims: Some((100, 100)),
                        page_aspect: None,
                        aspect: 1.0,
                    },
                    &mut textures,
                    Some(&page),
                    &mut select,
                    &mut found,
                    &mut deselected,
                    &toolbar_button_rects,
                );
                page_geometry = (page_origin, page_width / page.width);
                selected_text = found.map(|selection: Selection| selection.text);
                if selected_text.is_some() {
                    toolbar_active = true;
                }

                if toolbar_active {
                    let mut next_toolbar_button_rects = Vec::new();
                    let scale = page_width / page.width;
                    let padding_target = egui::pos2(
                        page_origin.x + (13.0 + 6.0 * 8.0) * scale,
                        page_origin.y + 25.0 * scale,
                    );
                    let origin = padding_target - egui::vec2(2.0, 2.0);
                    let screen = ui.ctx().content_rect();
                    let mut toolbar = egui::Ui::new(
                        ctx.clone(),
                        egui::Id::unique("reader_selection_toolbar_reselection_test"),
                        egui::UiBuilder::new()
                            .max_rect(egui::Rect::from_min_max(origin, screen.max))
                            .layer_id(egui::LayerId::new(
                                egui::Order::Foreground,
                                egui::Id::unique("reader_selection_toolbar_reselection_test"),
                            )),
                    );
                    for (index, rect) in toolbar_button_rects.iter().copied().enumerate() {
                        toolbar.interact(
                            rect,
                            egui::Id::unique((
                                "reader_selection_toolbar_reselection_guard_test",
                                index,
                            )),
                            egui::Sense::click_and_drag(),
                        );
                    }
                    let response = egui::Frame::popup(toolbar.style()).show(&mut toolbar, |ui| {
                        let button = ui.button("复制");
                        copy_button_rect = Some(button.rect);
                        copy_clicked = button.clicked();
                        next_toolbar_button_rects.push(button.rect);
                    });
                    toolbar_frame_rect = Some(response.response.rect);
                    toolbar_button_rects = next_toolbar_button_rects;
                } else {
                    toolbar_button_rects.clear();
                    copy_button_rect = None;
                    toolbar_frame_rect = None;
                }
                if copy_clicked {
                    select = None;
                    toolbar_active = false;
                    toolbar_button_rects.clear();
                    toolbar_frame_rect = None;
                }
            });
            output.textures_delta.clear();
            (
                selected_text,
                page_geometry,
                copy_clicked,
                copy_button_rect,
                toolbar_frame_rect,
            )
        };

        let (_, (page_origin, scale), _, _, _) = run_frame(Vec::new());
        let point = |index: usize| {
            egui::pos2(
                page_origin.x + (13.0 + index as f32 * 8.0) * scale,
                page_origin.y + 25.0 * scale,
            )
        };
        let press = |start: egui::Pos2| {
            vec![
                egui::Event::PointerMoved(start),
                egui::Event::PointerButton {
                    pos: start,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
            ]
        };

        let first_start = point(1);
        let first_end = point(5);
        run_frame(press(first_start));
        run_frame(vec![egui::Event::PointerMoved(first_end)]);
        let (first_text, _, _, copy_button_rect, toolbar_frame_rect) =
            run_frame(vec![egui::Event::PointerButton {
                pos: first_end,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            }]);
        let second_start = point(6);
        let second_end = point(2);
        assert!(
            toolbar_frame_rect
                .expect("toolbar should be rendered")
                .contains(second_start),
            "second selection should begin in the toolbar's frame padding"
        );
        assert!(
            !copy_button_rect
                .expect("toolbar should expose the copy button")
                .contains(second_start),
            "frame padding must not count as a toolbar button"
        );
        run_frame(press(second_start));
        run_frame(vec![egui::Event::PointerMoved(second_end)]);
        let (second_text, _, _, copy_button_rect, _) =
            run_frame(vec![egui::Event::PointerButton {
                pos: second_end,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            }]);

        let copy_button = copy_button_rect
            .expect("toolbar should be rendered after the next selection")
            .center();
        run_frame(press(copy_button));
        let (_, _, clicked, _, _) = run_frame(vec![egui::Event::PointerButton {
            pos: copy_button,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        }]);
        assert!(clicked, "toolbar button should still receive its click");

        let third_start = point(1);
        let third_end = point(3);
        run_frame(press(third_start));
        run_frame(vec![egui::Event::PointerMoved(third_end)]);
        let (third_text, _, _, _, _) = run_frame(vec![egui::Event::PointerButton {
            pos: third_end,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        }]);

        assert_eq!(first_text.as_deref(), Some("bcdef"));
        assert_eq!(second_text.as_deref(), Some("cdefg"));
        assert_eq!(third_text.as_deref(), Some("bcd"));
    }

    #[test]
    fn adaptive_pdf_dpi_tracks_visible_physical_width() {
        assert_eq!(adaptive_pdf_dpi(800.0, 2.0, 612.0), 189);
    }

    #[test]
    fn adaptive_pdf_dpi_does_not_oversample() {
        let target = adaptive_pdf_dpi(800.0, 1.0, 612.0);
        assert_eq!(target, 96);
        assert!(target <= 96);
    }

    #[test]
    fn adaptive_pdf_dpi_clamps_to_supported_range() {
        assert_eq!(adaptive_pdf_dpi(50.0, 1.0, 612.0), 96);
        assert_eq!(adaptive_pdf_dpi(5000.0, 2.0, 612.0), 480);
    }

    #[test]
    fn visible_chapter_prefers_target_below_prepended_chapter() {
        // 回补上一章后，上一章块顶在可视区上方，目标章节块顶正好贴在可视区顶部。
        let tops = [(4, -820.0), (5, 300.0)];
        assert_eq!(visible_chapter(4, &tops, 300.0), 5);
    }

    #[test]
    fn visible_chapter_falls_back_to_window_start() {
        assert_eq!(visible_chapter(3, &[], 0.0), 3);
        // 首章块顶还在可视区下方时仍按窗口起点。
        assert_eq!(visible_chapter(3, &[(3, 40.0)], 0.0), 3);
    }

    #[test]
    fn visible_chapter_keeps_current_while_scrolled_inside() {
        let tops = [(0, -600.0), (1, -120.0), (2, 260.0)];
        assert_eq!(visible_chapter(0, &tops, 0.0), 1);
    }

    #[test]
    fn drops_chapters_far_above() {
        let map = heights(&[(0, 1000.0), (1, 1000.0), (2, 1000.0), (3, 1000.0)]);
        let plan = plan_window(4, 0, 3, 2600.0, 400.0, &map, 800.0, 20.0, true);
        assert_eq!(plan.start, 1);
        assert_eq!(plan.offset, 1580.0);
        assert_eq!(plan.end, 3);
        assert!(plan.prepended.is_none());
    }

    #[test]
    fn keeps_window_while_scrolled_inside_chapter() {
        let map = heights(&[(0, 1000.0), (1, 1000.0)]);
        let plan = plan_window(2, 0, 1, 900.0, 400.0, &map, 800.0, 20.0, true);
        assert_eq!(plan.start, 0);
        assert_eq!(plan.offset, 900.0);
    }

    #[test]
    fn prepends_previous_chapter_at_top() {
        let map = heights(&[(0, 500.0), (1, 500.0)]);
        let plan = plan_window(2, 1, 1, 0.0, 400.0, &map, 800.0, 20.0, true);
        assert_eq!(plan.start, 0);
        assert_eq!(plan.offset, 520.0);
        assert_eq!(plan.prepended, Some((0, 500.0)));
    }

    #[test]
    fn trims_chapters_far_below() {
        let map = heights(&[(0, 500.0), (1, 500.0), (2, 500.0), (3, 500.0)]);
        let plan = plan_window(4, 0, 3, 0.0, 400.0, &map, 800.0, 20.0, true);
        assert_eq!(plan.start, 0);
        assert_eq!(plan.end, 1);
        assert_eq!(plan.offset, 0.0);
    }

    #[test]
    fn unknown_heights_fall_back_to_estimate() {
        let map = HashMap::new();
        let plan = plan_window(5, 2, 4, 0.0, 400.0, &map, 800.0, 20.0, true);
        assert_eq!(plan.start, 1);
        assert_eq!(plan.offset, 820.0);
        assert_eq!(plan.end, 4);
    }

    #[test]
    fn skips_prepend_before_first_measurement() {
        let map = HashMap::new();
        let plan = plan_window(5, 2, 4, 0.0, 400.0, &map, 800.0, 20.0, false);
        assert_eq!(plan.start, 2);
        assert_eq!(plan.offset, 0.0);
        assert!(plan.prepended.is_none());
    }

    #[test]
    fn extends_window_when_jumping_to_short_chapter() {
        // 跳到第 2 章时窗口只有 [2]，且这一章很短；不向下补足的话内容总高度不够，
        // ScrollArea 会把滚动偏移夹小，跳章就停在原处。
        let map = heights(&[(1, 800.0), (2, 200.0), (3, 200.0), (4, 200.0), (5, 200.0)]);
        let plan = plan_window(6, 2, 2, 0.0, 400.0, &map, 800.0, 20.0, true);
        assert_eq!(plan.start, 1);
        assert!(plan.end > 2, "短章节要向下补足");
        let covered: f32 = (plan.start..=plan.end)
            .map(|index| map.get(&index).copied().unwrap_or(800.0) + 20.0)
            .sum();
        assert!(
            covered > plan.offset + 400.0 * 1.5,
            "内容高度要覆盖偏移之后至少一屏：{covered} vs {}",
            plan.offset
        );
    }

    #[test]
    fn estimate_matches_rendered_block_height() {
        let heading = "第一章 初见\n".to_owned();
        let body = "The quick brown fox jumps over the lazy dog. ".repeat(400);
        let bold_start = heading.len() + 4;
        let chapter = Chapter {
            title: "Chapter One".to_owned(),
            text: format!("{heading}{body}"),
            spans: vec![
                StyleSpan {
                    start: 0,
                    end: heading.trim_end().len(),
                    style: TextStyle {
                        bold: true,
                        heading: 1,
                        ..TextStyle::default()
                    },
                },
                StyleSpan {
                    start: bold_start,
                    end: bold_start + 3,
                    style: TextStyle {
                        bold: true,
                        ..TextStyle::default()
                    },
                },
            ],
            links: vec![LinkSpan {
                start: bold_start,
                end: bold_start + 5,
                target: LinkTarget::Footnote("note".to_owned()),
            }],
            images: Vec::new(),
        };
        let ctx = egui::Context::default();
        let mut input = egui::RawInput::default();
        input.screen_rect = Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(600.0, 800.0),
        ));
        let mut sizes: Option<(f32, f32)> = None;
        let annotations = vec![Annotation {
            chapter: 0,
            start: bold_start,
            end: bold_start + 3,
            kind: AnnotationKind::Expression,
            note: "标注".to_owned(),
            detail: String::new(),
        }];
        let mut output = ctx.run_ui(input, |ui| {
            let family = egui::FontFamily::Proportional;
            let width = ui.available_width();
            let estimate = measure_chapter_height(
                ui,
                &chapter,
                &annotations,
                16.0,
                family.clone(),
                1.0,
                12.0,
                width,
                None,
            );
            let top = ui.cursor().top();
            let mut textures = HashMap::new();
            chapter_block(
                ui,
                0,
                &chapter,
                &annotations,
                &[],
                16.0,
                family,
                1.0,
                12.0,
                &mut textures,
            );
            sizes = Some((estimate, ui.cursor().top() - top));
        });
        // 无渲染后端，纹理增量直接丢弃。
        output.textures_delta.clear();
        let (estimate, actual) = sizes.expect("block should be rendered");
        assert!(
            (estimate - actual).abs() <= 1.0,
            "预排版高度 {estimate} 与实际块高 {actual} 相差过大"
        );
    }

    #[test]
    fn estimate_matches_block_with_image() {
        let mut png = Vec::new();
        image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
            40,
            20,
            image::Rgba([200, 200, 200, 255]),
        ))
        .write_to(
            &mut std::io::Cursor::new(&mut png),
            image::ImageOutputFormat::Png,
        )
        .expect("encode png");
        let chapter = Chapter {
            title: "带图章节".to_owned(),
            text: "插图前\n插图后\n".to_owned(),
            images: vec![ChapterImage {
                at: "插图前\n".len(),
                width: Some(40),
                height: Some(20),
                data: std::sync::Arc::new(png),
                ..ChapterImage::default()
            }],
            ..Chapter::default()
        };
        let ctx = egui::Context::default();
        let mut input = egui::RawInput::default();
        input.screen_rect = Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(600.0, 800.0),
        ));
        let mut sizes: Option<(f32, f32)> = None;
        let mut output = ctx.run_ui(input, |ui| {
            let family = egui::FontFamily::Proportional;
            let width = ui.available_width();
            let estimate = measure_chapter_height(
                ui,
                &chapter,
                &[],
                16.0,
                family.clone(),
                1.0,
                0.0,
                width,
                None,
            );
            let top = ui.cursor().top();
            let mut textures = HashMap::new();
            chapter_block(
                ui,
                0,
                &chapter,
                &[],
                &[],
                16.0,
                family,
                1.0,
                0.0,
                &mut textures,
            );
            sizes = Some((estimate, ui.cursor().top() - top));
        });
        // 无渲染后端，纹理增量直接丢弃。
        output.textures_delta.clear();
        let (estimate, actual) = sizes.expect("block should be rendered");
        assert!(
            (estimate - actual).abs() <= 1.0,
            "带图章节预排版高度 {estimate} 与实际块高 {actual} 相差过大"
        );
    }

    #[test]
    fn estimate_matches_block_with_inline_image() {
        let mut png = Vec::new();
        image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
            24,
            60,
            image::Rgba([200, 200, 200, 255]),
        ))
        .write_to(
            &mut std::io::Cursor::new(&mut png),
            image::ImageOutputFormat::Png,
        )
        .expect("encode png");
        let chapter = Chapter {
            title: "行内图".to_owned(),
            text: "Before this inline picture there is a long sentence that has to keep \
                   flowing to the right of the picture and then wrap onto the following \
                   lines, so the height estimate must match the real rendered height."
                .to_owned(),
            images: vec![ChapterImage {
                at: "Before this inline picture".len(),
                width: Some(24),
                height: Some(60),
                inline: true,
                data: std::sync::Arc::new(png),
                ..ChapterImage::default()
            }],
            ..Chapter::default()
        };
        let ctx = egui::Context::default();
        let mut input = egui::RawInput::default();
        input.screen_rect = Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(600.0, 800.0),
        ));
        let mut sizes: Option<(f32, f32, usize)> = None;
        let mut output = ctx.run_ui(input, |ui| {
            let family = egui::FontFamily::Proportional;
            let width = ui.available_width();
            let estimate = measure_chapter_height(
                ui,
                &chapter,
                &[],
                16.0,
                family.clone(),
                1.0,
                0.0,
                width,
                None,
            );
            let top = ui.cursor().top();
            let mut textures = HashMap::new();
            let render = chapter_block(
                ui,
                0,
                &chapter,
                &[],
                &[],
                16.0,
                family,
                1.0,
                0.0,
                &mut textures,
            );
            sizes = Some((estimate, ui.cursor().top() - top, render.segments.len()));
        });
        output.textures_delta.clear();
        let (estimate, actual, segments) = sizes.expect("block should be rendered");
        assert!(
            (estimate - actual).abs() <= 1.0,
            "行内图预排版高度 {estimate} 与实际块高 {actual} 相差过大"
        );
        // 图片前、图片右侧续排、图片之后的剩余正文，共三段。
        assert_eq!(segments, 3);
    }

    #[test]
    fn consecutive_inline_images_stay_on_one_line() {
        let mut png = Vec::new();
        image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
            20,
            20,
            image::Rgba([200, 200, 200, 255]),
        ))
        .write_to(
            &mut std::io::Cursor::new(&mut png),
            image::ImageOutputFormat::Png,
        )
        .expect("encode png");
        let first = "One two three four five six seven eight nine ten ";
        let middle = "alpha beta ";
        let tail = "zeta eta theta iota kappa lambda mu nu xi omicron pi rho sigma tau upsilon \
                    phi chi psi omega keeps flowing past the line end and wraps below";
        let image_at = |at: usize| ChapterImage {
            at,
            width: Some(20),
            height: Some(20),
            inline: true,
            data: std::sync::Arc::new(png.clone()),
            ..ChapterImage::default()
        };
        let chapter = Chapter {
            title: "两幅行内图".to_owned(),
            text: format!("{first}{middle}{tail}"),
            images: vec![
                image_at(first.len()),
                image_at((first.to_owned() + middle).len()),
            ],
            ..Chapter::default()
        };
        let ctx = egui::Context::default();
        let mut input = egui::RawInput::default();
        input.screen_rect = Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(600.0, 800.0),
        ));
        let mut sizes: Option<(f32, f32, usize, f32, f32)> = None;
        let mut output = ctx.run_ui(input, |ui| {
            let family = egui::FontFamily::Proportional;
            let width = ui.available_width();
            let estimate = measure_chapter_height(
                ui,
                &chapter,
                &[],
                16.0,
                family.clone(),
                1.0,
                0.0,
                width,
                None,
            );
            let top = ui.cursor().top();
            let mut textures = HashMap::new();
            let render = chapter_block(
                ui,
                0,
                &chapter,
                &[],
                &[],
                16.0,
                family,
                1.0,
                0.0,
                &mut textures,
            );
            // 片段 1 是第一幅图右侧的续排，片段 2 是第二幅图右侧的续排，应在同一行。
            let y1 = render.segments[1].output.galley_pos.y;
            let y2 = render.segments[2].output.galley_pos.y;
            sizes = Some((
                estimate,
                ui.cursor().top() - top,
                render.segments.len(),
                y1,
                y2,
            ));
        });
        output.textures_delta.clear();
        let (estimate, actual, segments, y1, y2) = sizes.expect("block should be rendered");
        assert!(
            (estimate - actual).abs() <= 1.0,
            "连续行内图预排版高度 {estimate} 与实际块高 {actual} 相差过大"
        );
        // 图前正文、第一幅图右侧、第二幅图右侧、换行后的剩余正文，共四段。
        assert_eq!(segments, 4);
        assert!(
            (y1 - y2).abs() <= 1.0,
            "两幅行内图的续排应在同一行：{y1} vs {y2}"
        );
    }
}
