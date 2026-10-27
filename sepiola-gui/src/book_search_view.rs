//! 书籍搜索控件、分面选择与结果组合。

use std::path::PathBuf;

use eframe::egui;

use crate::assets::pointing_hand;
use crate::book_filters::{field_label, matches_book, BOOK_FACET_FIELDS};
use crate::state::SepiolaApp;

impl SepiolaApp {
    pub fn book_search_ui(&mut self, ui: &mut egui::Ui, all_books: &[usize]) -> Vec<usize> {
        let mut start_content_search = false;
        let mut reset_content_search = false;
        let searching = self.library.book_search.content.is_searching();
        let progress = self.library.book_search.content.progress();
        ui.horizontal(|ui| {
            let response = ui.add(
                egui::TextEdit::singleline(&mut self.library.book_search.query)
                    .hint_text("搜索所有元信息")
                    .desired_width(260.0),
            );
            reset_content_search |= response.changed();
            if ui
                .checkbox(&mut self.library.book_search.search_content, "搜索正文")
                .changed()
                && !self.library.book_search.search_content
            {
                reset_content_search = true;
            }
            if pointing_hand(ui.add_enabled(
                self.library.book_search.search_content
                    && !self.library.book_search.query.trim().is_empty(),
                egui::Button::new("开始正文搜索"),
            ))
            .clicked()
            {
                start_content_search = true;
            }
            if searching {
                ui.spinner();
                ui.label(format!("正文 {}/{}", progress.0, progress.1));
                if pointing_hand(ui.small_button("取消")).clicked() {
                    reset_content_search = true;
                }
            }
        });
        if reset_content_search {
            self.library.book_search.content.reset();
        }

        let facets = &self.library.book_search.facets;
        let filters = &mut self.library.book_search.filters;
        ui.horizontal_wrapped(|ui| {
            for field in BOOK_FACET_FIELDS {
                let options = facets.get(&field).map(Vec::as_slice).unwrap_or(&[]);
                let selected = filters.entry(field).or_default();
                let selected_text = if selected.is_empty() {
                    format!("{}：全部", field_label(field))
                } else {
                    format!("{}：{}", field_label(field), selected)
                };
                let combo = egui::ComboBox::from_id_salt(format!("book_filter_{field:?}"))
                    .selected_text(selected_text)
                    .show_ui(ui, |ui| {
                        pointing_hand(ui.selectable_value(selected, String::new(), "全部"));
                        for (value, count) in options {
                            pointing_hand(ui.selectable_value(
                                selected,
                                value.clone(),
                                format!("{value}（{count}）"),
                            ));
                        }
                    });
                pointing_hand(combo.response);
            }
        });

        if start_content_search {
            let root = PathBuf::from(self.library.root.trim());
            let paths = all_books
                .iter()
                .map(|index| self.library.index.entries[*index].rel_path.clone())
                .collect();
            self.library
                .book_search
                .content
                .start(root, paths, &self.library.book_search.query);
        }

        let query = self.library.book_search.query.trim().to_lowercase();
        let content_matches = self.library.book_search.content.matches_for(&query);
        let filters = &self.library.book_search.filters;
        let search_content = self.library.book_search.search_content;
        let results: Vec<usize> = all_books
            .iter()
            .copied()
            .filter(|index| {
                let entry = &self.library.index.entries[*index];
                matches_book(
                    entry,
                    filters,
                    &query,
                    search_content,
                    content_matches.is_some_and(|matches| matches.contains(&entry.rel_path)),
                )
            })
            .collect();
        ui.label(format!("显示 {} / {} 本", results.len(), all_books.len()));
        if search_content
            && !query.is_empty()
            && content_matches.is_none()
            && !self.library.book_search.content.is_searching()
        {
            ui.weak("点击「开始正文搜索」检索 EPUB、PDF 文本层、FB2 和未加密 MOBI/AZW 正文。");
        }
        results
    }
}
