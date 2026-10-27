//! 书籍浏览页：表格展示内容级抽取的元数据（书名/作者/出版社/年份/译者/版本/ISBN）。

use std::sync::atomic::Ordering;

use eframe::egui;
use sepiola::library::FileKind;

use crate::assets::pointing_hand;
use crate::book_filters::{field_value, BookField};
use crate::books_table::show_books;
use crate::library::LibraryPage;
use crate::state::SepiolaApp;

/// 按书名（缺省用相对路径）排序后的索引。
fn sorted_books(app: &SepiolaApp) -> Vec<usize> {
    let mut books: Vec<_> = app
        .library
        .index
        .entries
        .iter()
        .enumerate()
        .filter_map(|(index, entry)| (entry.kind == FileKind::Book).then_some(index))
        .collect();
    books.sort_by_cached_key(|index| {
        field_value(&app.library.index.entries[*index], BookField::Title).unwrap_or_default()
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

        let all_books = sorted_books(self);
        if all_books.is_empty() {
            ui.label("库中暂无书籍；先在总览扫描文件类型，再进入书籍页提取元数据。");
            return;
        }
        if self.library.book_search.facets.is_empty() {
            self.library
                .book_search
                .refresh_facets(&self.library.index.entries);
        }
        let missing = all_books
            .iter()
            .filter(|index| self.library.index.entries[**index].book_meta.is_none())
            .count();
        let unattempted = all_books
            .iter()
            .filter(|index| {
                let entry = &self.library.index.entries[**index];
                entry.book_meta.is_none() && !entry.book_metadata_attempted
            })
            .count();
        let failed = missing - unattempted;
        ui.label(format!(
            "共 {} 本；{} 本未尝试提取，{} 本提取失败。",
            all_books.len(), unattempted, failed
        ));
        let start_metadata_scan = pointing_hand(ui.add_enabled(
            !self.library.scanning && unattempted > 0,
            egui::Button::new(format!("提取未处理书籍（{unattempted}）")),
        ))
        .clicked();
        let retry_failed_scan = pointing_hand(ui.add_enabled(
            !self.library.scanning && failed > 0,
            egui::Button::new(format!("重试失败项（{failed}）")),
        ))
        .clicked();
        let cancel_metadata_scan =
            self.library.scanning && pointing_hand(ui.button("取消扫描")).clicked();
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

        let books = self.book_search_ui(ui, &all_books);
        let total_books = self
            .library
            .index
            .entries
            .iter()
            .filter(|entry| entry.kind == FileKind::Book)
            .count();
        ui.label(format!("显示 {} / {} 本", books.len(), total_books));
        show_books(ui, &self.library.index.entries, &books);
        if start_metadata_scan {
            self.start_book_metadata_scan(false);
        }
        if retry_failed_scan {
            self.start_book_metadata_scan(true);
        }
        if cancel_metadata_scan {
            self.library.cancel.store(true, Ordering::Relaxed);
        }
    }
}
