//! 书籍浏览页：表格展示内容级抽取的元数据（书名/作者/出版社/年份/译者/版本/ISBN）。

use std::sync::atomic::Ordering;

use eframe::egui;
use sepiola::library::FileKind;

use crate::assets::pointing_hand;
use crate::book_filters::{field_value, BookField};
use crate::books_table::show_books;
use crate::library::{non_empty_root, LibraryPage};
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

/// 最近阅读的书（条目索引，章节序号，上次章节标题），按上次阅读时间倒序。
fn recent_books(app: &SepiolaApp) -> Vec<(usize, usize, String)> {
    let Some(root) = non_empty_root(&app.library.root) else {
        return Vec::new();
    };
    let state = sepiola::reader::ReadingState::load(&root);
    let mut positions: Vec<_> = state
        .positions
        .iter()
        .map(|(rel, position)| {
            (
                position.opened_at,
                rel.clone(),
                position.chapter,
                position.title.clone(),
            )
        })
        .collect();
    positions.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    positions
        .into_iter()
        .filter_map(|(_, rel, chapter, title)| {
            let index = app.library.index.entries.iter().position(|entry| {
                entry.rel_path.to_string_lossy().replace('\\', "/") == rel
            })?;
            Some((index, chapter, title))
        })
        .take(8)
        .collect()
}

impl SepiolaApp {
    pub fn books_ui(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if pointing_hand(ui.button("← 返回总览")).clicked() {
                self.library.page = LibraryPage::Overview;
            }
            ui.label(egui::RichText::new("书籍（元数据 · 点击书名或「阅读」打开正文）").size(16.0));
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
        let mut open_recent = None;
        let recent = recent_books(self);
        if !recent.is_empty() {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("最近阅读").strong());
                let recent_title =
                    |entry: &sepiola::library::LibraryEntry, chapter: usize, title: &str| {
                        let name = field_value(entry, BookField::Title)
                            .filter(|title| !title.trim().is_empty())
                            .unwrap_or_else(|| entry.rel_path.display().to_string());
                        let position = if title.trim().is_empty() {
                            format!("第 {} 节", chapter + 1)
                        } else {
                            title.to_string()
                        };
                        format!("{name}（{position}）")
                    };
                let selected = recent
                    .first()
                    .map(|(index, chapter, title)| {
                        recent_title(&self.library.index.entries[*index], *chapter, title)
                    })
                    .unwrap_or_default();
                egui::ComboBox::from_id_salt("books_recent")
                    .selected_text(selected)
                    .width(360.0)
                    .show_ui(ui, |ui| {
                        for (index, chapter, title) in recent {
                            let entry = &self.library.index.entries[index];
                            let label = recent_title(entry, chapter, &title);
                            if pointing_hand(ui.selectable_label(false, label)).clicked() {
                                open_recent = Some(entry.rel_path.clone());
                            }
                        }
                    });
            });
            ui.separator();
        }
        if let Some(rel) = open_recent {
            self.open_book(rel);
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
            all_books.len(),
            unattempted,
            failed
        ));
        let mut start_metadata_scan = false;
        let mut retry_failed_scan = false;
        let mut cancel_metadata_scan = false;
        let cancel_requested = self.library.cancel.load(Ordering::Relaxed);
        ui.horizontal(|ui| {
            start_metadata_scan = pointing_hand(ui.add_enabled(
                !self.library.scanning && unattempted > 0,
                egui::Button::new(format!("提取未处理书籍（{unattempted}）")),
            ))
            .clicked();
            retry_failed_scan = pointing_hand(ui.add_enabled(
                !self.library.scanning && failed > 0,
                egui::Button::new(format!("重试失败项（{failed}）")),
            ))
            .clicked();
            if self.library.scanning {
                cancel_metadata_scan = pointing_hand(ui.add_enabled(
                    !cancel_requested,
                    egui::Button::new(if cancel_requested {
                        "正在停止…"
                    } else {
                        "取消扫描"
                    }),
                ))
                .clicked();
            }
        });
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
        let clicked = show_books(ui, &self.library.index.entries, &books);
        if start_metadata_scan {
            self.start_book_metadata_scan(false);
        }
        if retry_failed_scan {
            self.start_book_metadata_scan(true);
        }
        if cancel_metadata_scan {
            self.request_library_cancel();
        }
        if let Some(index) = clicked {
            let rel = self.library.index.entries[index].rel_path.clone();
            self.open_book(rel);
        }
    }
}
