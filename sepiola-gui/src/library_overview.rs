//! 库第一级总览页：目录选择、扫描控制与各文件类型计数卡片。
//! 点击"图片"卡片进入图片浏览页（`library_view`），"书籍"卡片进入书籍元数据页
//! （`books_view`），其余类型待后续里程碑实现。

use std::sync::atomic::Ordering;

use eframe::egui;
use sepiola::library::{self, FileKind};

use crate::assets::{pick_dir, pointing_hand};
use crate::library::LibraryPage;
use crate::state::SepiolaApp;

/// 类型卡片配色（深色底 + 浅色文字，区分各类型）。
fn kind_accent(kind: FileKind) -> egui::Color32 {
    let (r, g, b) = match kind {
        FileKind::Image => (58, 110, 165),
        FileKind::Video => (150, 70, 90),
        FileKind::Audio => (80, 130, 90),
        FileKind::Recording => (60, 120, 120),
        FileKind::Document => (105, 105, 120),
        FileKind::Book => (140, 110, 60),
        FileKind::Driver => (95, 90, 130),
        FileKind::Installer => (70, 105, 140),
        FileKind::Archive => (110, 95, 80),
        FileKind::Project => (65, 115, 105),
        FileKind::Game => (130, 85, 120),
        FileKind::Other => (90, 90, 90),
    };
    egui::Color32::from_rgb(r, g, b)
}

impl SepiolaApp {
    pub fn overview_ui(&mut self, ui: &mut egui::Ui) {
        let mut selected_root = None;
        ui.horizontal(|ui| {
            if let Some(dir) = pick_dir(ui, "库目录", &mut self.library.root) {
                selected_root = Some(dir);
            }
            if !self.user_settings.recent_library_roots.is_empty() {
                let response = egui::ComboBox::from_label("最近书库")
                    .selected_text(if self.library.root.is_empty() {
                        "选择书库"
                    } else {
                        &self.library.root
                    })
                    .show_ui(ui, |ui| {
                        for root in &self.user_settings.recent_library_roots {
                            if pointing_hand(
                                ui.selectable_label(root == &self.library.root, root),
                            )
                            .clicked()
                            {
                                selected_root = Some(std::path::PathBuf::from(root));
                            }
                        }
                    });
                pointing_hand(response.response);
            }
            let scan = egui::Button::new(if self.library.scanning { "索引中…" } else { "扫描文件类型" })
                .min_size(egui::vec2(120.0, 32.0));
            if pointing_hand(ui.add_enabled(!self.library.scanning, scan)).clicked() {
                self.start_library_scan();
            }
            if self.library.scanning
                && ui
                    .button("取消扫描")
                    .on_hover_text("未处理的文件留待下次扫描")
                    .on_hover_cursor(egui::CursorIcon::PointingHand)
                    .clicked()
            {
                self.library.cancel.store(true, Ordering::Relaxed);
            }
        });
        if let Some(root) = selected_root {
            self.select_library_root(root);
        }
        if self.library.scanning {
            let (done, total) = self.library.progress;
            let fraction = if total == 0 { 0.0 } else { done as f32 / total as f32 };
            ui.add(
                egui::ProgressBar::new(fraction)
                    .show_percentage()
                    .text(format!("{done}/{total}")),
            );
        }
        if !self.library.message.is_empty() {
            ui.label(self.library.message.as_str());
        }
        ui.separator();

        let counts = self.library.index.kind_counts();
        let total = self.library.index.entries.len();
        ui.label(format!("共 {total} 个条目；点击类型卡片进入浏览页（当前已实现「图片」「书籍」）。"));
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .id_salt("lib_overview")
            .show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing = egui::vec2(12.0, 12.0);
                    for kind in library::ALL_KINDS {
                        let count = counts.get(kind).copied().unwrap_or(0);
                        let enabled =
                            matches!(*kind, FileKind::Image | FileKind::Book)
                                && count > 0
                                && !self.library.scanning;
                        let text = format!("{}\n{} 项", kind.label(), count);
                        let button = egui::Button::new(
                            egui::RichText::new(text).size(15.0).color(egui::Color32::WHITE),
                        )
                        .fill(kind_accent(*kind))
                        .min_size(egui::vec2(112.0, 76.0));
                        let hover = match kind {
                            FileKind::Image => {
                                if count > 0 {
                                    "进入图片库"
                                } else {
                                    "先扫描文件类型后再进入"
                                }
                            }
                            FileKind::Book => {
                                if count > 0 {
                                    "进入书籍库（内容级元数据）"
                                } else {
                                    "先扫描文件类型后再进入"
                                }
                            }
                            _ => "浏览页待后续里程碑实现",
                        };
                        if pointing_hand(ui.add_enabled(enabled, button))
                            .on_hover_text(hover)
                            .clicked()
                        {
                            self.library.page = match kind {
                                FileKind::Image => {
                                    self.start_image_classification();
                                    LibraryPage::Images
                                }
                                FileKind::Book => {
                                    self.start_book_metadata_scan(false);
                                    LibraryPage::Books
                                }
                                _ => LibraryPage::Overview,
                            };
                        }
                    }
                });
            });
    }
}
