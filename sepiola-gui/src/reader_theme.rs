//! 阅读主题、字体与版面设置（M45）。

use std::path::Path;

use eframe::egui;
use sepiola::reader::{ReaderFont, ReaderTheme};

use crate::assets::{base_name, pointing_hand, system_fonts};
use crate::state::SepiolaApp;

/// 阅读页最终配色：背景、文字、选区。
pub struct Palette {
    pub background: egui::Color32,
    pub text: egui::Color32,
    pub selection: egui::Color32,
}

fn palette(theme: ReaderTheme) -> Option<Palette> {
    let (background, text, selection) = match theme {
        ReaderTheme::Follow => return None,
        ReaderTheme::Paper => (
            egui::Color32::from_rgb(247, 244, 236),
            egui::Color32::from_rgb(34, 34, 34),
            egui::Color32::from_rgb(187, 214, 245),
        ),
        ReaderTheme::Sepia => (
            egui::Color32::from_rgb(242, 228, 201),
            egui::Color32::from_rgb(74, 58, 40),
            egui::Color32::from_rgb(216, 195, 158),
        ),
        ReaderTheme::Night => (
            egui::Color32::from_rgb(20, 22, 26),
            egui::Color32::from_rgb(201, 205, 212),
            egui::Color32::from_rgb(51, 80, 122),
        ),
    };
    Some(Palette {
        background,
        text,
        selection,
    })
}

fn rgb([r, g, b]: [u8; 3]) -> egui::Color32 {
    egui::Color32::from_rgb(r, g, b)
}

/// 背景到文字色之间的线性混合，用于生成控件底色。
/// `gamma_multiply` 对深色文字会得到近黑底色，导致按钮文字看不清。
fn blend(from: egui::Color32, to: egui::Color32, t: f32) -> egui::Color32 {
    let t = t.clamp(0.0, 1.0);
    let [r1, g1, b1, a1] = from.to_array();
    let [r2, g2, b2, _] = to.to_array();
    let mix = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t).round() as u8;
    egui::Color32::from_rgba_unmultiplied(mix(r1, r2), mix(g1, g2), mix(b1, b2), a1)
}

fn is_dark(color: egui::Color32) -> bool {
    let [r, g, b, _] = color.to_array();
    r as f32 * 0.299 + g as f32 * 0.587 + b as f32 * 0.114 < 128.0
}

/// 设置弹窗里的常用背景色板（名称 + RGB）。
const BACKGROUND_SWATCHES: &[(&str, [u8; 3])] = &[
    ("纯白", [255, 255, 255]),
    ("纸白", [247, 244, 236]),
    ("米黄", [242, 228, 201]),
    ("豆绿", [226, 238, 226]),
    ("淡蓝", [222, 234, 246]),
    ("浅灰", [224, 224, 224]),
    ("中灰", [128, 128, 128]),
    ("深灰", [64, 66, 70]),
    ("炭黑", [28, 30, 34]),
    ("纯黑", [0, 0, 0]),
];

impl Palette {
    fn shade(&self, towards_text: f32) -> egui::Color32 {
        blend(self.background, self.text, towards_text)
    }

    fn link(&self) -> egui::Color32 {
        if is_dark(self.background) {
            egui::Color32::from_rgb(122, 170, 240)
        } else {
            egui::Color32::from_rgb(38, 92, 172)
        }
    }

    fn border(&self) -> egui::Color32 {
        self.shade(0.28)
    }

    /// 把配色写入视觉样式：文字、选区、链接、按钮与输入框底色都跟着主题走。
    pub fn apply(&self, visuals: &mut egui::Visuals) {
        visuals.override_text_color = Some(self.text);
        visuals.selection.bg_fill = self.selection;
        // egui 会用 selection.stroke 的颜色重绘选中文字：取选区背景的反色，
        // 保证文字在任意主题/高亮底色下都清晰；取消选中后自动恢复原色。
        let [r, g, b, _] = self.selection.to_array();
        visuals.selection.stroke = egui::Stroke::new(
            1.0,
            egui::Color32::from_rgb(255 - r, 255 - g, 255 - b),
        );
        visuals.hyperlink_color = self.link();
        visuals.text_cursor.stroke = egui::Stroke::new(1.5, self.text);
        visuals.panel_fill = self.background;
        visuals.window_fill = self.background;
        visuals.window_stroke = egui::Stroke::new(1.0, self.border());
        visuals.extreme_bg_color = self.shade(0.06);
        visuals.faint_bg_color = self.shade(0.05);
        visuals.code_bg_color = self.shade(0.08);

        let widgets = &mut visuals.widgets;
        widgets.noninteractive.bg_fill = self.background;
        widgets.noninteractive.weak_bg_fill = self.background;
        widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0, self.shade(0.16));
        widgets.noninteractive.fg_stroke = egui::Stroke::new(1.0, self.text);
        // 按钮/下拉框底色比背景更靠近文字色，前景始终是文字色，保证对比度。
        widgets.inactive.weak_bg_fill = self.shade(0.12);
        widgets.inactive.bg_fill = self.shade(0.12);
        widgets.inactive.bg_stroke = egui::Stroke::new(1.0, self.border());
        widgets.inactive.fg_stroke = egui::Stroke::new(1.0, self.text);
        widgets.hovered.weak_bg_fill = self.shade(0.18);
        widgets.hovered.bg_fill = self.shade(0.18);
        widgets.hovered.bg_stroke = egui::Stroke::new(1.0, self.shade(0.38));
        widgets.hovered.fg_stroke = egui::Stroke::new(1.5, self.text);
        widgets.active.weak_bg_fill = self.shade(0.26);
        widgets.active.bg_fill = self.shade(0.26);
        widgets.active.bg_stroke = egui::Stroke::new(1.0, self.shade(0.46));
        widgets.active.fg_stroke = egui::Stroke::new(1.5, self.text);
        widgets.open = widgets.active;
    }
}

/// 主题预设 + 自定义颜色合成出的最终配色；全为默认时返回 `None`。
fn resolved_palette(
    visuals: &egui::Visuals,
    theme: ReaderTheme,
    background: Option<[u8; 3]>,
    text_color: Option<[u8; 3]>,
) -> Option<Palette> {
    let preset = palette(theme);
    if preset.is_none() && background.is_none() && text_color.is_none() {
        return None;
    }
    let background = background
        .map(rgb)
        .or_else(|| preset.as_ref().map(|palette| palette.background))
        .unwrap_or(visuals.panel_fill);
    // 只自定义背景色时按明暗自动选文字色，避免沿用应用主题的浅色/深色文字而看不清。
    let text = match text_color {
        Some(color) => rgb(color),
        None => match preset.as_ref() {
            Some(palette) => palette.text,
            None if background != visuals.panel_fill => {
                if is_dark(background) {
                    egui::Color32::from_rgb(232, 232, 232)
                } else {
                    egui::Color32::from_rgb(28, 28, 28)
                }
            }
            None => visuals.text_color(),
        },
    };
    let selection = preset
        .as_ref()
        .map(|palette| palette.selection)
        .unwrap_or_else(|| blend(background, text, 0.3));
    Some(Palette {
        background,
        text,
        selection,
    })
}

/// 颜色选择器用的当前底色/文字色。
pub fn resolved_colors(
    ui: &egui::Ui,
    theme: ReaderTheme,
    background: Option<[u8; 3]>,
    text_color: Option<[u8; 3]>,
) -> (egui::Color32, egui::Color32) {
    resolved_palette(ui.visuals(), theme, background, text_color)
        .map(|palette| (palette.background, palette.text))
        .unwrap_or_else(|| (ui.visuals().panel_fill, ui.visuals().text_color()))
}

/// 给阅读页铺主题底色并覆盖文字/选区/控件颜色；`Follow` 且无自定义颜色时不改变全局视觉。
pub fn apply_reader_theme(
    ui: &mut egui::Ui,
    theme: ReaderTheme,
    background: Option<[u8; 3]>,
    text_color: Option<[u8; 3]>,
) {
    let Some(palette) = resolved_palette(ui.visuals(), theme, background, text_color) else {
        return;
    };
    ui.painter()
        .rect_filled(ui.max_rect(), 0.0, palette.background);
    palette.apply(&mut ui.style_mut().visuals);
}

impl SepiolaApp {
    /// 阅读设置对话框：主题、字体（内置/系统）、字号、行间距、页边距、配色与词典地址。
    pub fn reader_settings_window(&mut self, ctx: &egui::Context) {
        if !self.library.reader.settings_open {
            return;
        }
        let style = ctx.style_of(ctx.theme());
        let palette = resolved_palette(
            &style.visuals,
            self.library.reader.theme,
            self.library.reader.background,
            self.library.reader.text_color,
        );
        let frame = match &palette {
            Some(palette) => egui::Frame::window(&style)
                .fill(palette.background)
                .stroke(egui::Stroke::new(1.0, palette.border())),
            None => egui::Frame::window(&style),
        };
        let mut open = true;
        let mut changed = false;
        let mut close = false;
        egui::Window::new("阅读设置")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(480.0)
            .frame(frame)
            .show(ctx, |ui| {
                if let Some(palette) = &palette {
                    palette.apply(&mut ui.style_mut().visuals);
                }
                ui.set_min_width(440.0);

                egui::Grid::new("reader_settings_grid")
                    .num_columns(2)
                    .spacing([16.0, 10.0])
                    .show(ui, |ui| {
                        ui.label("主题");
                        let mut theme = self.library.reader.theme;
                        egui::ComboBox::from_id_salt("reader_theme")
                            .width(160.0)
                            .selected_text(theme.label())
                            .show_ui(ui, |ui| {
                                for choice in ReaderTheme::ALL {
                                    ui.selectable_value(&mut theme, choice, choice.label());
                                }
                            });
                        if theme != self.library.reader.theme {
                            self.library.reader.theme = theme;
                            changed = true;
                        }
                        ui.end_row();

                        ui.label("字体");
                        ui.vertical(|ui| {
                            let current = match self.library.reader.custom_font.as_deref() {
                                Some(path) => base_name(Path::new(path)),
                                None => self.library.reader.font.label().to_string(),
                            };
                            egui::ComboBox::from_id_salt("reader_font")
                                .width(220.0)
                                .selected_text(current)
                                // 弹层里有搜索框与字体列表，点击选中项时不要直接关闭。
                                .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
                                .show_ui(ui, |ui| {
                                    for choice in ReaderFont::ALL {
                                        let selected = self.library.reader.custom_font.is_none()
                                            && self.library.reader.font == choice;
                                        if ui.selectable_label(selected, choice.label()).clicked() {
                                            self.library.reader.custom_font = None;
                                            if self.library.reader.font != choice {
                                                self.library.reader.font = choice;
                                                self.library.reader.chapter_heights.clear();
                                                changed = true;
                                            }
                                        }
                                    }
                                    ui.separator();
                                    ui.label("系统字体");
                                    ui.add(
                                        egui::TextEdit::singleline(
                                            &mut self.library.reader.font_query,
                                        )
                                        .hint_text("搜索字体名…"),
                                    );
                                    let query = self.library.reader.font_query.trim().to_lowercase();
                                    egui::ScrollArea::vertical()
                                        .id_salt("reader_font_list")
                                        .max_height(220.0)
                                        .show(ui, |ui| {
                                            for (name, path) in system_fonts() {
                                                if !query.is_empty()
                                                    && !name.to_lowercase().contains(&query)
                                                {
                                                    continue;
                                                }
                                                let path_text = path.display().to_string();
                                                let selected = self
                                                    .library
                                                    .reader
                                                    .custom_font
                                                    .as_deref()
                                                    == Some(path_text.as_str());
                                                if ui
                                                    .selectable_label(selected, name.as_str())
                                                    .clicked()
                                                {
                                                    self.library.reader.custom_font =
                                                        Some(path_text);
                                                    self.library
                                                        .reader
                                                        .chapter_heights
                                                        .clear();
                                                    changed = true;
                                                }
                                            }
                                        });
                                });
                            if self.library.reader.custom_font.is_some()
                                && pointing_hand(ui.button("恢复内置字体")).clicked()
                            {
                                self.library.reader.custom_font = None;
                                self.library.reader.chapter_heights.clear();
                                changed = true;
                            }
                        });
                        ui.end_row();

                        ui.label("字号");
                        let mut font_size = self.library.reader.font_size.round();
                        if ui
                            .add(
                                egui::DragValue::new(&mut font_size)
                                    .range(12.0..=32.0)
                                    .speed(1.0)
                                    .max_decimals(0)
                                    .suffix(" px"),
                            )
                            .changed()
                        {
                            self.library.reader.font_size = font_size.round();
                            self.library.reader.chapter_heights.clear();
                            changed = true;
                        }
                        ui.end_row();

                        ui.label("行间距");
                        let mut line_spacing = self.library.reader.line_spacing;
                        if ui
                            .add(
                                egui::DragValue::new(&mut line_spacing)
                                    .range(1.0..=2.6)
                                    .speed(0.02)
                                    .fixed_decimals(2)
                                    .suffix(" 倍"),
                            )
                            .changed()
                        {
                            self.library.reader.line_spacing = line_spacing;
                            self.library.reader.chapter_heights.clear();
                            changed = true;
                        }
                        ui.end_row();

                        ui.label("段间距");
                        let mut paragraph_spacing = self.library.reader.paragraph_spacing;
                        if ui
                            .add(
                                egui::DragValue::new(&mut paragraph_spacing)
                                    .range(0.0..=48.0)
                                    .speed(1.0)
                                    .max_decimals(0)
                                    .suffix(" px"),
                            )
                            .changed()
                        {
                            self.library.reader.paragraph_spacing = paragraph_spacing;
                            self.library.reader.chapter_heights.clear();
                            changed = true;
                        }
                        ui.end_row();

                        ui.label("页边距");
                        let mut margin = self.library.reader.margin;
                        if ui
                            .add(
                                egui::DragValue::new(&mut margin)
                                    .range(0.0..=120.0)
                                    .speed(1.0)
                                    .suffix(" px"),
                            )
                            .changed()
                        {
                            self.library.reader.margin = margin;
                            self.library.reader.chapter_heights.clear();
                            changed = true;
                        }
                        ui.end_row();

                        ui.label("背景色");
                        ui.horizontal(|ui| {
                            let (current_bg, current_text) = resolved_colors(
                                ui,
                                self.library.reader.theme,
                                self.library.reader.background,
                                self.library.reader.text_color,
                            );
                            let [r, g, b, _] = current_bg.to_array();
                            let mut background =
                                self.library.reader.background.unwrap_or([r, g, b]);
                            if ui.color_edit_button_srgb(&mut background).changed() {
                                self.library.reader.background = Some(background);
                                changed = true;
                            }
                            let [r, g, b, _] = current_text.to_array();
                            let mut text_color =
                                self.library.reader.text_color.unwrap_or([r, g, b]);
                            if ui.color_edit_button_srgb(&mut text_color).changed() {
                                self.library.reader.text_color = Some(text_color);
                                changed = true;
                            }
                            ui.label("文字色");
                        });
                        ui.end_row();

                        ui.label("调色板");
                        ui.horizontal_wrapped(|ui| {
                            let (current_bg, _) = resolved_colors(
                                ui,
                                self.library.reader.theme,
                                self.library.reader.background,
                                self.library.reader.text_color,
                            );
                            for &(name, swatch) in BACKGROUND_SWATCHES {
                                let color = rgb(swatch);
                                let selected = current_bg == color;
                                let stroke = if selected {
                                    egui::Stroke::new(2.0, ui.visuals().text_color())
                                } else {
                                    egui::Stroke::new(1.0, blend(color, ui.visuals().window_fill, 0.4))
                                };
                                let response = ui.add(
                                    egui::Button::new("")
                                        .fill(color)
                                        .stroke(stroke)
                                        .min_size(egui::vec2(24.0, 24.0)),
                                );
                                if response.clicked() {
                                    self.library.reader.background = Some(swatch);
                                    changed = true;
                                }
                                response.on_hover_text(name);
                            }
                        });
                        ui.end_row();
                    });

                ui.separator();
                ui.horizontal(|ui| {
                    if ui
                        .checkbox(&mut self.library.reader.show_annotations, "显示分析标注")
                        .changed()
                    {
                        changed = true;
                    }
                    for kind in sepiola::reader::analyze::AnnotationKind::ALL {
                        ui.colored_label(
                            crate::reader_stream::annotation_color(kind),
                            kind.label(),
                        );
                    }
                });
                ui.label(
                    egui::RichText::new(
                        "「分析本页」后，可见段落的词组、表达、句式、术语、专有名词与语法点会加彩标，\
                         点击查看详情并入笔记本。",
                    )
                    .small()
                    .weak(),
                );

                ui.separator();
                if self.dictionary_settings_ui(ui) {
                    self.save_user_settings();
                }

                ui.separator();
                ui.horizontal(|ui| {
                    if pointing_hand(ui.button("完成")).clicked() {
                        close = true;
                    }
                    if (self.library.reader.background.is_some()
                        || self.library.reader.text_color.is_some())
                        && pointing_hand(ui.button("配色重置")).clicked()
                    {
                        self.library.reader.background = None;
                        self.library.reader.text_color = None;
                        changed = true;
                    }
                    ui.weak("设置会自动保存");
                });
            });

        if changed {
            ctx.request_repaint();
            self.save_reader_position();
        }
        if !open || close || ctx.input(|input| input.key_pressed(egui::Key::Escape)) {
            self.library.reader.settings_open = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn luminance(color: egui::Color32) -> f32 {
        let [r, g, b, _] = color.to_array();
        r as f32 * 0.299 + g as f32 * 0.587 + b as f32 * 0.114
    }

    #[test]
    fn button_fill_keeps_text_contrast() {
        for theme in [ReaderTheme::Paper, ReaderTheme::Sepia, ReaderTheme::Night] {
            let palette = palette(theme).expect("preset");
            let fill = palette.shade(0.12);
            assert_ne!(fill, palette.background, "{theme:?} 按钮底色未跟随主题");
            assert!(
                (luminance(fill) - luminance(palette.text)).abs() > 100.0,
                "{theme:?} 按钮文字与底色对比度不足"
            );
        }
    }

    #[test]
    fn custom_background_derives_contrasting_text() {
        let visuals = egui::Visuals::dark();
        let palette = resolved_palette(&visuals, ReaderTheme::Follow, Some([255, 255, 255]), None)
            .expect("custom background should produce a palette");
        assert!(is_dark(palette.text), "白色背景应自动配深色文字");
    }
}
