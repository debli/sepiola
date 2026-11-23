//! Audio metadata browser and lazy extraction controls.

use eframe::egui;
use sepiola::audio::AudioMeta;
use sepiola::library::{FileKind, LibraryEntry};

use crate::assets::pointing_hand;
use crate::library::LibraryPage;
use crate::state::SepiolaApp;

fn sorted_audio(app: &SepiolaApp) -> Vec<usize> {
    let mut audio: Vec<_> = app
        .library
        .index
        .entries
        .iter()
        .enumerate()
        .filter_map(|(index, entry)| {
            matches!(entry.kind, FileKind::Audio | FileKind::Recording).then_some(index)
        })
        .collect();
    audio.sort_by(|left, right| {
        let left_entry = &app.library.index.entries[*left];
        let right_entry = &app.library.index.entries[*right];
        let left_meta = left_entry.audio_meta.as_ref();
        let right_meta = right_entry.audio_meta.as_ref();
        text(left_meta.and_then(|meta| meta.composer.as_deref()))
            .cmp(&text(right_meta.and_then(|meta| meta.composer.as_deref())))
            .then_with(|| {
                text(left_meta.and_then(|meta| meta.work.as_deref()))
                    .cmp(&text(right_meta.and_then(|meta| meta.work.as_deref())))
            })
            .then_with(|| {
                movement_order(left_meta).cmp(&movement_order(right_meta))
            })
            .then_with(|| left_entry.rel_path.cmp(&right_entry.rel_path))
    });
    audio
}

fn text(value: Option<&str>) -> String {
    value.unwrap_or_default().trim().to_lowercase()
}

fn movement_order(metadata: Option<&AudioMeta>) -> (u32, String) {
    let value = metadata.and_then(|meta| meta.movement_number.as_deref()).unwrap_or_default();
    let digits: String = value.chars().take_while(char::is_ascii_digit).collect();
    (digits.parse().unwrap_or(u32::MAX), value.to_lowercase())
}

fn contains_query(entry: &LibraryEntry, query: &str) -> bool {
    if query.is_empty() {
        return true;
    }
    let metadata = entry.audio_meta.as_ref();
    let mut fields = metadata
        .into_iter()
        .flat_map(|meta| [
            meta.title.as_deref(),
            meta.work.as_deref(),
            meta.composer.as_deref(),
            meta.artist.as_deref(),
            meta.conductor.as_deref(),
            meta.performers.as_deref(),
            meta.ensemble.as_deref(),
            meta.movement.as_deref(),
            meta.movement_number.as_deref(),
            meta.album.as_deref(),
            meta.recording_date.as_deref(),
            meta.label.as_deref(),
        ])
        .flatten();
    entry.rel_path.to_string_lossy().to_lowercase().contains(query)
        || fields.any(|field| field.to_lowercase().contains(query))
}

fn dash(value: Option<&str>) -> &str {
    value.filter(|value| !value.trim().is_empty()).unwrap_or("—")
}

impl SepiolaApp {
    pub fn music_ui(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if pointing_hand(ui.button("← 返回总览")).clicked() {
                self.library.page = LibraryPage::Overview;
            }
            ui.label(egui::RichText::new("音频元数据").size(16.0));
        });
        let all_audio = sorted_audio(self);
        if all_audio.is_empty() {
            ui.label("库中暂无音频文件；先在总览扫描文件类型。");
            return;
        }
        let missing = all_audio
            .iter()
            .filter(|index| self.library.index.entries[**index].audio_meta.is_none())
            .count();
        let unattempted = all_audio
            .iter()
            .filter(|index| {
                let entry = &self.library.index.entries[**index];
                entry.audio_meta.is_none() && !entry.audio_metadata_attempted
            })
            .count();
        let failed = missing - unattempted;
        ui.horizontal(|ui| {
            ui.label(format!("{} 个文件", all_audio.len()));
            ui.add(egui::TextEdit::singleline(&mut self.library.music_search)
                .hint_text("搜索作品、作曲家、指挥、演奏者或路径…")
                .desired_width(300.0));
            if pointing_hand(ui.add_enabled(
                !self.library.scanning && unattempted > 0,
                egui::Button::new(format!("提取元数据（{unattempted}）")),
            )).clicked() {
                self.start_audio_metadata_scan(false);
            }
            if pointing_hand(ui.add_enabled(
                !self.library.scanning && failed > 0,
                egui::Button::new(format!("重试失败（{failed}）")),
            )).clicked() {
                self.start_audio_metadata_scan(true);
            }
        });
        if self.library.scanning {
            let (done, total) = self.library.progress;
            ui.add(egui::ProgressBar::new(if total == 0 { 0.0 } else { done as f32 / total as f32 })
                .show_percentage()
                .text(format!("{done}/{total}")));
        }
        if !self.library.message.is_empty() {
            ui.label(self.library.message.as_str());
        }
        let query = self.library.music_search.trim().to_lowercase();
        let filtered: Vec<_> = all_audio
            .into_iter()
            .filter(|index| contains_query(&self.library.index.entries[*index], &query))
            .collect();
        ui.label(format!("显示 {} 项", filtered.len()));
        show_audio(ui, &self.library.index.entries, &filtered);
    }
}

fn show_audio(ui: &mut egui::Ui, entries: &[LibraryEntry], audio: &[usize]) {
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .id_salt("music_view")
        .show_rows(ui, ui.text_style_height(&egui::TextStyle::Body) + 4.0, audio.len() + 1, |ui, rows| {
            egui::Grid::new("music_grid")
                .num_columns(12)
                .striped(true)
                .spacing([12.0, 4.0])
                .start_row(rows.start)
                .show(ui, |ui| {
                    for row in rows {
                        if row == 0 {
                            for header in ["类型", "曲目", "作品", "作曲家", "艺术家", "指挥", "演奏者", "乐团", "乐章", "日期", "专辑", "文件"] {
                                ui.add(egui::Label::new(egui::RichText::new(header).strong()).truncate());
                            }
                            ui.end_row();
                            continue;
                        }
                        let entry = &entries[audio[row - 1]];
                        let empty = AudioMeta::default();
                        let meta = entry.audio_meta.as_ref().unwrap_or(&empty);
                        let file_name = entry.rel_path.file_name()
                            .map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
                        let movement = format!(
                            "{} {}",
                            dash(meta.movement_number.as_deref()),
                            dash(meta.movement.as_deref())
                        );
                        ui.label(if entry.kind == FileKind::Recording { "录音" } else { "音频" });
                        for value in [
                            dash(meta.title.as_deref()), dash(meta.work.as_deref()),
                            dash(meta.composer.as_deref()), dash(meta.artist.as_deref()),
                            dash(meta.conductor.as_deref()), dash(meta.performers.as_deref()),
                            dash(meta.ensemble.as_deref()),
                            movement.as_str(),
                            dash(meta.recording_date.as_deref()), dash(meta.album.as_deref()),
                        ] {
                            ui.add(egui::Label::new(value).truncate());
                        }
                        ui.add(egui::Label::new(file_name).truncate())
                            .on_hover_text(entry.rel_path.display().to_string());
                        ui.end_row();
                    }
                });
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry_with_audio_meta(meta: AudioMeta) -> LibraryEntry {
        LibraryEntry {
            rel_path: "audio/track.flac".into(),
            kind: FileKind::Audio,
            album: String::new(),
            person: None,
            manual: false,
            book_meta: None,
            audio_meta: Some(meta),
            image_classification_version: 0,
            book_metadata_attempted: false,
            audio_metadata_attempted: true,
        }
    }

    #[test]
    fn movement_numbers_sort_numerically() {
        assert_eq!(movement_order(Some(&AudioMeta { movement_number: Some("2".into()), ..Default::default() })), (2, "2".into()));
        assert!(movement_order(Some(&AudioMeta { movement_number: Some("10".into()), ..Default::default() })) > movement_order(Some(&AudioMeta { movement_number: Some("2".into()), ..Default::default() })));
    }

    #[test]
    fn search_matches_normalized_composer_and_path() {
        let entry = entry_with_audio_meta(AudioMeta {
            composer: Some("Ludwig van Beethoven".into()),
            ..AudioMeta::default()
        });
        assert!(contains_query(&entry, "beethoven"));
        assert!(contains_query(&entry, "track.flac"));
        assert!(!contains_query(&entry, "mozart"));
    }
}