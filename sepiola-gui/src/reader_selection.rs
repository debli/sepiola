//! 选区浮动工具条、翻译 / AI 解释与分享（M45）。

use std::io::Write;
use std::process::Command;
use std::sync::mpsc::{channel, TryRecvError};
use std::sync::{Arc, Mutex};

use eframe::egui;
use egui_commonmark::CommonMarkViewer;
use sepiola::reader::assist::{self, AssistKind};

use crate::reader::{AssistState, CardSeed, Selection};
use crate::state::SepiolaApp;

/// 捕获 egui 帧末发出的文字选区结算命令（label 选区只能这样拿到内容）。
#[derive(Clone, Default)]
pub struct LabelSelectionSink {
    text: Arc<Mutex<Option<String>>>,
}

impl LabelSelectionSink {
    /// 取走最近一次结算的选区文字。
    pub fn take(&self) -> Option<String> {
        self.text.lock().ok().and_then(|mut text| text.take())
    }
}

impl egui::plugin::Plugin for LabelSelectionSink {
    fn debug_name(&self) -> &'static str {
        "LabelSelectionSink"
    }

    fn output_hook(&mut self, ctx: &egui::Context, output: &mut egui::FullOutput) {
        for command in &output.platform_output.commands {
            if let egui::OutputCommand::TextSelectionSettled(text) = command {
                if let Ok(mut slot) = self.text.lock() {
                    *slot = Some(text.clone());
                }
                // 结算发生在帧末，催一帧让界面读出选区并画出工具条。
                ctx.request_repaint();
            }
        }
    }
}

enum Action {
    Copy,
    Translate,
    Explain,
    Card,
    /// 给选区加高亮（颜色）。
    Highlight([u8; 3]),
    /// 移除选区命中的高亮。
    Unhighlight,
    Share,
    Dictionary,
    Close,
}

/// 工具条上的高亮颜色。
const HIGHLIGHT_COLORS: [(&str, [u8; 3]); 4] = [
    ("黄色", [255, 221, 105]),
    ("绿色", [166, 226, 163]),
    ("蓝色", [155, 203, 247]),
    ("粉色", [247, 178, 208]),
];

/// 计算工具条位置：水平居中于选区，贴在选区上方（优先）或下方，并避开鼠标指针。
/// 预留的工具条高度：判断“上方还放得下吗”时用它，避免依赖实测尺寸产生反馈。
const TOOLBAR_HEIGHT: f32 = 48.0;

/// 计算工具条位置：水平居中于选区，贴在选区上方（优先）或下方。
/// 返回 (fixed_pos, pivot)；上方候选以底边定位、下方候选以顶边定位，
/// 实际高度与估算不一致时工具条只会朝远离选区的方向伸展，不会压回选区。
/// 位置只取决于选区与屏幕，不参考鼠标：否则鼠标移到工具条上会把工具条“推走”，
/// 形成来回跳动的反馈循环。
fn toolbar_placement(
    selection_rect: egui::Rect,
    size: egui::Vec2,
    screen: egui::Rect,
) -> (egui::Pos2, egui::Align2) {
    let half = size.x * 0.5;
    let center_x = if screen.width() >= size.x + 8.0 {
        selection_rect
            .right()
            .clamp(screen.left() + half + 4.0, screen.right() - half - 4.0)
    } else {
        screen.center().x
    };
    let height = TOOLBAR_HEIGHT.max(size.y);
    let gap = 8.0;
    let above_y = selection_rect.top() - gap;
    let below_y = selection_rect.bottom() + gap;
    let room_above = above_y - (screen.top() + 4.0);
    let room_below = (screen.bottom() - 4.0) - below_y;
    let pos = |y: f32| egui::pos2(center_x, y);
    if room_above >= height {
        (pos(above_y), egui::Align2::CENTER_BOTTOM)
    } else if room_below >= height {
        (pos(below_y), egui::Align2::CENTER_TOP)
    } else if room_above >= room_below {
        // 上下都放不下：贴在空间较大的一侧，并夹进屏幕。
        (
            pos(above_y.max(screen.top() + 4.0 + height)),
            egui::Align2::CENTER_BOTTOM,
        )
    } else {
        (
            pos(below_y.min(screen.bottom() - 4.0 - height)),
            egui::Align2::CENTER_TOP,
        )
    }
}

impl SepiolaApp {
    /// 用系统词典查询一段文字（浮动工具条与详情弹窗共用）。
    pub(crate) fn lookup_dictionary(&mut self, text: &str) {
        let target = self.library.reader.dictionary.target(text);
        if !has_scheme(&target) {
            self.notify("词典地址无效：需要 https:// 或 eudic:// 这样的完整地址");
        } else if let Err(error) = open_target(&target) {
            self.notify(format!("打开词典失败：{error}"));
        } else {
            self.notify("已交给系统词典查询");
        }
    }

    /// 浮动工具条：贴在选区的上方或下方，位置不受鼠标影响，不会因悬停而跳动。
    pub fn selection_toolbar(&mut self, ui: &egui::Ui) {
        let ctx = ui.ctx();
        let Some((text, id, rect, highlight)) = self
            .library
            .reader
            .selection
            .as_ref()
            .map(|selection| {
                (
                    selection.text.clone(),
                    selection.id,
                    selection.rect,
                    selection.highlight,
                )
            })
        else {
            self.library.reader.toolbar_rect = None;
            return;
        };
        // 选区滚出可视区域后不再显示工具条。
        let screen = ctx.content_rect();
        if !screen.expand(24.0).intersects(rect) {
            self.library.reader.toolbar_rect = None;
            return;
        }
        // 用上一帧实测尺寸估算位置，第一次出现时用默认值。
        let size = self
            .library
            .reader
            .toolbar_size
            .unwrap_or(egui::vec2(300.0, 40.0));
        let (pos, pivot) = toolbar_placement(rect, size, screen);
        // 工具条放在 Foreground 层：永远浮在正文与弹窗（AI 解释、详情）之上，
        // 否则贴在弹窗内选区上的工具条会被弹窗本身挡住。
        // 仍用 Ui::new 而非 Area：空白处不拦截指针，只有按钮接收点击，
        // 这样浮层遮住的文字仍能按下去继续拖选（独立 Area 会整块吞掉指针）。
        let origin = match pivot {
            egui::Align2::CENTER_BOTTOM => egui::pos2(pos.x - size.x * 0.5, pos.y - size.y),
            _ => egui::pos2(pos.x - size.x * 0.5, pos.y),
        };
        let configured = self.library.reader.ai.enabled();
        let dictionary = self.library.reader.dictionary.enabled();
        let mut action: Option<Action> = None;
        let mut toolbar = egui::Ui::new(
            ctx.clone(),
            egui::Id::new("reader_selection_toolbar"),
            egui::UiBuilder::new()
                .max_rect(egui::Rect::from_min_max(origin, screen.max))
                .layer_id(egui::LayerId::new(
                    egui::Order::Foreground,
                    egui::Id::new("reader_selection_toolbar"),
                )),
        );
        let response = egui::Frame::popup(toolbar.style()).show(&mut toolbar, |ui| {
            ui.horizontal(|ui| {
                if ui.button("复制").clicked() {
                    action = Some(Action::Copy);
                }
                if ui
                    .add_enabled(configured, egui::Button::new("翻译"))
                    .on_disabled_hover_text("在 config.toml 配置 [reader.ai] 后可用")
                    .clicked()
                {
                    action = Some(Action::Translate);
                }
                if ui
                    .add_enabled(configured, egui::Button::new("AI 解释"))
                    .on_disabled_hover_text("在 config.toml 配置 [reader.ai] 后可用")
                    .clicked()
                {
                    action = Some(Action::Explain);
                }
                if ui
                    .add_enabled(configured, egui::Button::new("添加卡片"))
                    .on_disabled_hover_text("在 config.toml 配置 [reader.ai] 后可用")
                    .clicked()
                {
                    action = Some(Action::Card);
                }
                // 高亮：只在正文选区上可用（弹窗里的文字没有章节位置）。
                if highlight.is_some() {
                    for (name, color) in HIGHLIGHT_COLORS {
                        let swatch = egui::Button::new("")
                            .fill(egui::Color32::from_rgb(color[0], color[1], color[2]))
                            .min_size(egui::vec2(14.0, 14.0));
                        if ui.add(swatch).on_hover_text(name).clicked() {
                            action = Some(Action::Highlight(color));
                        }
                    }
                    if ui
                        .button("取消高亮")
                        .on_hover_text("移除选中文字上的高亮")
                        .clicked()
                    {
                        action = Some(Action::Unhighlight);
                    }
                }
                if ui.button("分享").clicked() {
                    action = Some(Action::Share);
                }
                if ui
                    .add_enabled(dictionary, egui::Button::new("词典"))
                    .on_disabled_hover_text(
                        "在 config.toml 配置 [reader.dictionary] url 后可用",
                    )
                    .clicked()
                {
                    action = Some(Action::Dictionary);
                }
                ui.separator();
                if ui.button("关闭").clicked() {
                    action = Some(Action::Close);
                }
            });
        });
        self.library.reader.toolbar_rect = Some(response.response.rect);
        // 鼠标悬停时不再更新尺寸，避免尺寸变化反过来影响位置判断。
        if !response.response.hovered() {
            self.library.reader.toolbar_size = Some(response.response.rect.size());
        }

        match action {
            Some(Action::Copy) => {
                ctx.copy_text(text.clone());
                self.notify("已复制选中文字");
                self.clear_selection(ctx, id);
            }
            Some(Action::Translate) => self.start_assist(AssistKind::Translate, text),
            Some(Action::Explain) => self.start_assist(AssistKind::Explain, text),
            Some(Action::Card) => {
                // 选中文字没有现成的 AI 解释，交给 AI 生成卡片内容。
                self.request_add_cards(vec![CardSeed {
                    front: text.clone(),
                    back: String::new(),
                    kind: None,
                }]);
                self.clear_selection(ctx, id);
            }
            Some(Action::Highlight(color)) => {
                if let Some((chapter, start, end)) = highlight {
                    self.apply_highlight(chapter, start, end, Some(color));
                }
                self.clear_selection(ctx, id);
            }
            Some(Action::Unhighlight) => {
                if let Some((chapter, start, end)) = highlight {
                    self.apply_highlight(chapter, start, end, None);
                }
                self.clear_selection(ctx, id);
            }
            Some(Action::Share) => {
                if let Err(error) = share_text(&text) {
                    ctx.copy_text(text.clone());
                    self.notify(format!("分享失败（{error}），已复制到剪贴板"));
                } else {
                    self.notify("已生成分享文件并交给系统打开");
                }
                self.clear_selection(ctx, id);
            }
            Some(Action::Dictionary) => {
                self.lookup_dictionary(&text);
                self.clear_selection(ctx, id);
            }
            Some(Action::Close) => self.clear_selection(ctx, id),
            None => {}
        }
    }

    fn clear_selection(&mut self, ctx: &egui::Context, id: egui::Id) {
        if let Some(mut state) = egui::widgets::text_edit::TextEditState::load(ctx, id) {
            state.cursor.set_char_range(None);
            state.store(ctx, id);
        }
        // 弹窗里的 Markdown 文字用的是 label 选区，一并清掉。
        ctx.plugin::<egui::text_selection::LabelSelectionState>()
            .lock()
            .clear_selection();
        self.library.reader.selection = None;
        self.library.reader.toolbar_rect = None;
        // 位图页面上的自定义文字选区也要一起清掉。
        self.library.reader.pdf_select = None;
    }

    fn start_assist(&mut self, kind: AssistKind, text: String) {
        if !self.library.reader.ai.enabled() {
            self.notify("未配置 [reader.ai]，无法使用翻译 / AI 解释");
            return;
        }
        let settings = self.library.reader.ai.clone();
        let source = text.clone();
        // 带上选区所在的段落，让模型能判断指代与语境。
        let context = self.selection_context();
        let (sender, receiver) = channel();
        std::thread::spawn(move || {
            let _ = sender.send(assist::ask_with_usage(
                &settings,
                kind,
                &text,
                context.as_deref(),
            ));
        });
        self.library.reader.assist = AssistState {
            open: true,
            title: kind.title().to_string(),
            source,
            loading: true,
            receiver: Some(receiver),
            ..AssistState::default()
        };
    }

    pub fn poll_assist(&mut self) {
        let result = match self.library.reader.assist.receiver.as_ref() {
            Some(receiver) => match receiver.try_recv() {
                Ok(result) => Some(result),
                Err(TryRecvError::Empty) => None,
                Err(TryRecvError::Disconnected) => {
                    Some(Err("翻译线程已退出".to_string()))
                }
            },
            None => None,
        };
        let Some(result) = result else { return };
        self.library.reader.assist.receiver = None;
        self.library.reader.assist.loading = false;
        match result {
            Ok((body, usage)) => {
                self.library.reader.assist.body = body;
                self.library.reader.assist.usage = Some(usage);
            }
            Err(error) => self.library.reader.assist.error = Some(error),
        }
    }

    pub fn assist_window(&mut self, ctx: &egui::Context) {
        if !self.library.reader.assist.open {
            return;
        }
        let title = self.library.reader.assist.title.clone();
        let body = self.library.reader.assist.body.clone();
        let mut open = true;
        let mut copy = false;
        let mut add_note = false;
        // egui 的 label 选区在帧末才结算，由插件转存到这里。
        let settled = self.label_selection.take();
        egui::Window::new(title)
            .open(&mut open)
            .default_width(420.0)
            .resizable(true)
            .show(ctx, |ui| {
                if self.library.reader.assist.loading {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("正在请求…");
                    });
                }
                if let Some(error) = &self.library.reader.assist.error {
                    ui.colored_label(egui::Color32::from_rgb(200, 70, 70), error.as_str());
                }
                if !body.is_empty() {
                    egui::ScrollArea::vertical()
                        .max_height(320.0)
                        .auto_shrink([false, true])
                        .show(ui, |ui| {
                            // AI 返回的内容通常是 Markdown，按格式渲染；
                            // egui 的 label 本身支持拖选，选区由下方工具条复用。
                            ui.style_mut().text_styles.insert(
                                egui::TextStyle::Body,
                                egui::FontId::proportional(15.0),
                            );
                            CommonMarkViewer::new().show(
                                ui,
                                &mut self.library.reader.assist.markdown_cache,
                                &body,
                            );
                        });
                    if let Some(usage) = &self.library.reader.assist.usage {
                        ui.label(egui::RichText::new(usage.summary()).small().weak());
                    }
                    ui.horizontal(|ui| {
                        if ui.button("复制结果").clicked() {
                            copy = true;
                        }
                        if ui.button("放入笔记").clicked() {
                            add_note = true;
                        }
                    });
                }
                self.library.reader.assist.rect = Some(ui.max_rect());
            });
        // 只有拖选发生在弹窗内才接管：正文 TextEdit 的选区结算也走同一个命令。
        let pointer = ctx.input(|input| input.pointer.interact_pos());
        if let Some(text) = settled {
            let text = text.trim().to_string();
            let inside = pointer
                .zip(self.library.reader.assist.rect)
                .is_some_and(|(pointer, rect)| rect.contains(pointer));
            if let (true, false, Some(anchor)) = (inside, text.is_empty(), pointer) {
                // 拿不到 label 选区的包围盒，用拖选结束的位置当锚点，工具条贴在它上方。
                self.library.reader.selection = Some(Selection {
                    text,
                    id: egui::Id::new("assist_body"),
                    rect: egui::Rect::from_min_size(
                        anchor - egui::vec2(0.0, 14.0),
                        egui::vec2(0.0, 14.0),
                    ),
                    highlight: None,
                });
                ctx.request_repaint();
            }
        }
        if copy {
            ctx.copy_text(self.library.reader.assist.body.clone());
            self.notify("已复制结果");
        }
        if add_note {
            let front = self.library.reader.assist.source.clone();
            let body = self.library.reader.assist.body.clone();
            self.add_note_card(&front, &body, Vec::new());
        }
        if !open {
            self.library.reader.assist.open = false;
        }
    }
}

/// 把文字写入临时文件，并交给系统默认程序打开；失败时由调用方回退到剪贴板。
fn share_text(text: &str) -> Result<(), String> {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let path = std::env::temp_dir().join(format!("sepiola-share-{stamp}.txt"));
    let mut file = std::fs::File::create(&path).map_err(|error| error.to_string())?;
    file.write_all(text.as_bytes())
        .map_err(|error| error.to_string())?;
    #[cfg(target_os = "windows")]
    let mut command = {
        let mut command = Command::new("cmd");
        command.args(["/C", "start", ""]);
        command
    };
    #[cfg(target_os = "macos")]
    let mut command = Command::new("open");
    #[cfg(all(unix, not(target_os = "macos")))]
    let mut command = Command::new("xdg-open");
    command
        .arg(&path)
        .spawn()
        .map(|_| ())
        .map_err(|error| error.to_string())
}

/// 用系统默认程序打开链接或自定义 URI scheme（`https://`、`eudic://` 等）。
pub(crate) fn open_target(target: &str) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    let mut command = {
        let mut command = Command::new("cmd");
        command.args(["/C", "start", ""]);
        command
    };
    #[cfg(target_os = "macos")]
    let mut command = Command::new("open");
    #[cfg(all(unix, not(target_os = "macos")))]
    let mut command = Command::new("xdg-open");
    command
        .arg(target)
        .spawn()
        .map(|_| ())
        .map_err(|error| error.to_string())
}

/// 判断是否为带 scheme 的完整地址，避免把半截模板直接丢给系统。
pub(crate) fn has_scheme(target: &str) -> bool {
    let Some((scheme, _)) = target.split_once("://") else {
        return false;
    };
    let mut chars = scheme.chars();
    matches!(chars.next(), Some(first) if first.is_ascii_alphabetic())
        && chars.all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '+' | '-' | '.'))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn placed_rect(pos: egui::Pos2, pivot: egui::Align2, size: egui::Vec2) -> egui::Rect {
        pivot.align_size_within_rect(size, egui::Rect::from_min_size(pos, egui::Vec2::ZERO))
    }

    #[test]
    fn recognises_supported_schemes() {
        assert!(has_scheme("https://dict.example/?q=a"));
        assert!(has_scheme("eudic://dict/a"));
        assert!(has_scheme("x-apple.systempreferences://x"));
        assert!(!has_scheme("dict.example/?q=a"));
        assert!(!has_scheme("://broken"));
        assert!(!has_scheme(""));
    }

    #[test]
    fn toolbar_sits_above_selection() {
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1000.0, 800.0));
        let selection = egui::Rect::from_min_max(egui::pos2(300.0, 300.0), egui::pos2(700.0, 360.0));
        let size = egui::vec2(300.0, 40.0);
        let (pos, pivot) = toolbar_placement(selection, size, screen);
        let rect = placed_rect(pos, pivot, size);
        assert!(rect.bottom() <= selection.top(), "工具条应位于选区上方");
        assert!(!rect.intersects(selection), "工具条不应与选区重叠");
    }

    #[test]
    fn toolbar_goes_below_when_no_room_above() {
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1000.0, 800.0));
        let selection = egui::Rect::from_min_max(egui::pos2(300.0, 10.0), egui::pos2(700.0, 60.0));
        let size = egui::vec2(300.0, 40.0);
        let (pos, pivot) = toolbar_placement(selection, size, screen);
        let rect = placed_rect(pos, pivot, size);
        assert!(rect.top() >= selection.bottom(), "顶部空间不足时应放到选区下方");
        assert!(!rect.intersects(selection));
    }

    #[test]
    fn toolbar_stays_inside_screen_and_outside_selection() {
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1000.0, 800.0));
        let size = egui::vec2(300.0, 40.0);
        // 选区几乎占满屏幕高度，上方与下方都不足以完整放下工具条。
        for top in [2.0, 200.0, 380.0, 600.0] {
            let selection = egui::Rect::from_min_max(
                egui::pos2(300.0, top),
                egui::pos2(700.0, (top + 180.0).min(798.0)),
            );
            let (pos, pivot) = toolbar_placement(selection, size, screen);
            let rect = placed_rect(pos, pivot, size);
            assert!(
                rect.top() >= screen.top() && rect.bottom() <= screen.bottom(),
                "工具条应保持在屏幕内：{rect:?}"
            );
        }
    }
}
