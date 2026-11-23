//! 电子书阅读解析层（M44）：EPUB / MOBI / AZW3 / TXT / PDF → 章节文本与样式区间。
//!
//! 解析全部使用纯 Rust 库，不调用 calibre、pdftotext 等外部程序；加密书籍、
//! 扫描版 PDF 等无法取出正文的情况返回带原因的错误，由界面提示。

mod epub;
mod markup;
mod mobi;
mod pdf;
mod text;

pub mod assist;
pub mod analyze;
pub mod analysis_store;
pub mod highlight;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// 一个阅读章节：EPUB 的一篇文档、TXT 的一章、PDF 的一页或一段。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Chapter {
    pub title: String,
    pub text: String,
    /// 正文的样式区间（UTF-8 字节范围），空表示整章为普通正文。
    #[serde(default)]
    pub spans: Vec<StyleSpan>,
    /// 正文中的内部链接：脚注与跨章节引用。
    #[serde(default)]
    pub links: Vec<LinkSpan>,
    /// 章节内的插图，按 `at` 升序排列。
    #[serde(default)]
    pub images: Vec<ChapterImage>,
}

/// 章节内的一幅插图：在正文中的插入位置 + 原始压缩字节。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ChapterImage {
    /// 插入位置（`Chapter::text` 的字节偏移，与样式区间同一坐标系）。
    pub at: usize,
    /// 图片原始像素宽度；无法读取时为空，渲染时按默认尺寸处理。
    pub width: Option<u32>,
    pub height: Option<u32>,
    /// XHTML 里声明的显示宽度（CSS 像素）；用于限制插图尺寸。
    #[serde(default)]
    pub declared_width: Option<f32>,
    #[serde(default)]
    pub declared_height: Option<f32>,
    /// 声明为正文宽度的百分比（`width:40%` → 0.4）。
    #[serde(default)]
    pub width_percent: Option<f32>,
    /// 行内插图：与正文同一行，不单独占一个段落。
    #[serde(default)]
    pub inline: bool,
    /// 图片原始字节（PNG/JPEG/…），渲染时按需解码，避免整本书提前占内存。
    #[serde(skip)]
    pub data: std::sync::Arc<Vec<u8>>,
}

/// 内部链接的目标。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum LinkTarget {
    /// 跳到书内另一章。
    Chapter(usize),
    /// 脚注：直接携带注释正文，点击时弹窗展示。
    Footnote(String),
}

/// `Chapter::text` 中的一段可点击区间，`start..end` 为字节偏移。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LinkSpan {
    pub start: usize,
    pub end: usize,
    pub target: LinkTarget,
}

/// 正文样式：粗体、斜体、等宽与标题级别（0 为正文，1..=6 对应 h1..h6）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextStyle {
    pub bold: bool,
    pub italic: bool,
    pub mono: bool,
    pub heading: u8,
}

impl TextStyle {
    pub fn is_plain(self) -> bool {
        self == Self::default()
    }
}

/// `Chapter::text` 中的一段样式区间，`start..end` 为字节偏移。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StyleSpan {
    pub start: usize,
    pub end: usize,
    pub style: TextStyle,
}

/// 解析后的整本书。
#[derive(Clone, Debug, Default)]
pub struct BookDocument {
    pub title: Option<String>,
    pub author: Option<String>,
    pub chapters: Vec<Chapter>,
}

impl BookDocument {
    /// 目录里可识别的章节号总数（如 105）；识别不到任何章节号时为 `None`。
    /// 封面、致谢、前言等前置内容不计入。
    pub fn numbered_total(&self) -> Option<usize> {
        let total = self
            .chapters
            .iter()
            .filter(|chapter| chapter_number(&chapter.title).is_some())
            .count();
        (total > 0).then_some(total)
    }

    /// 第 `index` 章在书籍目录里的章节进度：(章节号, 章节总数)。
    /// 标题无法识别章节号时返回 `None`，调用方回退到文档序号。
    pub fn chapter_progress(&self, index: usize) -> Option<(usize, usize)> {
        let number = chapter_number(&self.chapters.get(index)?.title)?;
        Some((number, self.numbered_total()?))
    }

    /// 按扩展名打开电子书；不支持的格式或取不到正文时返回错误信息。
    pub fn open(path: &Path) -> Result<Self, String> {
        let ext = path
            .extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| ext.to_ascii_lowercase())
            .ok_or_else(|| format!("无法识别文件扩展名：{}", path.display()))?;
        let document = match ext.as_str() {
            "epub" => epub::parse(path)?,
            "mobi" | "azw" | "azw3" | "asw3" => mobi::parse(path)?,
            "txt" => text::parse_file(path)?,
            "pdf" => pdf::parse(path)?,
            _ => return Err(format!("不支持的电子书格式：.{ext}")),
        };
        if document.chapters.iter().all(|chapter| chapter.text.trim().is_empty()) {
            return Err("未解析到正文（可能是加密书籍或纯图片扫描版）".to_string());
        }
        Ok(document)
    }
}

/// 阅读进度文件路径：与库索引同目录，不修改书籍本身。
pub fn reading_state_path(root: &Path) -> PathBuf {
    root.join(crate::library::LIBRARY_DIR).join("reading.json")
}

/// 从目录标题解析章节号：支持 `Chapter 55`、`CHAPTER 1`、`Chapter Fifteen`、`第 3 章` 等写法。
/// 标题里没有显式章节号时返回 `None`，由调用方按文档序号显示。
pub fn chapter_number(title: &str) -> Option<usize> {
    let title = title.trim();
    let lower = title.to_ascii_lowercase();
    if let Some(rest) = lower.strip_prefix("chapter") {
        let digits: String = rest
            .trim_start_matches(|ch: char| !ch.is_ascii_digit())
            .chars()
            .take_while(char::is_ascii_digit)
            .collect();
        if !digits.is_empty() {
            return digits.parse().ok();
        }
        return word_number(rest);
    }
    if let Some(rest) = title.strip_prefix('第') {
        let digits: String = rest
            .trim_start()
            .chars()
            .take_while(char::is_ascii_digit)
            .collect();
        if !digits.is_empty() && rest.trim_start().trim_start_matches(|ch: char| ch.is_ascii_digit() || ch.is_whitespace()).starts_with('章') {
            return digits.parse().ok();
        }
    }
    None
}

/// 解析英文数词：`one`…`ninety-nine`（含连字符写法 `twenty-one`）。
/// 仅接受标题中紧跟 `chapter` 的首个字母词，避免误伤其它英文词。
fn word_number(text: &str) -> Option<usize> {
    let text = text.trim_start();
    let token: String = text
        .chars()
        .take_while(|ch| ch.is_ascii_alphabetic() || *ch == '-')
        .collect();
    let token = token.trim_matches('-');
    if token.is_empty() {
        return None;
    }
    if let Some((tens, ones)) = token.split_once('-') {
        let tens = tens_number(tens)?;
        let ones = unit_number(ones)?;
        return Some(tens + ones);
    }
    unit_number(token)
        .or_else(|| teen_number(token))
        .or_else(|| tens_number(token))
}

fn unit_number(word: &str) -> Option<usize> {
    match word {
        "one" => Some(1),
        "two" => Some(2),
        "three" => Some(3),
        "four" => Some(4),
        "five" => Some(5),
        "six" => Some(6),
        "seven" => Some(7),
        "eight" => Some(8),
        "nine" => Some(9),
        _ => None,
    }
}

fn teen_number(word: &str) -> Option<usize> {
    match word {
        "ten" => Some(10),
        "eleven" => Some(11),
        "twelve" => Some(12),
        "thirteen" => Some(13),
        "fourteen" => Some(14),
        "fifteen" => Some(15),
        "sixteen" => Some(16),
        "seventeen" => Some(17),
        "eighteen" => Some(18),
        "nineteen" => Some(19),
        _ => None,
    }
}

fn tens_number(word: &str) -> Option<usize> {
    match word {
        "twenty" => Some(20),
        "thirty" => Some(30),
        "forty" => Some(40),
        "fifty" => Some(50),
        "sixty" => Some(60),
        "seventy" => Some(70),
        "eighty" => Some(80),
        "ninety" => Some(90),
        _ => None,
    }
}

fn default_line_spacing() -> f32 {
    1.6
}

fn default_margin() -> f32 {
    32.0
}

fn default_paragraph_spacing() -> f32 {
    0.0
}

/// 每本书的阅读位置（章节序号与上次阅读时间）。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ReadingPosition {
    #[serde(default)]
    pub chapter: usize,
    /// 上次阅读章节的目录标题；旧文件缺省为空，显示时回退到章节序号。
    #[serde(default)]
    pub title: String,
    /// 上次阅读的 Unix 时间戳（秒）；旧文件缺省为 0。
    #[serde(default)]
    pub opened_at: i64,
    /// 上次阅读在章节内的垂直滚动偏移（像素）；旧文件缺省为 0。
    #[serde(default)]
    pub offset: f32,
    /// 上次阅读位置在中线处的字符序号（章节内）。比像素偏移更能抵抗重排、
    /// 字号变化与短章节；旧文件缺省为 `None`，退回像素偏移。
    #[serde(default)]
    pub anchor: Option<usize>,
    /// 锚点所在的章节序号（视口顶部那一行文字所属的章）。`chapter` 用于显示
    /// （屏幕中线所在章），二者可能相差一章；旧文件缺省为 `None`，视为 `chapter`。
    #[serde(default)]
    pub position_chapter: Option<usize>,
}

/// 阅读页主题。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReaderTheme {
    /// 跟随应用的深/浅色设置。
    #[default]
    Follow,
    /// 纸白。
    Paper,
    /// 米黄。
    Sepia,
    /// 夜间。
    Night,
}

impl ReaderTheme {
    pub const ALL: [Self; 4] = [Self::Follow, Self::Paper, Self::Sepia, Self::Night];

    pub fn label(self) -> &'static str {
        match self {
            Self::Follow => "跟随",
            Self::Paper => "纸白",
            Self::Sepia => "米黄",
            Self::Night => "夜间",
        }
    }
}

/// 正文字体族。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReaderFont {
    /// 黑体（比例字体）。
    #[default]
    Sans,
    /// 宋体（衬线字体）。
    Serif,
    /// 楷体。
    Kai,
    /// 等宽字体。
    Mono,
}

impl ReaderFont {
    pub const ALL: [Self; 4] = [Self::Sans, Self::Serif, Self::Kai, Self::Mono];

    pub fn label(self) -> &'static str {
        match self {
            Self::Sans => "黑体",
            Self::Serif => "宋体",
            Self::Kai => "楷体",
            Self::Mono => "等宽",
        }
    }
}

/// 全局阅读设置与按书保存的进度。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ReadingState {
    #[serde(default)]
    pub font_size: f32,
    /// 阅读主题（M45）。
    #[serde(default)]
    pub theme: ReaderTheme,
    /// 正文字体族（M45）。
    #[serde(default)]
    pub font: ReaderFont,
    /// 系统字体文件路径；设置后覆盖 `font`。
    #[serde(default)]
    pub custom_font: Option<String>,
    /// 行间距倍数（1.0 为默认行高）。
    #[serde(default = "default_line_spacing")]
    pub line_spacing: f32,
    /// 正文页边距（像素）。
    #[serde(default = "default_margin")]
    pub margin: f32,
    /// 段落之间的垂直间距（像素）；旧文件缺省为 0。
    #[serde(default = "default_paragraph_spacing")]
    pub paragraph_spacing: f32,
    /// 自定义背景色（RGB），空表示跟随主题。
    #[serde(default)]
    pub background: Option<[u8; 3]>,
    /// 自定义文字颜色（RGB），空表示跟随主题。
    #[serde(default)]
    pub text_color: Option<[u8; 3]>,
    #[serde(default)]
    pub positions: BTreeMap<String, ReadingPosition>,
}

impl Default for ReadingState {
    fn default() -> Self {
        Self {
            font_size: 18.0,
            theme: ReaderTheme::default(),
            font: ReaderFont::default(),
            custom_font: None,
            line_spacing: default_line_spacing(),
            margin: default_margin(),
            paragraph_spacing: default_paragraph_spacing(),
            background: None,
            text_color: None,
            positions: BTreeMap::new(),
        }
    }
}

impl ReadingState {
    pub fn load(root: &Path) -> Self {
        std::fs::read_to_string(reading_state_path(root))
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, root: &Path) -> Result<(), String> {
        let path = reading_state_path(root);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| format!("创建 {} 失败：{error}", parent.display()))?;
        }
        let text = serde_json::to_string_pretty(self)
            .map_err(|error| format!("序列化阅读进度失败：{error}"))?;
        std::fs::write(&path, text)
            .map_err(|error| format!("写入 {} 失败：{error}", path.display()))
    }

    /// 字号（旧文件缺省或越界时回退到默认值）。
    pub fn readable_font_size(&self) -> f32 {
        if (10.0..=36.0).contains(&self.font_size) {
            self.font_size
        } else {
            18.0
        }
    }

    /// 行间距倍数（越界时回退默认值）。
    pub fn readable_line_spacing(&self) -> f32 {
        if (0.8..=3.0).contains(&self.line_spacing) {
            self.line_spacing
        } else {
            default_line_spacing()
        }
    }

    /// 页边距（越界时回退默认值）。
    pub fn readable_margin(&self) -> f32 {
        if (0.0..=200.0).contains(&self.margin) {
            self.margin
        } else {
            default_margin()
        }
    }

    /// 段落间距（越界时回退默认值）。
    pub fn readable_paragraph_spacing(&self) -> f32 {
        if (0.0..=48.0).contains(&self.paragraph_spacing) {
            self.paragraph_spacing
        } else {
            default_paragraph_spacing()
        }
    }

    pub fn position(&self, rel: &Path) -> usize {
        self.positions
            .get(&rel.to_string_lossy().replace('\\', "/"))
            .map(|position| position.chapter)
            .unwrap_or_default()
    }

    /// 上次阅读在章节内的滚动偏移（没有记录时为章节开头）。
    pub fn position_offset(&self, rel: &Path) -> f32 {
        self.positions
            .get(&rel.to_string_lossy().replace('\\', "/"))
            .map(|position| position.offset.max(0.0))
            .unwrap_or_default()
    }

    /// 上次阅读位置的字符锚点（章节内字符序号），没有记录时为 `None`。
    pub fn position_anchor(&self, rel: &Path) -> Option<usize> {
        self.positions
            .get(&rel.to_string_lossy().replace('\\', "/"))
            .and_then(|position| position.anchor)
    }

    /// 锚点所在章节（视口顶部那一行文字所属的章），没有记录时回退到显示章节。
    pub fn position_chapter(&self, rel: &Path) -> usize {
        let position = self
            .positions
            .get(&rel.to_string_lossy().replace('\\', "/"));
        match position {
            Some(position) => position.position_chapter.unwrap_or(position.chapter),
            None => 0,
        }
    }

    pub fn record(
        &mut self,
        rel: &Path,
        chapter: usize,
        title: &str,
        offset: f32,
        anchor: Option<usize>,
        position_chapter: Option<usize>,
    ) {
        let opened_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_secs() as i64)
            .unwrap_or_default();
        self.positions.insert(
            rel.to_string_lossy().replace('\\', "/"),
            ReadingPosition {
                chapter,
                title: title.to_string(),
                opened_at,
                offset: offset.max(0.0),
                anchor,
                position_chapter,
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!("sepiola-reader-{tag}-{nanos}"));
        std::fs::create_dir_all(&dir).expect("mkdir");
        dir
    }

    #[test]
    fn reading_state_round_trip() {
        let dir = temp_dir("state");
        let mut state = ReadingState::default();
        state.font_size = 22.0;
        state.theme = ReaderTheme::Sepia;
        state.font = ReaderFont::Mono;
        state.custom_font = Some("/tmp/font.ttf".to_string());
        state.line_spacing = 1.25;
        state.margin = 48.0;
        state.background = Some([12, 34, 56]);
        state.text_color = Some([240, 240, 230]);
        state.record(
            Path::new("novels/三体.txt"),
            7,
            "第七章",
            240.0,
            Some(1234),
            Some(6),
        );
        state.save(&dir).expect("save");
        let loaded = ReadingState::load(&dir);
        assert_eq!(loaded.font_size, 22.0);
        assert_eq!(loaded.theme, ReaderTheme::Sepia);
        assert_eq!(loaded.font, ReaderFont::Mono);
        assert_eq!(loaded.custom_font.as_deref(), Some("/tmp/font.ttf"));
        assert_eq!(loaded.line_spacing, 1.25);
        assert_eq!(loaded.margin, 48.0);
        assert_eq!(loaded.background, Some([12, 34, 56]));
        assert_eq!(loaded.text_color, Some([240, 240, 230]));
        assert_eq!(loaded.position(Path::new("novels/三体.txt")), 7);
        assert_eq!(loaded.position_offset(Path::new("novels/三体.txt")), 240.0);
        assert_eq!(loaded.position_anchor(Path::new("novels/三体.txt")), Some(1234));
        assert_eq!(loaded.position_chapter(Path::new("novels/三体.txt")), 6);
        assert_eq!(
            loaded.positions[&"novels/三体.txt".to_string()].title,
            "第七章"
        );
        assert_eq!(loaded.position(Path::new("other.txt")), 0);
        assert_eq!(loaded.position_offset(Path::new("other.txt")), 0.0);
        assert_eq!(loaded.position_anchor(Path::new("other.txt")), None);
        assert_eq!(loaded.position_chapter(Path::new("other.txt")), 0);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn parses_chapter_numbers_from_titles() {
        assert_eq!(chapter_number("Chapter 55"), Some(55));
        assert_eq!(chapter_number("CHAPTER 1"), Some(1));
        assert_eq!(chapter_number("Chapter 55, The Da Vinci Code"), Some(55));
        assert_eq!(chapter_number("Chapter Fifteen"), Some(15));
        assert_eq!(chapter_number("chapter ONE"), Some(1));
        assert_eq!(chapter_number("Chapter twenty-one: The Origin"), Some(21));
        assert_eq!(chapter_number("Chapters"), None);
        assert_eq!(chapter_number("第 3 章 初入江湖"), Some(3));
        assert_eq!(chapter_number("第12章"), Some(12));
        assert_eq!(chapter_number("Prologue"), None);
        assert_eq!(chapter_number("封面"), None);
    }

    #[test]
    fn reports_numbered_chapter_progress() {
        let document = BookDocument {
            chapters: ["Cover", "Prologue", "Chapter 1", "Chapter 2"]
                .iter()
                .map(|title| Chapter {
                    title: title.to_string(),
                    ..Chapter::default()
                })
                .collect(),
            ..BookDocument::default()
        };
        assert_eq!(document.numbered_total(), Some(2));
        assert_eq!(document.chapter_progress(2), Some((1, 2)));
        assert_eq!(document.chapter_progress(3), Some((2, 2)));
        assert_eq!(document.chapter_progress(0), None);
    }

    #[test]
    fn unsupported_extension_is_rejected() {
        let dir = temp_dir("ext");
        let error = BookDocument::open(&dir.join("a.djvu")).expect_err("unsupported");
        assert!(error.contains("不支持"));
        std::fs::remove_dir_all(&dir).ok();
    }
}
