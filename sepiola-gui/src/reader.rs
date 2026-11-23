//! 电子书阅读页（M44/M45）：后台解析、跨章连续滚动、选区工具条与阅读设置。

use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use eframe::egui;
use sepiola::config::{ReaderAiSettings, ReaderDictionarySettings, ReaderSettings};
use sepiola::notebook::ai::{self as notebook_ai, CardDraft};
use sepiola::reader::analysis_store::{self, BookAnalysis};
use sepiola::reader::highlight::{BookHighlights, Highlight};
use sepiola::reader::analyze::{
    self, Analysis, AnalysisItem, AnalysisProgress, Annotation, AnnotationKind,
};
use sepiola::reader::{BookDocument, ReaderFont, ReaderTheme, ReadingState};

use crate::assets::pointing_hand;
use crate::library::non_empty_root;
use crate::state::SepiolaApp;

/// 详情浮窗里的只读文本块：可以拖选，聚焦且有选区时返回正文同款工具条的选区信息。
pub(crate) fn selectable_text(
    ui: &mut egui::Ui,
    id: egui::Id,
    text: &str,
    font_id: egui::FontId,
    width: f32,
) -> Option<Selection> {
    let mut buffer: &str = text;
    let output = egui::TextEdit::multiline(&mut buffer)
        .id(id)
        .font(font_id)
        .frame(egui::Frame::NONE)
        .desired_rows(1)
        .desired_width(width)
        .show(ui);
    let range = output.cursor_range?;
    if !output.response.has_focus() {
        return None;
    }
    let first = range.primary.index.0.min(range.secondary.index.0);
    let last = range.primary.index.0.max(range.secondary.index.0);
    if last <= first {
        return None;
    }
    let picked: String = text.chars().skip(first).take(last - first).collect();
    if picked.trim().is_empty() {
        return None;
    }
    // 与正文一样：工具条贴在首末光标包围盒的上/下方。
    let head = output.galley.pos_from_cursor(range.primary);
    let tail = output.galley.pos_from_cursor(range.secondary);
    Some(Selection {
        text: picked,
        id,
        rect: head.union(tail).translate(output.galley_pos.to_vec2()),
        highlight: None,
    })
}

/// 加入卡片的一个条目：正面、已有 AI 内容（空串表示没有）与分类。
#[derive(Clone, Debug, Default)]
pub struct CardSeed {
    pub front: String,
    pub back: String,
    /// 分析给出的分类；沿用已有内容时写进卡片的标签。
    pub kind: Option<AnnotationKind>,
}

/// 加入卡片前的确认信息：条目已有 AI 生成的解释时，询问沿用还是重新生成。
#[derive(Clone, Debug, Default)]
pub struct PendingCards {
    pub seeds: Vec<CardSeed>,
}

/// 把 AI 生成的释义与详解拼成卡片背面。
fn annotation_back(note: &str, detail: &str) -> String {
    [note, detail]
        .into_iter()
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// 当前选中的文字（用于浮动工具条）。
pub struct Selection {
    pub text: String,
    /// 正文 `TextEdit` 的持久 Id，用于关闭工具条时清除选区。
    pub id: egui::Id,
    /// 选区在屏幕坐标系中的包围盒，占位工具条据此贴在选区上/下方。
    pub rect: egui::Rect,
    /// 选区在章节正文中的位置（章节序号, 起始字符, 结束字符），用于高亮。
    /// 弹窗里的文字没有章节位置，为 `None`。
    pub highlight: Option<(usize, usize, usize)>,
}

/// 翻译 / AI 解释的结果状态。
#[derive(Default)]
pub struct AssistState {
    pub open: bool,
    pub title: String,
    /// 发起请求时选中的原文，作为「放入笔记」的卡片正面。
    pub source: String,
    pub loading: bool,
    pub body: String,
    pub error: Option<String>,
    /// 最近一次请求的耗时与 token 用量。
    pub usage: Option<sepiola::ai::AiUsage>,
    pub receiver: Option<Receiver<Result<(String, sepiola::ai::AiUsage), String>>>,
    /// Markdown 渲染缓存（跨帧复用）。
    pub markdown_cache: egui_commonmark::CommonMarkCache,
    /// 上一帧弹窗的内容矩形，用于判断文字选区是否发生在弹窗里。
    pub rect: Option<egui::Rect>,
}

/// 阅读页状态；`rel` 为空表示尚未打开书籍。
pub struct ReaderState {
    pub rel: Option<PathBuf>,
    pub title: String,
    pub author: String,
    pub document: Option<Arc<BookDocument>>,
    pub chapter: usize,
    pub loading: bool,
    pub error: Option<String>,
    /// 解析结果，附带 PDF 的页面点尺寸（其它格式为 `None`）。
    pub receiver: Option<Receiver<Result<(BookDocument, Option<(f32, f32)>), String>>>,
    pub font_size: f32,
    pub theme: ReaderTheme,
    pub font: ReaderFont,
    /// 系统字体文件路径；设置后覆盖 `font`。
    pub custom_font: Option<String>,
    /// 行间距倍数（1.0 为默认行高）。
    pub line_spacing: f32,
    /// 正文左右页边距（像素）。
    pub margin: f32,
    /// 段落之间的垂直间距（像素）。
    pub paragraph_spacing: f32,
    /// 已安装到字体表里的自定义字体，避免每帧重建字体。
    pub applied_font: Option<String>,
    /// 阅读设置弹窗是否展开。
    pub settings_open: bool,
    /// 系统字体搜索框内容。
    pub font_query: String,
    /// 自定义背景/文字颜色（RGB）；空表示跟随主题。
    pub background: Option<[u8; 3]>,
    pub text_color: Option<[u8; 3]>,
    /// 正在查看的脚注内容（点击正文脚注标记后弹出）。
    pub footnote: Option<String>,
    pub toc_open: bool,
    /// 连续滚动窗口：当前渲染的章节区间与代次（代次变化会重置滚动位置）。
    pub stream_start: usize,
    pub stream_end: usize,
    pub stream_generation: u64,
    /// 已测得的章节块高度（px），供滑动窗口丢弃/回补章节时补偿滚动偏移。
    pub chapter_heights: HashMap<usize, f32>,
    /// 阅读区插图的纹理缓存（章节序号, 图片序号）；解码失败记 `None`，避免重复尝试。
    pub image_textures: HashMap<(usize, usize), Option<egui::TextureHandle>>,
    /// 上次布局的正文宽度；宽度变化会使高度缓存失效。
    pub stream_width: f32,
    /// 上一帧 ScrollArea 的内宽（用于回补前预排版，需与正文换行宽度一致）。
    pub stream_inner_width: f32,
    /// 章间分隔线的实测高度，丢弃/回补章节时一并计入。
    pub separator_height: f32,
    /// 上一帧的滚动偏移（内容坐标）。
    pub stream_offset: f32,
    /// 下一帧需要强制设置到 `ScrollArea` 的偏移（用于回补后的位置校正）。
    pub pending_offset: Option<f32>,
    pub selection: Option<Selection>,
    /// 上一帧工具条的屏幕矩形，用于判断点击是否落在工具条上。
    pub toolbar_rect: Option<egui::Rect>,
    /// 上一帧工具条的实际尺寸，用于让位给鼠标前先算出候选矩形。
    pub toolbar_size: Option<egui::Vec2>,
    pub assist: AssistState,
    /// 是否在正文显示 AI 分析彩标。
    pub show_annotations: bool,
    /// 本书的全部章节分析结果（一个书库文件，按章缓存）。
    pub book_analysis: BookAnalysis,
    /// 本书的用户高亮（与 `book_analysis` 同级的书库文件）。
    pub highlights: BookHighlights,
    /// 分析结果窗口是否展开。
    pub analysis_open: bool,
    /// 正在分析的章节；`None` 表示空闲。
    pub analysis_loading: Option<usize>,
    /// 分析结果窗口需要滚动到的章节（发起分析后定位用）。
    pub analysis_scroll_to: Option<usize>,
    pub analysis_error: Option<String>,
    pub analysis_receiver: Option<Receiver<Result<(usize, Analysis), String>>>,
    /// 分段分析的实时进度（后台线程写入，界面每帧读取）。
    pub analysis_progress: Option<Arc<Mutex<AnalysisProgress>>>,
    /// 本次分析的开始时间，用于界面显示实时耗时。
    pub analysis_started: Option<Instant>,
    /// 勾选的分析条目（章节序号，条目序号），用于批量加入笔记本。
    pub book_selected: BTreeSet<(usize, usize)>,
    /// 条目追问问答。
    pub item_chat: Option<ItemChat>,
    /// 待跳转的条目（章节序号，起始字节，结束字节）。
    pub pending_item: Option<(usize, usize, usize)>,
    /// 点击彩标后展示的条目。
    pub annotation_detail: Option<Annotation>,
    /// 「加入笔记本」后台任务（AI 去重 + 生成卡片）。
    pub cards_receiver: Option<Receiver<Result<(Vec<CardDraft>, Vec<String>), String>>>,
    pub cards_loading: bool,
    pub cards_error: Option<String>,
    /// 加入卡片前等待用户确认「沿用已有 AI 内容 / 重新生成」。
    pub pending_cards: Option<PendingCards>,
    /// 当前阅读位置在章节内的垂直偏移（像素），每帧由正文流更新。
    pub chapter_offset: f32,
    /// 最近一次落盘的位置：(锚点章节, 章内偏移, 字符锚点, 显示章节)。
    /// 用来判断滚动停稳后是否还需要再存一次。
    pub last_saved_position: Option<(usize, f32, Option<usize>, usize)>,
    /// 打开书籍时待恢复的章内偏移（解析完成后应用一次）。
    pub restore_offset: f32,
    /// 每帧更新的字符锚点（视口顶部那一行的首字符，章节内字符序号）。
    pub anchor_char: Option<usize>,
    /// 锚点所在章节（视口顶部那一行文字所属的章），与 `chapter`（显示用）可能差一章。
    pub position_chapter: usize,
    /// 打开书籍时待恢复的字符锚点；渲染出目标章节后滚动到该字符并清空。
    pub restore_anchor: Option<usize>,
    /// 打开书籍时待恢复的锚点章节；解析完成后用它作为窗口起点。
    pub restore_position_chapter: Option<usize>,
    /// 每章累计字符数（前缀和，长度 = 章节数 + 1），用于计算全书阅读进度。
    pub chapter_char_offsets: Vec<usize>,
    pub ai: ReaderAiSettings,
    /// 词典跳转模板（`[reader.dictionary] url`），支持 `https://` 与自定义 scheme。
    pub dictionary: ReaderDictionarySettings,
    pub notice: String,
    pub notice_until: Option<Instant>,
    /// PDF 页面渲染：书籍绝对路径（渲染工作线程在打开时启动）。
    pub pdf_path: Option<PathBuf>,
    /// PDF 渲染分辨率（按屏幕像素自适应）。
    pub pdf_dpi: u32,
    /// 已渲染页的 PNG 数据（`None` 表示该页渲染失败，不再重试）。
    pub pdf_pages: HashMap<usize, Option<Arc<Vec<u8>>>>,
    /// 各页像素尺寸（渲染成功后填充），用于计算显示尺寸。
    pub pdf_sizes: HashMap<usize, (u32, u32)>,
    /// 已发出渲染请求、尚未返回的页。
    pub pdf_pending: BTreeSet<usize>,
    /// 渲染请求通道（每本 PDF 一个后台工作线程）：(页, dpi, 是否需要文字层)。
    pub pdf_sender: Option<Sender<(usize, u32, bool)>>,
    /// 渲染结果通道。
    pub pdf_receiver: Option<Receiver<Result<PdfPageRender, (usize, String)>>>,
    /// 首个成功页学到的宽高比（宽/高），未渲染页占位时按它估算高度。
    pub pdf_aspect: Option<f32>,
    /// 页面尺寸（点，1/72 英寸），由首次渲染反推；用于按屏幕像素算渲染分辨率。
    pub pdf_page_pt: Option<(f32, f32)>,
    /// PDF 是否带文本层（扫描版为 false，只能页面渲染）。
    pub pdf_has_text: bool,
    /// 文本版 PDF 是否使用文本重排（默认关闭：按页渲染，保留版面且文字可选中）。
    pub pdf_text_mode: bool,
    /// 每页的文字层版面（字符矩形），用于在位图上做文字选区。
    pub pdf_text: HashMap<usize, Option<Arc<sepiola::reader::pdf::PdfTextPage>>>,
    /// 位图页面上的文字选区（字符序号区间）。
    pub pdf_select: Option<PdfSelect>,
}

/// 后台渲染完成的一页：PNG 与像素尺寸，附带该页文字层版面（与 dpi 无关）。
pub struct PdfPageRender {
    pub page: usize,
    pub dpi: u32,
    pub width: u32,
    pub height: u32,
    pub png: Vec<u8>,
    pub text: Option<sepiola::reader::pdf::PdfTextPage>,
}

/// 位图页面上的一次文字选区：起点与当前点都按「拼平字符序号」记录。
#[derive(Clone, Copy, Debug)]
pub struct PdfSelect {
    pub page: usize,
    pub anchor: usize,
    pub cursor: usize,
    /// 是否处于拖动中；双击选词时为 false，按住不动不会把选区缩成一个字符。
    pub dragging: bool,
}

impl Default for ReaderState {
    fn default() -> Self {
        Self {
            rel: None,
            title: String::new(),
            author: String::new(),
            document: None,
            chapter: 0,
            loading: false,
            error: None,
            receiver: None,
            font_size: 18.0,
            theme: ReaderTheme::default(),
            font: ReaderFont::default(),
            custom_font: None,
            line_spacing: 1.6,
            margin: 32.0,
            paragraph_spacing: 0.0,
            applied_font: None,
            settings_open: false,
            font_query: String::new(),
            background: None,
            text_color: None,
            footnote: None,
            toc_open: false,
            stream_start: 0,
            stream_end: 0,
            stream_generation: 0,
            chapter_heights: HashMap::new(),
            image_textures: HashMap::new(),
            stream_width: 0.0,
            stream_inner_width: 0.0,
            separator_height: 22.0,
            stream_offset: 0.0,
            pending_offset: None,
            selection: None,
            toolbar_rect: None,
            toolbar_size: None,
            assist: AssistState::default(),
            show_annotations: true,
            book_analysis: BookAnalysis::default(),
            highlights: BookHighlights::default(),
            analysis_open: false,
            analysis_loading: None,
            analysis_scroll_to: None,
            analysis_error: None,
            analysis_receiver: None,
            analysis_progress: None,
            analysis_started: None,
            book_selected: BTreeSet::new(),
            item_chat: None,
            pending_item: None,
            annotation_detail: None,
            cards_receiver: None,
            cards_loading: false,
            cards_error: None,
            pending_cards: None,
            chapter_offset: 0.0,
            last_saved_position: None,
            restore_offset: 0.0,
            anchor_char: None,
            position_chapter: 0,
            restore_anchor: None,
            restore_position_chapter: None,
            chapter_char_offsets: Vec::new(),
            ai: ReaderAiSettings::default(),
            dictionary: ReaderDictionarySettings::default(),
            notice: String::new(),
            notice_until: None,
            pdf_path: None,
            pdf_dpi: sepiola::reader::pdf::DEFAULT_DPI,
            pdf_pages: HashMap::new(),
            pdf_sizes: HashMap::new(),
            pdf_pending: BTreeSet::new(),
            pdf_sender: None,
            pdf_receiver: None,
            pdf_aspect: None,
            pdf_page_pt: None,
            pdf_has_text: false,
            pdf_text_mode: false,
            pdf_text: HashMap::new(),
            pdf_select: None,
        }
    }
}

impl ReaderState {
    /// 调整 PDF 渲染分辨率：清掉旧 dpi 的页面数据与纹理，触发重新渲染。
    /// 页面尺寸（点）与 dpi 无关，保留下来供下一次自适应计算。
    pub fn set_pdf_dpi(&mut self, dpi: u32) {
        let dpi = dpi.clamp(72, 300);
        if dpi == self.pdf_dpi {
            return;
        }
        self.pdf_dpi = dpi;
        self.pdf_pages.clear();
        self.pdf_sizes.clear();
        self.pdf_pending.clear();
        // 宽高比与页面点尺寸和 dpi 无关，版面高度也不变，无需清空。
        self.image_textures.clear();
    }

    /// 是否用页面位图显示：默认所有 PDF 都按页渲染（保留版面、文字可选中）；
    /// 带文本层的 PDF 可以切到文本重排（可调字号，但失去版式）。
    pub fn pdf_page_mode(&self) -> bool {
        self.pdf_path.is_some() && !(self.pdf_text_mode && self.pdf_has_text)
    }
}

/// 条目追问：保留最近若干轮对话。
#[derive(Default)]
pub struct ItemChat {
    pub chapter: usize,
    pub item: usize,
    pub messages: Vec<(bool, String)>,
    pub input: String,
    pub loading: bool,
    pub error: Option<String>,
    /// 最近一次回复的耗时与 token 用量。
    pub usage: Option<sepiola::ai::AiUsage>,
    pub receiver: Option<Receiver<Result<(String, sepiola::ai::AiUsage), String>>>,
}

impl SepiolaApp {
    /// 打开一本书：后台解析，界面先显示加载状态；沿用上次章节、字号与主题。
    pub fn open_book(&mut self, rel: PathBuf) {
        let Some(root) = non_empty_root(&self.library.root) else {
            self.library.message = "请先选择库目录".to_string();
            return;
        };
        self.save_reader_position();
        let reading = ReadingState::load(&root);
        let chapter = reading.position(&rel);
        let offset = reading.position_offset(&rel).max(0.0);
        let anchor = reading.position_anchor(&rel);
        // 锚点所在章节才是正文窗口的起点（显示章节可能比它晚一章）。
        let position_chapter = reading.position_chapter(&rel);
        let path = root.join(&rel);
        let display = rel
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_else(|| rel.display().to_string());
        let (sender, receiver) = channel();
        let open_path = path.clone();
        // PDF 的页面尺寸在解析线程里顺手读出来：首帧就按真实宽高比排版，
        // 避免第一页渲染完成时版面跳动导致阅读位置漂移。
        let is_pdf = path
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| ext.eq_ignore_ascii_case("pdf"));
        std::thread::spawn(move || {
            let size = if is_pdf {
                sepiola::reader::pdf::page_size(&open_path)
            } else {
                None
            };
            let _ = sender.send(BookDocument::open(&open_path).map(|document| (document, size)));
        });
        // PDF 走页面渲染模式：起一个后台工作线程，按需渲染单页 PNG。
        let pdf_cache_dir = root
            .join(sepiola::library::LIBRARY_DIR)
            .join("pdf_render");
        let (pdf_sender, pdf_receiver) = if is_pdf {
            Self::spawn_pdf_worker(path.clone(), pdf_cache_dir.clone())
        } else {
            (None, None)
        };
        let settings = self.reader_settings();
        // 打开书时读取该书已缓存的分析结果；正文变化后按指纹逐章失效。
        let book_analysis = analysis_store::load(&root, &rel);
        let highlights = sepiola::reader::highlight::load(&root, &rel);
        self.library.reader = ReaderState {
            rel: Some(rel),
            book_analysis,
            highlights,
            title: display,
            font_size: reading.readable_font_size(),
            theme: reading.theme,
            font: reading.font,
            custom_font: reading.custom_font.clone(),
            line_spacing: reading.readable_line_spacing(),
            margin: reading.readable_margin(),
            paragraph_spacing: reading.readable_paragraph_spacing(),
            background: reading.background,
            text_color: reading.text_color,
            restore_offset: offset,
            restore_anchor: anchor,
            restore_position_chapter: Some(position_chapter),
            anchor_char: anchor,
            position_chapter,
            chapter_offset: offset,
            last_saved_position: None,
            chapter,
            loading: true,
            receiver: Some(receiver),
            // 每本书用新的滚动代次，避免沿用上一本书的 ScrollArea 内部偏移。
            stream_generation: self.library.reader.stream_generation.wrapping_add(1),
            ai: settings.ai,
            dictionary: settings.dictionary,
            pdf_path: is_pdf.then(|| path.clone()),
            pdf_dpi: reading.readable_pdf_dpi(),
            pdf_text_mode: reading.pdf_text_mode,
            pdf_sender,
            pdf_receiver,
            ..ReaderState::default()
        };
        self.library.message = "正在解析书籍…".to_string();
        self.reader_window = true;
        // 立即记录打开时间，最近阅读列表无需等翻页或关闭书籍。
        self.save_reader_position();
    }

    /// PDF 页面渲染工作线程：串行处理渲染请求，避免并发拉起多个 mutool。
    fn spawn_pdf_worker(
        path: PathBuf,
        cache_dir: PathBuf,
    ) -> (
        Option<Sender<(usize, u32, bool)>>,
        Option<Receiver<Result<PdfPageRender, (usize, String)>>>,
    ) {
        let (req_tx, req_rx) = channel::<(usize, u32, bool)>();
        let (res_tx, res_rx) = channel();
        std::thread::spawn(move || {
            // Sender 被丢弃（换书）后循环退出。
            while let Ok((page, dpi, need_text)) = req_rx.recv() {
                let result = match sepiola::reader::pdf::render_page_png(
                    &path, page, dpi, &cache_dir,
                ) {
                    Ok(bytes) => match Self::png_dims(&bytes) {
                        Some((width, height)) => {
                            // 文字层与 dpi 无关；只在缺的时候取一次。
                            let text = need_text
                                .then(|| sepiola::reader::pdf::page_text_layout(&path, page).ok())
                                .flatten();
                            Ok(PdfPageRender {
                                page,
                                dpi,
                                width,
                                height,
                                png: bytes,
                                text,
                            })
                        }
                        None => Err((page, "页面图片尺寸读取失败".to_string())),
                    },
                    Err(error) => Err((page, error)),
                };
                if res_tx.send(result).is_err() {
                    break;
                }
            }
        });
        (Some(req_tx), Some(res_rx))
    }

    /// 读取 PNG 头部得到像素尺寸，不做完整解码。
    fn png_dims(bytes: &[u8]) -> Option<(u32, u32)> {
        image::io::Reader::new(std::io::Cursor::new(bytes))
            .with_guessed_format()
            .ok()?
            .into_dimensions()
            .ok()
    }

    /// 收取后台渲染完成的页面；换 dpi 后到达的过期结果直接丢弃。
    pub fn poll_pdf_pages(&mut self) {
        // 先取出接收端，处理完再放回，避免与状态可变借用冲突。
        let Some(receiver) = self.library.reader.pdf_receiver.take() else {
            return;
        };
        loop {
            match receiver.try_recv() {
                Ok(Ok(rendered)) => {
                    let PdfPageRender {
                        page,
                        dpi,
                        width,
                        height,
                        png,
                        text,
                    } = rendered;
                    let reader = &mut self.library.reader;
                    // 先清 pending，再判断是否过期：否则换 dpi 后旧结果被丢弃时
                    // pending 会永远留着，这一页就再也不会重新渲染。
                    let pending = reader.pdf_pending.remove(&page);
                    if dpi != reader.pdf_dpi || !pending {
                        continue;
                    }
                    // 每页都记下文字层结果：`None` 表示该页没有文字层，
                    // 避免每帧重复去取同一页。
                    reader.pdf_text.insert(page, text.map(Arc::new));
                    if reader.pdf_aspect.is_none() && width > 0 && height > 0 {
                        reader.pdf_aspect = Some(width as f32 / height as f32);
                    }
                    // 像素尺寸 ÷ dpi × 72 = 页面点尺寸；只学一次。
                    if reader.pdf_page_pt.is_none() && dpi > 0 && width > 0 && height > 0 {
                        let scale = 72.0 / dpi as f32;
                        reader.pdf_page_pt =
                            Some((width as f32 * scale, height as f32 * scale));
                    }
                    reader.pdf_sizes.insert(page, (width, height));
                    reader.pdf_pages.insert(page, Some(Arc::new(png)));
                }
                Ok(Err((page, _error))) => {
                    let reader = &mut self.library.reader;
                    reader.pdf_pending.remove(&page);
                    // 标记失败，避免每帧重试。
                    reader.pdf_pages.entry(page).or_insert(None);
                }
                Err(TryRecvError::Empty) | Err(TryRecvError::Disconnected) => break,
            }
        }
        self.library.reader.pdf_receiver = Some(receiver);
    }

    pub fn poll_reader(&mut self) {
        self.poll_pdf_pages();
        self.poll_assist();
        self.poll_analysis();
        self.poll_analysis_cards();
        self.poll_item_chat();
        let result = match self.library.reader.receiver.as_ref() {
            Some(receiver) => match receiver.try_recv() {
                Ok(result) => Some(result),
                Err(TryRecvError::Empty) => None,
                Err(TryRecvError::Disconnected) => {
                    Some(Err("书籍解析线程已退出".to_string()))
                }
            },
            None => None,
        };
        let Some(result) = result else { return };
        self.library.reader.receiver = None;
        self.library.reader.loading = false;
        match result {
            Ok((document, page_size)) => {
                let chapter = self
                    .library
                    .reader
                    .chapter
                    .min(document.chapters.len().saturating_sub(1));
                // PDF 是否带文本层：有文本时优先文本模式（可选中、可 AI 分析），
                // 扫描版只能页面渲染。
                if document.is_pdf {
                    self.library.reader.pdf_has_text = document
                        .chapters
                        .iter()
                        .any(|chapter| !chapter.text.trim().is_empty());
                    if let Some((width, height)) = page_size.filter(|(w, h)| *w > 0.0 && *h > 0.0) {
                        self.library.reader.pdf_page_pt = Some((width, height));
                        self.library.reader.pdf_aspect = Some(width / height);
                    }
                }
                if let Some(title) = document.title.clone().filter(|title| !title.trim().is_empty()) {
                    self.library.reader.title = title;
                }
                self.library.reader.author = document.author.clone().unwrap_or_default();
                // 章节字符前缀和，供全书进度条使用（随文档一次性计算）。
                let mut offsets = Vec::with_capacity(document.chapters.len() + 1);
                let mut total_chars = 0usize;
                offsets.push(0);
                for chapter in &document.chapters {
                    total_chars += chapter.text.chars().count();
                    offsets.push(total_chars);
                }
                self.library.reader.chapter_char_offsets = offsets;
                self.library.reader.chapter = chapter;
                // 正文窗口从锚点所在章开始（可能比显示章节早一章）。
                let position_chapter = self
                    .library
                    .reader
                    .restore_position_chapter
                    .take()
                    .unwrap_or(chapter)
                    .min(document.chapters.len().saturating_sub(1));
                self.library.reader.position_chapter = position_chapter;
                self.library.reader.stream_start = position_chapter;
                self.library.reader.stream_end = position_chapter;
                self.library.reader.chapter_heights.clear();
                self.library.reader.image_textures.clear();
                // 恢复到上次离开时的章内位置。优先用字符锚点（抗重排、短章节也准），
                // 没有锚点（旧数据）才用像素偏移。
                let offset = std::mem::take(&mut self.library.reader.restore_offset);
                let anchor = self.library.reader.restore_anchor.take();
                self.library.reader.stream_offset = offset;
                // 像素偏移作为首帧的初值（锚点滚动会在此基础上校正到行首）。
                self.library.reader.pending_offset = Some(offset);
                self.library.reader.restore_anchor = anchor;
                self.library.reader.chapter_offset = offset;
                self.library.message = format!(
                    "已打开《{}》：{} 章",
                    self.library.reader.title,
                    document.chapters.len()
                );
                self.library.reader.document = Some(Arc::new(document));
                self.library.reader.error = None;
                // 解析完成后立即回写一次进度，让「最近阅读」拿到章节标题，
                // 而不是等用户翻页才更新。
                self.save_reader_position();
            }
            Err(error) => {
                self.library.message = format!("打开书籍失败：{error}");
                self.library.reader.error = Some(error);
            }
        }
    }

    /// 分析当前章节（整章一次），结果按章缓存到本书的分析文件。
    pub fn start_analysis(&mut self) {
        self.start_chapter_analysis(self.library.reader.chapter);
    }

    /// 分析指定章节；无论是否已有缓存都会重新请求，用于手动刷新。
    pub fn start_chapter_analysis(&mut self, chapter: usize) {
        if self.library.reader.analysis_loading.is_some() {
            return;
        }
        let Some(document) = self.library.reader.document.clone() else {
            return;
        };
        if !self.library.reader.ai.enabled() {
            self.notify(sepiola::ai::MISSING_HINT);
            return;
        }
        let settings = self.library.reader.ai.clone();
        let progress = Arc::new(Mutex::new(AnalysisProgress {
            chapter,
            ..AnalysisProgress::default()
        }));
        let thread_progress = progress.clone();
        let (sender, receiver) = channel();
        std::thread::spawn(move || {
            let result = analyze::analyze_chapter_with_progress(
                &settings,
                &document,
                chapter,
                move |update| {
                    if let Ok(mut slot) = thread_progress.lock() {
                        *slot = update;
                    }
                },
            )
            .map(|analysis| (chapter, analysis));
            let _ = sender.send(result);
        });
        self.library.reader.analysis_loading = Some(chapter);
        self.library.reader.analysis_scroll_to = Some(chapter);
        self.library.reader.analysis_error = None;
        self.library.reader.analysis_receiver = Some(receiver);
        self.library.reader.analysis_progress = Some(progress);
        self.library.reader.analysis_started = Some(Instant::now());
        self.library.reader.analysis_open = true;
    }

    fn poll_analysis(&mut self) {
        let result = match self.library.reader.analysis_receiver.as_ref() {
            Some(receiver) => match receiver.try_recv() {
                Ok(result) => Some(result),
                Err(TryRecvError::Empty) => None,
                Err(TryRecvError::Disconnected) => {
                    Some(Err("分析线程已退出".to_string()))
                }
            },
            None => None,
        };
        let Some(result) = result else { return };
        self.library.reader.analysis_receiver = None;
        self.library.reader.analysis_loading = None;
        self.library.reader.analysis_progress = None;
        self.library.reader.analysis_started = None;
        match result {
            Ok((chapter, analysis)) => {
                let Some(document) = self.library.reader.document.clone() else {
                    return;
                };
                let Some(target) = document.chapters.get(chapter) else {
                    return;
                };
                let fingerprint = analysis_store::fingerprint(&target.text);
                let now = sepiola::notebook::now_unix();
                self.library.reader.book_selected.retain(|(index, _)| *index != chapter);
                for index in 0..analysis.items.len() {
                    self.library.reader.book_selected.insert((chapter, index));
                }
                self.library.reader.book_analysis.insert(
                    chapter,
                    fingerprint,
                    analysis,
                    now,
                );
                self.library.reader.show_annotations = true;
                self.library.reader.analysis_open = true;
                self.library.reader.analysis_error = None;
                self.save_book_analysis();
            }
            Err(error) => self.library.reader.analysis_error = Some(error),
        }
    }

    /// 保存整本书的分析结果；失败只在结果窗口提示，不打断阅读。
    fn save_book_analysis(&mut self) {
        let Some(root) = non_empty_root(&self.library.root) else {
            return;
        };
        let Some(rel) = self.library.reader.rel.clone() else {
            return;
        };
        let mut book = self.library.reader.book_analysis.clone();
        book.book = rel.display().to_string();
        book.title = self.library.reader.title.clone();
        if let Err(error) = analysis_store::save(&root, &rel, &book) {
            self.library.reader.analysis_error = Some(format!("保存分析结果失败：{error}"));
        }
    }

    /// 章节的缓存结果（正文指纹不一致时视为无缓存）。
    fn chapter_entry(&self, chapter: usize) -> Option<analysis_store::ChapterAnalysis> {
        let document = self.library.reader.document.as_ref()?;
        let text = &document.chapters.get(chapter)?.text;
        self.library
            .reader
            .book_analysis
            .entry(chapter, analysis_store::fingerprint(text))
            .cloned()
    }

    /// 把勾选的分析条目交给 AI 去重并生成卡片，完成后追加到笔记本。
    pub fn add_analysis_cards(&mut self) {
        if self.library.reader.cards_loading {
            return;
        }
        let Some(document) = self.library.reader.document.clone() else {
            return;
        };
        let chosen: Vec<(usize, usize)> =
            self.library.reader.book_selected.iter().copied().collect();
        let mut items = Vec::new();
        for (chapter, index) in chosen {
            let Some(target) = document.chapters.get(chapter) else {
                continue;
            };
            let fingerprint = analysis_store::fingerprint(&target.text);
            let Some(entry) = self
                .library
                .reader
                .book_analysis
                .entry(chapter, fingerprint)
            else {
                continue;
            };
            if let Some(item) = entry.items.get(index) {
                items.push(CardSeed {
                    front: item.text.clone(),
                    back: annotation_back(&item.note, &item.detail),
                    kind: Some(item.kind),
                });
            }
        }
        if items.is_empty() {
            self.notify("请先勾选要加入笔记本的条目");
            return;
        }
        self.request_add_cards(items);
    }

    /// 加入卡片入口：`items` 为卡片内容。只要有条目已生成过解释，
    /// 先弹窗让用户选择沿用还是重新生成；全部没有时直接走 AI 生成。
    pub fn request_add_cards(&mut self, items: Vec<CardSeed>) {
        if items.iter().any(|seed| !seed.back.trim().is_empty()) {
            self.library.reader.pending_cards = Some(PendingCards { seeds: items });
        } else {
            let texts = items.into_iter().map(|seed| seed.front).collect();
            self.add_cards_from_texts(texts);
        }
    }

    /// 加入卡片前的选择浮窗：沿用已有 AI 内容 / 重新生成 / 取消。
    pub fn pending_cards_window(&mut self, ctx: &egui::Context) {
        let Some(pending) = self.library.reader.pending_cards.clone() else {
            return;
        };
        let mut reuse = false;
        let mut regenerate = false;
        let mut cancel = false;
        egui::Window::new("加入卡片")
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.label(format!(
                    "这 {} 个条目已有 AI 生成的解释，如何生成卡片？",
                    pending.seeds.len()
                ));
                ui.horizontal(|ui| {
                    if ui.button("使用已有内容").clicked() {
                        reuse = true;
                    }
                    if ui.button("重新生成").clicked() {
                        regenerate = true;
                    }
                    if ui.button("取消").clicked() {
                        cancel = true;
                    }
                });
            });
        if !reuse && !regenerate && !cancel {
            return;
        }
        self.library.reader.pending_cards = None;
        if cancel {
            return;
        }
        if regenerate {
            self.add_cards_from_texts(pending.seeds.iter().map(|seed| seed.front.clone()).collect());
            return;
        }
        // 沿用已有内容：分类写进卡片标签；没有生成过解释的条目仍交给 AI 生成。
        let mut need_ai = Vec::new();
        for seed in &pending.seeds {
            if seed.back.trim().is_empty() {
                need_ai.push(seed.front.clone());
            } else {
                let tags = seed
                    .kind
                    .map(|kind| vec![kind.label().to_string()])
                    .unwrap_or_default();
                self.add_note_card(&seed.front, &seed.back, tags);
            }
        }
        if !need_ai.is_empty() {
            self.add_cards_from_texts(need_ai);
        }
    }

    /// 把若干词汇/表达加入笔记本：先程序去重，再交给 AI 语义去重并生成卡片。
    pub fn add_cards_from_texts(&mut self, texts: Vec<String>) {
        if self.library.reader.cards_loading {
            return;
        }
        let settings = self.library.reader.ai.clone();
        if !settings.enabled() {
            self.notify(sepiola::ai::MISSING_HINT);
            return;
        }
        let mut candidates = Vec::new();
        let mut duplicates = 0usize;
        for text in texts {
            if text.trim().is_empty() {
                continue;
            }
            if self.notebook.notebook.find_duplicate(&text).is_some() {
                duplicates += 1;
                continue;
            }
            candidates.push(text);
        }
        if candidates.is_empty() {
            self.notify(if duplicates > 0 {
                "该条目已在笔记本中"
            } else {
                "没有可加入的条目"
            });
            return;
        }
        let existing: Vec<String> = self
            .notebook
            .notebook
            .cards
            .iter()
            .map(|card| card.front.clone())
            .collect();
        let system = notebook_ai::card_system(&settings).to_string();
        let user = notebook_ai::card_user(&candidates, &existing);
        let (sender, receiver) = channel();
        std::thread::spawn(move || {
            let result = sepiola::ai::ask(&settings, &system, &user)
                .and_then(|reply| notebook_ai::parse_cards(&reply));
            let _ = sender.send(result);
        });
        self.library.reader.cards_loading = true;
        self.library.reader.cards_error = None;
        self.library.reader.cards_receiver = Some(receiver);
    }

    fn poll_analysis_cards(&mut self) {
        let result = match self.library.reader.cards_receiver.as_ref() {
            Some(receiver) => match receiver.try_recv() {
                Ok(result) => Some(result),
                Err(TryRecvError::Empty) => None,
                Err(TryRecvError::Disconnected) => {
                    Some(Err("卡片生成线程已退出".to_string()))
                }
            },
            None => None,
        };
        let Some(result) = result else { return };
        self.library.reader.cards_receiver = None;
        self.library.reader.cards_loading = false;
        match result {
            Ok((drafts, skipped)) => {
                let now = sepiola::notebook::now_unix();
                let mut added = 0usize;
                for draft in drafts {
                    if self.notebook.notebook.find_duplicate(&draft.front).is_some() {
                        continue;
                    }
                    self.notebook
                        .notebook
                        .add(&draft.front, &draft.back, draft.tags, now);
                    added += 1;
                }
                let mut message = format!("已加入 {added} 张卡片");
                if !skipped.is_empty() {
                    message.push_str(&format!("，AI 判定重复 {} 条", skipped.len()));
                }
                if added > 0 {
                    if let Some(root) = non_empty_root(&self.library.root) {
                        if let Err(error) = self.notebook.notebook.save(&root) {
                            self.library.reader.cards_error =
                                Some(format!("保存笔记本失败：{error}"));
                            return;
                        }
                        self.notebook.invalidate_stats();
                    }
                }
                self.library.reader.cards_error = None;
                self.notify(message);
            }
            Err(error) => {
                self.notify(format!("生成卡片失败：{error}"));
                self.library.reader.cards_error = Some(error);
            }
        }
    }

    /// 把 AI 结果直接作为一张卡片放入笔记本（`front` 为词条/问题，`back` 为回复正文，
    /// `tags` 为分类等标签）。
    pub fn add_note_card(&mut self, front: &str, back: &str, tags: Vec<String>) {
        let front = front.trim();
        let back = back.trim();
        if front.is_empty() || back.is_empty() {
            self.notify("没有可放入笔记的内容");
            return;
        }
        if self.notebook.notebook.find_duplicate(front).is_some() {
            self.notify("该条目已在笔记本中");
            return;
        }
        let now = sepiola::notebook::now_unix();
        self.notebook.notebook.add(front, back, tags, now);
        if let Some(root) = non_empty_root(&self.library.root) {
            if let Err(error) = self.notebook.notebook.save(&root) {
                self.notify(format!("保存笔记本失败：{error}"));
                return;
            }
            self.notebook.invalidate_stats();
        }
        self.notify("已放入笔记本");
    }

    /// 全书分析结果窗口：按章查看缓存、跳转原文、勾选加入笔记本、逐条追问。
    pub fn analysis_window(&mut self, ctx: &egui::Context) {
        if !self.library.reader.analysis_open {
            return;
        }
        let Some(document) = self.library.reader.document.clone() else {
            return;
        };
        let loading = self.library.reader.analysis_loading;
        let error = self.library.reader.analysis_error.clone();
        let cards_loading = self.library.reader.cards_loading;
        let cards_error = self.library.reader.cards_error.clone();
        let mut show_annotations = self.library.reader.show_annotations;
        let mut selected = self.library.reader.book_selected.clone();
        let cached: Vec<Option<analysis_store::ChapterAnalysis>> = (0..document.chapters.len())
            .map(|index| self.chapter_entry(index))
            .collect();
        let cached_count = cached.iter().filter(|entry| entry.is_some()).count();
        let progress = self
            .library
            .reader
            .analysis_progress
            .as_ref()
            .and_then(|progress| progress.lock().ok().map(|slot| slot.clone()));
        let elapsed = self
            .library
            .reader
            .analysis_started
            .map(|started| started.elapsed().as_secs_f64())
            .unwrap_or_default();
        let current = self.library.reader.chapter;
        let mut scroll_to = self.library.reader.analysis_scroll_to;
        let mut open = true;
        let mut close = false;
        let mut analyze_chapter: Option<usize> = None;
        let mut clear_chapter: Option<usize> = None;
        let mut jump: Option<(usize, usize, usize)> = None;
        let mut chat: Option<(usize, usize)> = None;
        let mut add_cards = false;

        egui::Window::new("AI 分析结果")
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .default_width(520.0)
            .default_height(560.0)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.checkbox(&mut show_annotations, "正文彩标");
                    let analyze_label = match document.chapter_progress(current) {
                        Some((number, _)) => format!("分析第 {number} 章"),
                        None => format!("分析第 {} 节", current + 1),
                    };
                    if ui.button(analyze_label).clicked() {
                        analyze_chapter = Some(current);
                    }
                    if loading.is_some() {
                        ui.spinner();
                    }
                    ui.weak(format!("已缓存 {cached_count} 章"));
                });
                if loading.is_some() {
                    if let Some(progress) = &progress {
                        let fraction = if progress.chunks > 0 {
                            progress.chunk as f32 / progress.chunks as f32
                        } else {
                            0.0
                        };
                        let stage = match progress.stage {
                            analyze::AnalysisStage::Requesting => "请求中",
                            analyze::AnalysisStage::Done => "完成",
                            analyze::AnalysisStage::Failed => "失败",
                        };
                        let tokens =
                            (progress.sent_chars + progress.received_chars + 3) / 4;
                        let text = format!(
                            "{stage} · 第 {}/{} 段 · {elapsed:.0}s · 发送 {:.1}k 字 · 返回 {:.1}k 字 · ≈{tokens} tokens",
                            progress.chunk,
                            progress.chunks,
                            progress.sent_chars as f64 / 1000.0,
                            progress.received_chars as f64 / 1000.0,
                        );
                        ui.add(
                            egui::ProgressBar::new(fraction)
                                .text(text)
                                .desired_width(ui.available_width()),
                        );
                    }
                }
                ui.label(
                    egui::RichText::new(
                        "每章一条结果，保存在书库 .sepiola/analysis 下；不手动刷新时直接读取缓存。",
                    )
                    .small()
                    .weak(),
                );
                if let Some(error) = &error {
                    ui.colored_label(egui::Color32::from_rgb(200, 70, 70), error.as_str());
                }
                ui.separator();
                egui::ScrollArea::vertical()
                    .id_salt("book_analysis_scroll")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        for (index, chapter) in document.chapters.iter().enumerate() {
                            let entry = cached.get(index).and_then(|entry| entry.as_ref());
                            let status = match entry {
                                Some(entry) => {
                                    if entry.stats.chunks > 0 {
                                        format!(
                                            "{} 条 · {} 段 · {:.0}s · ≈{} tokens",
                                            entry.items.len(),
                                            entry.stats.chunks,
                                            entry.stats.elapsed_secs,
                                            entry.stats.tokens
                                        )
                                    } else {
                                        format!("{} 条", entry.items.len())
                                    }
                                }
                                None if loading == Some(index) => "分析中…".to_string(),
                                None => "未分析".to_string(),
                            };
                            // 发起分析后自动滚动到对应章节并展开。
                            let header = match document.chapter_progress(index) {
                                Some(_) => format!("{}  ·  {status}", chapter.title),
                                None => {
                                    format!("第 {} 节  {}  ·  {status}", index + 1, chapter.title)
                                }
                            };
                            let response = egui::CollapsingHeader::new(header)
                            .open((scroll_to == Some(index)).then_some(true))
                            .id_salt(("book_analysis_chapter", index))
                            .default_open(index == current && entry.is_some())
                            .show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    if ui.button("分析本章").clicked() {
                                        analyze_chapter = Some(index);
                                    }
                                    if entry.is_some() && ui.button("清除缓存").clicked() {
                                        clear_chapter = Some(index);
                                    }
                                });
                                let Some(entry) = entry else {
                                    ui.weak("尚未分析，点击上方按钮生成。");
                                    return;
                                };
                                if !entry.summary.is_empty() {
                                    ui.label(egui::RichText::new(&entry.summary).strong());
                                    ui.add_space(4.0);
                                }
                                if entry.stats.chunks > 0 {
                                    ui.weak(format!(
                                        "上次分析：{} 段 · {:.0}s · 发送 {} 字 / 返回 {} 字 · ≈{} tokens",
                                        entry.stats.chunks,
                                        entry.stats.elapsed_secs,
                                        entry.stats.sent_chars,
                                        entry.stats.received_chars,
                                        entry.stats.tokens
                                    ));
                                    ui.add_space(4.0);
                                }
                                for kind in analyze::AnnotationKind::ALL {
                                    let group: Vec<(usize, &AnalysisItem)> = entry
                                        .items
                                        .iter()
                                        .enumerate()
                                        .filter(|(_, item)| item.kind == kind)
                                        .collect();
                                    if group.is_empty() {
                                        continue;
                                    }
                                    ui.label(
                                        egui::RichText::new(kind.label())
                                            .color(crate::reader_stream::annotation_color(kind))
                                            .strong(),
                                    );
                                    for (item_index, item) in group {
                                        ui.horizontal(|ui| {
                                            let key = (index, item_index);
                                            let mut checked = selected.contains(&key);
                                            if ui.checkbox(&mut checked, "").changed() {
                                                if checked {
                                                    selected.insert(key);
                                                } else {
                                                    selected.remove(&key);
                                                }
                                            }
                                            let color =
                                                crate::reader_stream::annotation_color(kind);
                                            let response = ui.add(
                                                egui::Label::new(
                                                    egui::RichText::new(&item.text).color(color),
                                                )
                                                .sense(egui::Sense::click()),
                                            );
                                            if crate::assets::pointing_hand(response).clicked() {
                                                let span = entry
                                                    .annotations
                                                    .iter()
                                                    .find(|annotation| {
                                                        annotation.kind == item.kind
                                                            && chapter.text.get(
                                                                annotation.start..annotation.end,
                                                            ) == Some(item.text.as_str())
                                                    })
                                                    .map(|annotation| {
                                                        (annotation.start, annotation.end)
                                                    })
                                                    .or_else(|| {
                                                        chapter.text.find(&item.text).map(|start| {
                                                            (start, start + item.text.len())
                                                        })
                                                    });
                                                if let Some((start, end)) = span {
                                                    jump = Some((index, start, end));
                                                }
                                            }
                                            if ui.button("追问").clicked() {
                                                chat = Some((index, item_index));
                                            }
                                        });
                                        if !item.note.is_empty() {
                                            ui.label(&item.note);
                                        }
                                        if !item.detail.is_empty() {
                                            ui.label(
                                                egui::RichText::new(&item.detail).small().weak(),
                                            );
                                        }
                                        ui.add_space(6.0);
                                    }
                                }
                                if entry.items.is_empty() {
                                    ui.weak("本章没有值得标记的语言点");
                                }
                            });
                            if scroll_to == Some(index) {
                                response
                                    .header_response
                                    .scroll_to_me(Some(egui::Align::TOP));
                                scroll_to = None;
                            }
                        }
                    });
                ui.separator();
                ui.horizontal(|ui| {
                    let label = if cards_loading {
                        "生成中…"
                    } else {
                        "加入笔记本"
                    };
                    if ui
                        .add_enabled(
                            !cards_loading && !selected.is_empty(),
                            egui::Button::new(label),
                        )
                        .clicked()
                    {
                        add_cards = true;
                    }
                    if ui.button("全选").clicked() {
                        for (chapter, entry) in cached.iter().enumerate() {
                            if let Some(entry) = entry {
                                for index in 0..entry.items.len() {
                                    selected.insert((chapter, index));
                                }
                            }
                        }
                    }
                    if ui.button("全不选").clicked() {
                        selected.clear();
                    }
                    ui.weak(format!("已选 {} 条", selected.len()));
                });
                if let Some(error) = &cards_error {
                    ui.colored_label(egui::Color32::from_rgb(200, 70, 70), error.as_str());
                }
                if ui.button("关闭").clicked() {
                    close = true;
                }
            });

        self.library.reader.show_annotations = show_annotations;
        self.library.reader.book_selected = selected;
        self.library.reader.analysis_scroll_to = scroll_to;
        if let Some(index) = analyze_chapter {
            self.start_chapter_analysis(index);
        }
        if let Some(index) = clear_chapter {
            self.library.reader.book_analysis.remove(index);
            self.library
                .reader
                .book_selected
                .retain(|(chapter, _)| *chapter != index);
            self.save_book_analysis();
        }
        if let Some((chapter, start, end)) = jump {
            self.jump_to_item(chapter, start, end);
        }
        if let Some((chapter, item)) = chat {
            self.open_item_chat(chapter, item);
        }
        if add_cards {
            self.add_analysis_cards();
        }
        if !open || close {
            self.library.reader.analysis_open = false;
        }
    }

    /// 跳到条目对应原文：切到该章并把正文选区定位到条目。
    fn jump_to_item(&mut self, chapter: usize, start: usize, end: usize) {
        self.goto_chapter(chapter);
        self.library.reader.pending_item = Some((chapter, start, end));
    }

    /// 打开某条目的追问窗口；同一本书内按需保留历史。
    pub fn open_item_chat(&mut self, chapter: usize, item: usize) {
        if let Some(chat) = &self.library.reader.item_chat {
            if chat.chapter == chapter && chat.item == item {
                return;
            }
        }
        self.library.reader.item_chat = Some(ItemChat {
            chapter,
            item,
            ..ItemChat::default()
        });
    }

    /// 发送追问：带上条目解释与最近几轮对话。
    pub fn send_item_chat(&mut self) {
        let Some((chapter, item_index, loading, input)) =
            self.library.reader.item_chat.as_ref().map(|chat| {
                (
                    chat.chapter,
                    chat.item,
                    chat.loading,
                    chat.input.clone(),
                )
            })
        else {
            return;
        };
        if loading {
            return;
        }
        let question = input.trim().to_string();
        if question.is_empty() {
            return;
        }
        let settings = self.library.reader.ai.clone();
        if !settings.enabled() {
            self.notify(sepiola::ai::MISSING_HINT);
            return;
        }
        let Some(entry) = self.chapter_entry(chapter) else {
            return;
        };
        let Some(item) = entry.items.get(item_index) else {
            return;
        };
        let chapter_title = self
            .library
            .reader
            .document
            .as_ref()
            .and_then(|document| document.chapters.get(chapter))
            .map(|chapter| chapter.title.clone())
            .unwrap_or_default();
        let history: Vec<(bool, String)> = self
            .library
            .reader
            .item_chat
            .as_ref()
            .map(|chat| {
                chat.messages
                    .iter()
                    .rev()
                    .take(6)
                    .rev()
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        let mut user = format!(
            "书籍：{}\n章节：{}\n条目：{}\n",
            self.library.reader.title, chapter_title, item.text
        );
        if !item.note.is_empty() {
            user.push_str(&format!("已有解释：{}\n", item.note));
        }
        if !item.detail.is_empty() {
            user.push_str(&format!("扩展讲解：{}\n", item.detail));
        }
        if !history.is_empty() {
            user.push_str("最近对话：\n");
            for (is_user, text) in &history {
                user.push_str(&format!("{}：{}\n", if *is_user { "用户" } else { "助手" }, text));
            }
        }
        user.push_str(&format!("用户问题：{question}"));
        let system = analyze::chat_system(&settings).to_string();
        let (sender, receiver) = channel();
        std::thread::spawn(move || {
            let _ = sender.send(sepiola::ai::ask_with_usage(&settings, &system, &user));
        });
        if let Some(chat) = self.library.reader.item_chat.as_mut() {
            chat.messages.push((true, question));
            chat.input.clear();
            chat.loading = true;
            chat.error = None;
            chat.receiver = Some(receiver);
        }
    }

    fn poll_item_chat(&mut self) {
        let Some(chat) = self.library.reader.item_chat.as_mut() else {
            return;
        };
        let result = match chat.receiver.as_ref() {
            Some(receiver) => match receiver.try_recv() {
                Ok(result) => Some(result),
                Err(TryRecvError::Empty) => None,
                Err(TryRecvError::Disconnected) => {
                    Some(Err("追问线程已退出".to_string()))
                }
            },
            None => None,
        };
        let Some(result) = result else { return };
        chat.receiver = None;
        chat.loading = false;
        match result {
            Ok((reply, usage)) => {
                chat.messages.push((false, reply.trim().to_string()));
                chat.usage = Some(usage);
            }
            Err(error) => chat.error = Some(error),
        }
    }

    /// 条目追问窗口：上下文 + 对话历史 + 输入框。
    pub fn item_chat_window(&mut self, ctx: &egui::Context) {
        let Some((chapter, item_index)) = self
            .library
            .reader
            .item_chat
            .as_ref()
            .map(|chat| (chat.chapter, chat.item))
        else {
            return;
        };
        let Some(entry) = self.chapter_entry(chapter) else {
            return;
        };
        let Some(item) = entry.items.get(item_index).cloned() else {
            return;
        };
        let mut open = true;
        let mut close = false;
        let mut send = false;
        let mut add_card = false;
        let mut note_reply: Option<String> = None;
        let cards_loading = self.library.reader.cards_loading;
        egui::Window::new("追问")
            .open(&mut open)
            .collapsible(false)
            .default_width(460.0)
            .show(ctx, |ui| {
                ui.colored_label(
                    crate::reader_stream::annotation_color(item.kind),
                    item.kind.label(),
                );
                ui.label(egui::RichText::new(&item.text).size(20.0).strong());
                if !item.note.is_empty() {
                    ui.label(&item.note);
                }
                if !item.detail.is_empty() {
                    ui.label(egui::RichText::new(&item.detail).small().weak());
                }
                if ui
                    .add_enabled(
                        !cards_loading,
                        egui::Button::new(if cards_loading {
                            "生成卡片中…"
                        } else {
                            "加入卡片"
                        }),
                    )
                    .clicked()
                {
                    add_card = true;
                }
                ui.separator();
                let Some(chat) = self.library.reader.item_chat.as_mut() else {
                    return;
                };
                egui::ScrollArea::vertical()
                    .id_salt("item_chat_scroll")
                    .max_height(280.0)
                    .stick_to_bottom(true)
                    .show(ui, |ui| {
                        if chat.messages.is_empty() && !chat.loading {
                            ui.weak("就这个条目继续提问，例如搭配、语法、近义词或背景知识。");
                        }
                        for (is_user, text) in &chat.messages {
                            ui.horizontal_wrapped(|ui| {
                                ui.weak(if *is_user { "你" } else { "AI" });
                                ui.label(text);
                            });
                        }
                        if chat.loading {
                            ui.horizontal(|ui| {
                                ui.spinner();
                                ui.weak("思考中…");
                            });
                        }
                        if let Some(error) = &chat.error {
                            ui.colored_label(egui::Color32::from_rgb(200, 70, 70), error.as_str());
                        }
                    });
                if let Some(usage) = &chat.usage {
                    ui.label(egui::RichText::new(usage.summary()).small().weak());
                }
                // 最后一条 AI 回复可直接放入笔记本。
                if let Some((_, reply)) = chat.messages.iter().rev().find(|(is_user, _)| !*is_user)
                {
                    if ui.button("放入笔记").clicked() {
                        note_reply = Some(reply.clone());
                    }
                }
                ui.separator();
                let response = ui.add(
                    egui::TextEdit::multiline(&mut chat.input)
                        .hint_text("就这个条目继续提问…")
                        .desired_rows(2)
                        .desired_width(f32::INFINITY),
                );
                if response.has_focus()
                    && ui.input(|input| {
                        input.key_pressed(egui::Key::Enter) && !input.modifiers.shift
                    })
                {
                    send = true;
                }
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(
                            !chat.loading && !chat.input.trim().is_empty(),
                            egui::Button::new("发送"),
                        )
                        .clicked()
                    {
                        send = true;
                    }
                    if ui.button("关闭").clicked() {
                        close = true;
                    }
                });
            });
        if send {
            self.send_item_chat();
        }
        if add_card {
            let back = annotation_back(&item.note, &item.detail);
            self.request_add_cards(vec![CardSeed {
                front: item.text.clone(),
                back,
                kind: Some(item.kind),
            }]);
        }
        if let Some(reply) = note_reply {
            let tags = vec![item.kind.label().to_string()];
            self.add_note_card(&item.text, &reply, tags);
        }
        if close || !open {
            self.library.reader.item_chat = None;
        }
    }

    /// 点击正文彩标或分析条目后的详情浮窗。
    pub fn annotation_window(&mut self, ctx: &egui::Context) {
        let Some(annotation) = self.library.reader.annotation_detail.clone() else {
            return;
        };
        let original = self
            .library
            .reader
            .document
            .as_ref()
            .and_then(|document| document.chapters.get(annotation.chapter))
            .and_then(|chapter| chapter.text.get(annotation.start..annotation.end))
            .map(str::to_string)
            .filter(|text| !text.is_empty());
        let mut close = false;
        let mut add_card = false;
        let mut lookup: Option<String> = None;
        let cards_loading = self.library.reader.cards_loading;
        let mut popup_selection: Option<Selection> = None;
        egui::Window::new(format!("{}详情", annotation.kind.label()))
            .collapsible(false)
            .resizable(true)
            .default_width(360.0)
            .show(ctx, |ui| {
                let color = crate::reader_stream::annotation_color(annotation.kind);
                ui.colored_label(color, annotation.kind.label());
                // 三段文字都渲染成只读但可选中的文本块，选中后复用正文的浮动工具条。
                let salt = (annotation.chapter, annotation.start);
                let body_font = ui.style().text_styles[&egui::TextStyle::Body].clone();
                let mono_font = ui.style().text_styles[&egui::TextStyle::Monospace].clone();
                // 释义用粗体：阅读器注册了 reader-bold 字族时用它，否则退回正文字体。
                let bold_family = egui::FontFamily::Name("reader-bold".into());
                let has_bold = ui.fonts_mut(|fonts| fonts.families().contains(&bold_family));
                let bold_font = if has_bold {
                    egui::FontId {
                        size: body_font.size,
                        family: bold_family,
                    }
                } else {
                    body_font.clone()
                };
                let mut selection: Option<Selection> = None;
                if let Some(original) = &original {
                    let id = ui.scope_id().with(("anno_original", salt));
                    let width = ui.available_width();
                    selection =
                        selectable_text(ui, id, original, mono_font, width).or(selection);
                }
                if !annotation.note.is_empty() {
                    let id = ui.scope_id().with(("anno_note", salt));
                    let width = ui.available_width();
                    selection = selectable_text(ui, id, &annotation.note, bold_font, width)
                        .or(selection);
                }
                if !annotation.detail.is_empty() {
                    let id = ui.scope_id().with(("anno_detail", salt));
                    let width = ui.available_width();
                    selection =
                        selectable_text(ui, id, &annotation.detail, body_font, width).or(selection);
                }
                popup_selection = selection;
                ui.separator();
                let dictionary = self.library.reader.dictionary.enabled();
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(
                            original.is_some() && !cards_loading,
                            egui::Button::new(if cards_loading {
                                "生成卡片中…"
                            } else {
                                "加入卡片"
                            }),
                        )
                        .clicked()
                    {
                        add_card = true;
                    }
                    if ui
                        .add_enabled(dictionary && original.is_some(), egui::Button::new("词典"))
                        .on_disabled_hover_text(
                            "在 config.toml 配置 [reader.dictionary] url 后可用",
                        )
                        .clicked()
                    {
                        lookup = original.clone();
                    }
                    if ui.button("关闭").clicked() {
                        close = true;
                    }
                });
            });
        if let Some(selection) = popup_selection {
            // 浮层工具条由正文流每帧统一绘制；选区变化后催一帧，让它立刻出现。
            let changed = self
                .library
                .reader
                .selection
                .as_ref()
                .map_or(true, |old| {
                    old.id != selection.id || old.text != selection.text || old.rect != selection.rect
                });
            if changed {
                ctx.request_repaint();
            }
            self.library.reader.selection = Some(selection);
        }
        if add_card {
            if let Some(original) = original {
                let back = annotation_back(&annotation.note, &annotation.detail);
                self.request_add_cards(vec![CardSeed {
                    front: original,
                    back,
                    kind: Some(annotation.kind),
                }]);
            }
        }
        if let Some(text) = lookup {
            self.lookup_dictionary(&text);
        }
        if close || ctx.input(|input| input.key_pressed(egui::Key::Escape)) {
            self.library.reader.annotation_detail = None;
        }
    }

    /// 全书阅读进度（0.0–1.0）：按视口顶部锚点在全书字符中的位置计算。
    fn reading_progress(&self) -> f32 {
        let offsets = &self.library.reader.chapter_char_offsets;
        let Some(&total) = offsets.last() else {
            return 0.0;
        };
        if total == 0 {
            return 0.0;
        }
        let chapter = self
            .library
            .reader
            .position_chapter
            .min(offsets.len().saturating_sub(2));
        let base = offsets[chapter];
        let chapter_len = offsets[chapter + 1] - base;
        let anchor = self
            .library
            .reader
            .anchor_char
            .unwrap_or(0)
            .min(chapter_len);
        ((base + anchor) as f32 / total as f32).clamp(0.0, 1.0)
    }

    /// 把当前章节、字号、主题与字体写回 `.sepiola/reading.json`。
    pub fn save_reader_position(&mut self) {
        let (Some(root), Some(rel)) =
            (non_empty_root(&self.library.root), self.library.reader.rel.clone())
        else {
            return;
        };
        let mut state = ReadingState::load(&root);
        state.font_size = self.library.reader.font_size;
        state.theme = self.library.reader.theme;
        state.font = self.library.reader.font;
        state.custom_font = self.library.reader.custom_font.clone();
        state.line_spacing = self.library.reader.line_spacing;
        state.margin = self.library.reader.margin;
        state.paragraph_spacing = self.library.reader.paragraph_spacing;
        state.background = self.library.reader.background;
        state.text_color = self.library.reader.text_color;
        state.pdf_dpi = self.library.reader.pdf_dpi;
        state.pdf_text_mode = self.library.reader.pdf_text_mode;
        let chapter = self.library.reader.chapter;
        let title = self
            .library
            .reader
            .document
            .as_ref()
            .and_then(|document| document.chapters.get(chapter))
            .map(|chapter| chapter.title.clone())
            .unwrap_or_default();
        state.record(
            &rel,
            chapter,
            &title,
            self.library.reader.chapter_offset,
            self.library.reader.anchor_char,
            Some(self.library.reader.position_chapter),
        );
        self.library.reader.last_saved_position = Some((
            self.library.reader.position_chapter,
            self.library.reader.chapter_offset,
            self.library.reader.anchor_char,
            chapter,
        ));
        if let Err(error) = state.save(&root) {
            self.library.message = format!("保存阅读进度失败：{error}");
        }
    }

    /// 章节内字符区间 →（章节正文，起止字节）。
    fn chapter_range(
        &self,
        chapter: usize,
        start_char: usize,
        end_char: usize,
    ) -> Option<(String, usize, usize)> {
        let text = self
            .library
            .reader
            .document
            .as_ref()?
            .chapters
            .get(chapter)?
            .text
            .clone();
        let offset = |index: usize| {
            text.char_indices()
                .nth(index)
                .map(|(byte, _)| byte)
                .unwrap_or(text.len())
        };
        let (start, end) = (offset(start_char), offset(end_char));
        Some((text, start, end))
    }

    /// 选区所在的整段文字，作为翻译 / AI 解释的上下文。
    pub(crate) fn selection_context(&self) -> Option<String> {
        /// 最多携带的上下文字符数（约一段话）。
        const CONTEXT_CHARS: usize = 800;
        let (chapter, start, end) = self.library.reader.selection.as_ref()?.highlight?;
        let (text, start, end) = self.chapter_range(chapter, start, end)?;
        sepiola::reader::assist::paragraph_context(&text, start, end, CONTEXT_CHARS)
    }

    /// 给正文选区加高亮；`color` 为 `None` 时移除命中的高亮。
    pub(crate) fn apply_highlight(
        &mut self,
        chapter: usize,
        start_char: usize,
        end_char: usize,
        color: Option<[u8; 3]>,
    ) {
        let Some((_, start, end)) = self.chapter_range(chapter, start_char, end_char) else {
            return;
        };
        if start >= end {
            return;
        }
        let changed = match color {
            Some(color) => {
                self.library.reader.highlights.add(Highlight {
                    chapter,
                    start,
                    end,
                    color,
                });
                true
            }
            None => self.library.reader.highlights.remove(chapter, start, end),
        };
        if changed {
            self.save_highlights();
        }
    }

    /// 高亮文件很小，改动后立即落盘。
    fn save_highlights(&mut self) {
        let (Some(root), Some(rel)) =
            (non_empty_root(&self.library.root), self.library.reader.rel.clone())
        else {
            return;
        };
        let mut book = self.library.reader.highlights.clone();
        book.book = rel.to_string_lossy().into_owned();
        if let Err(error) = sepiola::reader::highlight::save(&root, &rel, &book) {
            self.notify(format!("保存高亮失败：{error}"));
        }
    }

    pub fn close_reader(&mut self) {
        self.save_reader_position();
        self.library.reader = ReaderState::default();
        self.reader_window = false;
    }

    pub fn notify(&mut self, text: impl Into<String>) {
        self.library.reader.notice = text.into();
        self.library.reader.notice_until = Some(Instant::now() + std::time::Duration::from_secs(4));
    }

    pub fn reader_ui(&mut self, ui: &mut egui::Ui) {
        if self.library.reader.rel.is_none() {
            self.reader_window = false;
            return;
        }
        crate::reader_theme::apply_reader_theme(
            ui,
            self.library.reader.theme,
            self.library.reader.background,
            self.library.reader.text_color,
        );
        // 自定义字体变化时重建字体表：书籍打开或设置里切换系统字体。
        let custom = self.library.reader.custom_font.clone();
        if custom != self.library.reader.applied_font {
            crate::assets::install_reader_fonts(
                ui.ctx(),
                custom.as_deref().map(std::path::Path::new),
            );
            self.library.reader.applied_font = custom;
            self.library.reader.chapter_heights.clear();
        }
        let mut back = false;
        // 顶栏与章节导航留出内边距，避免内容紧贴窗口边缘。
        egui::Frame::NONE
            .inner_margin(egui::Margin::symmetric(12, 8))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    back = pointing_hand(ui.button("← 关闭阅读")).clicked();
            let title = self.library.reader.title.clone();
            ui.add(egui::Label::new(egui::RichText::new(title).size(17.0).strong()).truncate());
            let author = self.library.reader.author.clone();
            if !author.is_empty() {
                ui.weak(author);
            }
            if self
                .library
                .reader
                .notice_until
                .is_some_and(|until| Instant::now() < until)
            {
                ui.colored_label(egui::Color32::from_rgb(90, 150, 100), &self.library.reader.notice);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if pointing_hand(ui.button("阅读设置")).clicked() {
                    self.library.reader.settings_open = !self.library.reader.settings_open;
                }
                let toc_label = if self.library.reader.toc_open {
                    "收起目录"
                } else {
                    "目录"
                };
                if pointing_hand(ui.button(toc_label)).clicked() {
                    self.library.reader.toc_open = !self.library.reader.toc_open;
                }
                if pointing_hand(ui.button("分析结果")).clicked() {
                    self.library.reader.analysis_open = true;
                }
                let analyzing = self.library.reader.analysis_loading.is_some();
                let label = if analyzing { "分析中…" } else { "分析本章" };
                let enabled = !analyzing && self.library.reader.ai.enabled();
                let response = ui
                    .add_enabled(enabled, egui::Button::new(label))
                    .on_disabled_hover_text(sepiola::ai::MISSING_HINT);
                if pointing_hand(response).clicked() {
                    self.start_analysis();
                }
            });
            });
        });
        if back {
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
            self.close_reader();
            return;
        }

        if self.library.reader.loading {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("正在解析正文…");
            });
            return;
        }
        if let Some(error) = &self.library.reader.error {
            ui.colored_label(egui::Color32::from_rgb(200, 70, 70), error.as_str());
            return;
        }
        let Some(document) = self.library.reader.document.clone() else {
            return;
        };
        let total = document.chapters.len();
        let chapter = self.library.reader.chapter.min(total.saturating_sub(1));
        let mut navigation: Option<isize> = None;
        let mut jump: Option<usize> = None;
        self.reader_toc_drawer(ui, &document, &mut jump);
        egui::Frame::NONE
            .inner_margin(egui::Margin::symmetric(12, 6))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    if pointing_hand(ui.add_enabled(
                        chapter > 0,
                        egui::Button::new("上一章"),
                    ))
                    .clicked()
                    {
                        navigation = Some(-1);
                    }
                    if pointing_hand(ui.add_enabled(
                        chapter + 1 < total,
                        egui::Button::new("下一章"),
                    ))
                    .clicked()
                    {
                        navigation = Some(1);
                    }
                    // 章节号优先用目录里的真实编号（如 Chapter 55 → 55/105），
                    // 识别不到时才回退到文档序号，避免前置内容把编号顶偏。
                    match document.chapter_progress(chapter) {
                        Some((number, numbered_total)) => {
                            ui.label(format!("{number}/{numbered_total}"));
                        }
                        None => {
                            ui.label(format!("{}/{}", chapter + 1, total));
                        }
                    }
                    let title = document.chapters[chapter].title.as_str();
                    // 标题本身就是「Chapter N」时不再重复显示。
                    let bare = document
                        .chapter_progress(chapter)
                        .is_some_and(|(number, _)| is_bare_chapter_title(title, number));
                    if !bare {
                        ui.add(egui::Label::new(title).truncate());
                    }
                });
            });
        if ui.input(|input| input.key_pressed(egui::Key::ArrowLeft)) {
            navigation = Some(-1);
        }
        if ui.input(|input| input.key_pressed(egui::Key::ArrowRight)) {
            navigation = Some(1);
        }
        // 全书阅读进度：章节导航下方的细进度条，通栏显示。
        let progress = self.reading_progress();
        egui::Frame::NONE
            .inner_margin(egui::Margin::symmetric(12, 0))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    let percent = format!("{:.1}%", progress * 100.0);
                    let bar_width = (ui.available_width() - 52.0).max(40.0);
                    ui.add(
                        egui::ProgressBar::new(progress)
                            .desired_width(bar_width)
                            .desired_height(6.0)
                            .corner_radius(3.0),
                    )
                    .on_hover_text("全书阅读进度");
                    ui.label(egui::RichText::new(percent).small().weak());
                });
            });
        // 页边距只影响正文区域，顶栏与章节导航保持通栏。
        let margin = self.library.reader.margin;
        let side = margin.round().clamp(0.0, 120.0) as i8;
        let vertical = (margin * 0.4).round().clamp(0.0, 48.0) as i8;
        egui::Frame::NONE
            .inner_margin(egui::Margin::symmetric(side, vertical))
            .show(ui, |ui| {
                self.reader_stream(ui, &document);
            });
        self.assist_window(ui.ctx());
        self.footnote_window(ui.ctx());
        self.reader_settings_window(ui.ctx());
        self.analysis_window(ui.ctx());
        self.annotation_window(ui.ctx());
        self.item_chat_window(ui.ctx());
        self.pending_cards_window(ui.ctx());

        if let Some(index) = jump {
            self.goto_chapter(index);
        } else if let Some(delta) = navigation {
            let target = (chapter as isize + delta).clamp(0, total as isize - 1) as usize;
            if target != chapter {
                self.goto_chapter(target);
            }
        }
    }

    /// 右侧目录抽屉：以浮层从右侧滑出，不改变正文宽度，避免动画期间反复重排正文造成卡顿。
    fn reader_toc_drawer(
        &mut self,
        ui: &mut egui::Ui,
        document: &Arc<BookDocument>,
        jump: &mut Option<usize>,
    ) {
        let open = self.library.reader.toc_open;
        let slide = ui
            .ctx()
            .animate_bool_responsive(egui::Id::unique("reader_toc_slide"), open);
        if slide <= 0.0 {
            return;
        }
        let area = ui.available_rect_before_wrap();
        let width = 300.0_f32.min(area.width());
        let pos = egui::pos2(area.right() - width * slide, area.top());
        // 未完全滑出时裁掉溢出到右侧的部分。
        let clip = egui::Rect::from_min_max(pos, area.right_bottom());
        let mut close = false;
        egui::Area::new(egui::Id::unique("reader_toc_drawer"))
            .fixed_pos(pos)
            .order(egui::Order::Foreground)
            .show(ui.ctx(), |ui| {
                ui.set_clip_rect(clip);
                let fill = ui.visuals().panel_fill;
                let stroke = ui.visuals().widgets.noninteractive.bg_stroke;
                egui::Frame::NONE
                    .fill(fill)
                    .stroke(stroke)
                    .inner_margin(egui::Margin::same(12))
                    .show(ui, |ui| {
                        ui.set_width(width - 24.0);
                        ui.set_min_height(area.height() - 24.0);
                        ui.horizontal(|ui| {
                            ui.strong("目录");
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if pointing_hand(ui.button("×")).clicked() {
                                        close = true;
                                    }
                                },
                            );
                        });
                        ui.separator();
                        // 列表高度必须显式限定，浮层里的可用高度是无限的。
                        let list_height = (area.bottom() - 12.0 - ui.cursor().top()).max(0.0);
                        egui::ScrollArea::vertical()
                            .id_salt("reader_toc_list")
                            .max_height(list_height)
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                for (index, item) in document.chapters.iter().enumerate() {
                                    // 目录标题已含章节号（如 Chapter 55）时不再加前缀，
                                    // 否则会与前置内容的文档序号混在一起造成编号错位。
                                    let label = if sepiola::reader::chapter_number(&item.title).is_some()
                                    {
                                        item.title.clone()
                                    } else {
                                        format!("{} {}", index + 1, item.title)
                                    };
                                    let current = index == self.library.reader.chapter;
                                    if pointing_hand(ui.selectable_label(current, label)).clicked() {
                                        *jump = Some(index);
                                        close = true;
                                    }
                                }
                            });
                    });
            });
        self.library.reader.toc_open = open && !close;
    }

    /// 脚注浮窗：点击正文中的脚注标记后展示，关闭或 Esc 收起。
    pub fn footnote_window(&mut self, ctx: &egui::Context) {
        let Some(text) = self.library.reader.footnote.clone() else {
            return;
        };
        let mut close = false;
        egui::Window::new("脚注")
            .collapsible(false)
            .resizable(true)
            .default_width(380.0)
            .show(ctx, |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("reader_footnote_scroll")
                    .max_height(260.0)
                    .show(ui, |ui| {
                        ui.add(egui::Label::new(text.as_str()).wrap());
                    });
                ui.separator();
                ui.horizontal(|ui| {
                    if pointing_hand(ui.button("复制")).clicked() {
                        ctx.copy_text(text.clone());
                    }
                    if pointing_hand(ui.button("关闭")).clicked() {
                        close = true;
                    }
                });
            });
        if close || ctx.input(|input| input.key_pressed(egui::Key::Escape)) {
            self.library.reader.footnote = None;
        }
    }

    pub fn goto_chapter(&mut self, chapter: usize) {
        self.library.reader.chapter = chapter;
        self.library.reader.stream_start = chapter;
        self.library.reader.stream_end = chapter;
        self.library.reader.stream_generation = self.library.reader.stream_generation.wrapping_add(1);
        self.library.reader.stream_offset = 0.0;
        // 必须显式写回偏移，否则偏移恰好不变时（例如跳到第一章）ScrollArea
        // 会保留旧的内部偏移，跳转看起来没有生效。
        self.library.reader.pending_offset = Some(0.0);
        self.library.reader.chapter_offset = 0.0;
        self.library.reader.position_chapter = chapter;
        // 主动跳章后旧锚点失效，等正文流按新位置重新生成。
        self.library.reader.anchor_char = None;
        self.library.reader.restore_anchor = None;
        self.library.reader.restore_position_chapter = None;
        self.save_reader_position();
    }
}

/// 标题是否只是「Chapter N / 第 N 章」这种不含额外信息的写法。
fn is_bare_chapter_title(title: &str, number: usize) -> bool {
    let compact: String = title
        .chars()
        .filter(|ch| ch.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect();
    compact == format!("chapter{number}") || compact == format!("第{number}章")
}

/// config.toml 中的阅读器配置，被设置页填写的值覆盖（留空则沿用）。
pub fn merged_reader_settings(user: &crate::user_settings::UserSettings) -> ReaderSettings {
    let mut settings = load_reader_settings();
    if !user.ai_provider.trim().is_empty() {
        settings.ai.provider = sepiola::config::AiProvider::from_key(&user.ai_provider);
    }
    let overrides = [
        (&user.ai_base_url, &mut settings.ai.base_url),
        (&user.ai_api_key, &mut settings.ai.api_key),
        (&user.ai_model, &mut settings.ai.model),
        (&user.proxy_http, &mut settings.ai.proxy_http),
        (&user.proxy_https, &mut settings.ai.proxy_https),
        (&user.ai_cli_path, &mut settings.ai.cli_path),
        (&user.ai_cli_args, &mut settings.ai.cli_args),
        (&user.ai_cli_model, &mut settings.ai.cli_model),
        (&user.ai_prompt_translate, &mut settings.ai.prompts.translate),
        (&user.ai_prompt_explain, &mut settings.ai.prompts.explain),
        (&user.ai_prompt_analyze, &mut settings.ai.prompts.analyze),
        (&user.ai_prompt_exercise, &mut settings.ai.prompts.exercise),
        (&user.ai_prompt_sentence, &mut settings.ai.prompts.sentence),
        (&user.ai_prompt_card, &mut settings.ai.prompts.card),
        (&user.ai_prompt_chat, &mut settings.ai.prompts.chat),
        (&user.dictionary_url, &mut settings.dictionary.url),
    ];
    for (value, slot) in overrides {
        if !value.trim().is_empty() {
            *slot = value.trim().to_string();
        }
    }
    settings
}

/// 按可执行文件目录、当前目录的顺序查找 config.toml 中的 `[reader]`。
pub fn load_reader_settings() -> ReaderSettings {
    let mut candidates = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join("config.toml"));
        }
    }
    candidates.push(PathBuf::from("config.toml"));
    for path in candidates {
        if path.is_file() {
            if let Ok(config) = sepiola::config::Config::load_file(&path) {
                return config.reader;
            }
        }
    }
    sepiola::config::ReaderSettings::default()
}

impl SepiolaApp {
    /// 打开书籍、以及设置页修改后刷新工具条依赖的服务配置。
    pub fn reader_settings(&self) -> ReaderSettings {
        merged_reader_settings(&self.user_settings)
    }

    pub fn sync_reader_services(&mut self) {
        let settings = self.reader_settings();
        self.library.reader.ai = settings.ai.clone();
        self.library.reader.dictionary = settings.dictionary;
        self.notebook.ai = settings.ai;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_settings_override_config_file_values() {
        let user = crate::user_settings::UserSettings {
            dictionary_url: " eudic://dict/{word} ".to_string(),
            ai_base_url: "https://api.example.test/v1".to_string(),
            ai_model: "test-model".to_string(),
            ..crate::user_settings::UserSettings::default()
        };
        let settings = merged_reader_settings(&user);
        assert_eq!(settings.dictionary.url, "eudic://dict/{word}");
        assert_eq!(settings.ai.base_url, "https://api.example.test/v1");
        assert_eq!(settings.ai.model, "test-model");
    }

    #[test]
    fn user_settings_override_prompts() {
        let user = crate::user_settings::UserSettings {
            ai_prompt_explain: "  用英文解释。 ".to_string(),
            ..crate::user_settings::UserSettings::default()
        };
        let settings = merged_reader_settings(&user);
        assert_eq!(settings.ai.prompts.explain, "用英文解释。");
        assert!(settings.ai.prompts.translate.is_empty());
    }

    #[test]
    fn blank_user_settings_keep_config_file_values() {
        let config = load_reader_settings();
        let merged = merged_reader_settings(&crate::user_settings::UserSettings::default());
        assert_eq!(merged.dictionary.url, config.dictionary.url);
        assert_eq!(merged.ai.base_url, config.ai.base_url);
        assert_eq!(merged.ai.api_key, config.ai.api_key);
        assert_eq!(merged.ai.model, config.ai.model);
    }
}
