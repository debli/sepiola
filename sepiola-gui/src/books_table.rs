//! 书籍元信息表格。

use eframe::egui;
use sepiola::book::BookMeta;
use sepiola::library::LibraryEntry;

fn dash<T: std::fmt::Display>(value: &Option<T>) -> String {
    value
        .as_ref()
        .map(|value| value.to_string())
        .unwrap_or_else(|| "—".to_string())
}

pub fn show_books(ui: &mut egui::Ui, entries: &[LibraryEntry], books: &[usize]) {
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
                            let entry = &entries[books[row - 1]];
                            let empty = BookMeta::default();
                            let meta = entry.book_meta.as_ref().unwrap_or(&empty);
                            let file_name = entry
                                .rel_path
                                .file_name()
                                .map(|name| name.to_string_lossy().into_owned())
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
}
