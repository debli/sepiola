//! 书籍浏览页：表格展示内容级抽取的元数据（书名/作者/出版社/年份/译者/版本/ISBN）。

use std::sync::atomic::Ordering;

use eframe::egui;
use sepiola::book::BookMeta;
use sepiola::library::FileKind;

use crate::assets::pointing_hand;
use crate::library::LibraryPage;
use crate::state::SepiolaApp;

/// 可选字段转占位文本。
fn dash<T: std::fmt::Display>(v: &Option<T>) -> String {
    v.as_ref()
        .map(|v| v.to_string())
        .unwrap_or_else(|| "—".to_string())
}

/// 按书名（缺省用相对路径）排序后的书籍条目。
fn sorted_books(app: &SepiolaApp) -> Vec<&sepiola::library::LibraryEntry> {
    let mut books: Vec<_> = app
        .library
        .index
        .entries
        .iter()
        .filter(|entry| entry.kind == FileKind::Book)
        .collect();
    books.sort_by_cached_key(|entry| {
        entry
            .book_meta
            .as_ref()
            .and_then(|meta| meta.title.clone())
            .unwrap_or_else(|| entry.rel_path.display().to_string())
    });
    books
}

impl SepiolaApp {
    pub fn books_ui(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if pointing_hand(ui.button("← 返回总览")).clicked() {
                self.library.page = LibraryPage::Overview;
            }
            ui.label(egui::RichText::new("书籍元数据（内容级抽取）").size(16.0));
        });
        if !self.library.message.is_empty() {
            ui.label(self.library.message.as_str());
        }
        ui.separator();

        let books = sorted_books(self);
        if books.is_empty() {
            ui.label("库中暂无书籍；扫描归类后自动解析 EPUB/FB2/MOBI/PDF 内容抽取元数据。");
            return;
        }
        let missing = books
            .iter()
            .filter(|entry| entry.book_meta.is_none())
            .count();
        ui.label(format!(
            "共 {} 本；{} 本尚无内容级元信息。",
            books.len(),
            missing
        ));
        let can_backfill_metadata = missing > 0 && self.library.index.version < 9;
        let start_metadata_scan = if can_backfill_metadata {
            pointing_hand(ui.add_enabled(
                !self.library.scanning,
                egui::Button::new("扫描并补全元信息"),
            ))
            .clicked()
        } else {
            false
        };
        let cancel_metadata_scan = self.library.scanning
            && pointing_hand(ui.button("取消扫描")).clicked();
        if self.library.scanning {
            let (done, total) = self.library.progress;
            let fraction = if total == 0 {
                0.0
            } else {
                done as f32 / total as f32
            };
            ui.add(
                egui::ProgressBar::new(fraction)
                    .show_percentage()
                    .text(format!("{done}/{total}")),
            );
        }
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .id_salt("books_view")
            .show_rows(
                ui,
                ui.text_style_height(&egui::TextStyle::Body) + 4.0,
                books.len() + 1,
                |ui, rows| {
                    let row_spacing = ui.spacing().item_spacing.y;
                    egui::Grid::new("books_grid")
                        .num_columns(8)
                        .striped(true)
                        .min_row_height(ui.text_style_height(&egui::TextStyle::Body) + 4.0)
                        .max_col_width(210.0)
                        .spacing([14.0, row_spacing])
                        .start_row(rows.start)
                        .show(ui, |ui| {
                            for row in rows {
                                if row == 0 {
                                    for header in [
                                        "书名",
                                        "作者",
                                        "出版社",
                                        "年份",
                                        "译者",
                                        "版本",
                                        "ISBN",
                                        "文件",
                                    ] {
                                        ui.add(
                                            egui::Label::new(
                                                egui::RichText::new(header).strong().size(15.0),
                                            )
                                            .truncate(),
                                        );
                                    }
                                    ui.end_row();
                                    continue;
                                }
                                let entry = books[row - 1];
                                let empty = BookMeta::default();
                                let meta = entry.book_meta.as_ref().unwrap_or(&empty);
                                let file_name = entry
                                    .rel_path
                                    .file_name()
                                    .map(|n| n.to_string_lossy().into_owned())
                                    .unwrap_or_default();
                                let title = meta.title.clone().unwrap_or_else(|| {
                                    entry
                                        .rel_path
                                        .file_stem()
                                        .map(|name| name.to_string_lossy().into_owned())
                                        .unwrap_or_default()
                                });
                                ui.add(egui::Label::new(title).truncate());
                                for value in [
                                    dash(&meta.author),
                                    dash(&meta.publisher),
                                    dash(&meta.year),
                                    dash(&meta.translator),
                                    dash(&meta.edition),
                                    dash(&meta.isbn),
                                ] {
                                    ui.add(egui::Label::new(value).truncate());
                                }
                                ui.add(
                                    egui::Label::new(
                                        egui::RichText::new(file_name)
                                            .weak()
                                            .size(12.0)
                                            .color(egui::Color32::from_gray(170)),
                                    )
                                    .truncate(),
                                )
                                .on_hover_text(entry.rel_path.display().to_string());
                                ui.end_row();
                            }
                        });
                },
            );
        drop(books);
        if start_metadata_scan {
            self.start_library_scan();
        }
        if cancel_metadata_scan {
            self.library.cancel.store(true, Ordering::Relaxed);
        }
    }
}
