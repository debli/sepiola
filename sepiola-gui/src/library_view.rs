//! 图片浏览页：相册 chips、重打标签、虚拟化文件列表、右键菜单与预览。
//! 目录选择与扫描控制见 `library_overview.rs`（第一级总览页）。

use eframe::egui;
use sepiola::library::{self, FileKind};

use crate::assets::pointing_hand;
use crate::library::LibraryPage;
use crate::library_ops::{row_context_menu, RowAction};
use crate::state::SepiolaApp;

/// 虚拟化行高：固定高度才能让 show_rows 精确计算可见范围。
const ROW_HEIGHT: f32 = 40.0;

impl SepiolaApp {
    pub fn library_ui(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if pointing_hand(ui.button("← 返回总览")).clicked() {
                self.library.page = LibraryPage::Overview;
            }
            let pending = self
                .library
                .index
                .entries
                .iter()
                .filter(|entry| {
                    entry.kind == FileKind::Image
                        && !entry.manual
                        && entry.image_classification_version
                            < sepiola::library::IMAGE_CLASSIFICATION_VERSION
                })
                .count();
            if pointing_hand(ui.add_enabled(
                !self.library.scanning && pending > 0,
                egui::Button::new(format!("分类未处理图片（{pending}）")),
            ))
            .clicked()
            {
                self.start_image_classification();
            }
            let image_count = self
                .library
                .index
                .entries
                .iter()
                .filter(|entry| entry.kind == FileKind::Image && !entry.manual)
                .count();
            if pointing_hand(ui.add_enabled(
                !self.library.scanning && image_count > 0,
                egui::Button::new("重新分类"),
            ))
            .clicked()
            {
                self.start_image_reclassification();
            }
            self.develop_button_ui(ui);
        });
        if self.library.scanning {
            let (done, total) = self.library.progress;
            let fraction = if total == 0 {
                0.0
            } else {
                done as f32 / total as f32
            };
            ui.horizontal(|ui| {
                ui.add(
                    egui::ProgressBar::new(fraction)
                        .show_percentage()
                        .text(format!("{done}/{total}")),
                );
                if pointing_hand(ui.button("取消当前任务")).clicked() {
                    self.library.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
                }
            });
        }
        self.develop_progress_ui(ui);
        if !self.library.message.is_empty() {
            ui.label(self.library.message.as_str());
        }
        ui.separator();

        let albums = self.library.index.albums();
        let reset_counts = self.library.active_task
            == Some(crate::library::LibraryTask::ReclassifyImages);
        // 内置相册全量展示（含 0 计数），自定义相册追加在后，保证新分类始终可见。
        let mut chips: Vec<(String, usize)> = library::ALL_ALBUMS
            .iter()
            .map(|name| {
                (
                    name.to_string(),
                    if reset_counts {
                        0
                    } else {
                        albums.get(*name).copied().unwrap_or(0)
                    },
                )
            })
            .collect();
        for (name, count) in &albums {
            if !library::ALL_ALBUMS.contains(&name.as_str()) {
                chips.push((name.clone(), if reset_counts { 0 } else { *count }));
            }
        }
        ui.horizontal_wrapped(|ui| {
            let all_count = self
                .library
                .index
                .entries
                .iter()
                .filter(|entry| entry.kind == FileKind::Image)
                .count();
            if pointing_hand(ui
                .selectable_label(
                    self.library.selected_album.is_none(),
                    format!("全部（{all_count}）"),
                )
            )
            .clicked()
            {
                self.library.selected_album = None;
                self.library.selected_rel = None;
                self.library.preview = None;
            }
            for (name, count) in &chips {
                if pointing_hand(ui
                    .selectable_label(
                        self.library.selected_album.as_deref() == Some(name.as_str()),
                        format!("{name}（{count}）"),
                    )
                )
                .clicked()
                {
                    self.library.selected_album = Some(name.clone());
                    self.library.selected_rel = None;
                    self.library.preview = None;
                }
            }
        });

        let mut options: Vec<String> = albums.keys().cloned().collect();
        for builtin in library::ALL_ALBUMS {
            if !options.iter().any(|name| name == builtin) {
                options.push(builtin.to_string());
            }
        }
        if self.library.album_draft.is_empty() {
            self.library.album_draft = self
                .library
                .selected_album
                .clone()
                .unwrap_or_else(|| library::ALBUM_SCENERY.to_string());
        }
        ui.horizontal(|ui| {
            ui.label("重打标签 →");
            let response = egui::ComboBox::from_id_salt("album_draft")
                .selected_text(self.library.album_draft.as_str())
                .show_ui(ui, |ui| {
                    for option in &options {
                        pointing_hand(ui.selectable_value(
                            &mut self.library.album_draft,
                            option.clone(),
                            option,
                        ));
                    }
                });
            pointing_hand(response.response);
            ui.add(
                egui::TextEdit::singleline(&mut self.library.album_draft)
                    .hint_text("或输入新相册名")
                    .desired_width(150.0),
            );
            let enabled =
                self.library.selected_rel.is_some() && !self.library.album_draft.trim().is_empty();
            if pointing_hand(ui.add_enabled(enabled, egui::Button::new("移动/重打标签")))
                .clicked()
            {
                self.retag_selected();
            }
        });
        ui.weak("点击行选中并预览；右键行打开操作菜单。");
        ui.separator();

        // 仅保存可见行的索引，避免每帧克隆全部路径；本页只列图片。
        let visible: Vec<usize> = self
            .library
            .index
            .entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| {
                entry.kind == FileKind::Image
                    && self
                        .library
                        .selected_album
                        .as_ref()
                        .map_or(true, |album| entry.album == *album)
            })
            .map(|(i, _)| i)
            .collect();
        let draft = self.library.album_draft.clone();
        let mut pending: Option<RowAction> = None;
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .id_salt("lib_files")
            .show_rows(ui, ROW_HEIGHT, visible.len(), |ui, range| {
                ui.spacing_mut().item_spacing.y = 0.0;
                for pos in range {
                    let entry = &self.library.index.entries[visible[pos]];
                    let rel = entry.rel_path.clone();
                    let kind = entry.kind;
                    let label = format!(
                        "{:<7} {}{}{}",
                        kind.as_str(),
                        rel.display(),
                        entry
                            .person
                            .as_deref()
                            .map(|p| format!(" · {p}"))
                            .unwrap_or_default(),
                        if entry.manual { " (手动)" } else { "" }
                    );
                    let selected =
                        self.library.selected_rel.as_deref() == Some(rel.as_path());
                    ui.horizontal(|ui| {
                        // 缩略图槽：整行高，等比居中；缺失时占位并投递后台生成任务。
                        let (slot_rect, slot_resp) =
                            ui.allocate_exact_size(egui::vec2(ROW_HEIGHT, ROW_HEIGHT), egui::Sense::click());
                        let slot_resp = pointing_hand(slot_resp);
                        if kind == FileKind::Image {
                            match self.library.thumb_textures.get(&rel) {
                                Some(texture) => {
                                    let size = texture.size_vec2();
                                    let scale = (ROW_HEIGHT / size.x)
                                        .min(ROW_HEIGHT / size.y)
                                        .min(1.0);
                                    let fitted = size * scale;
                                    let rect = egui::Rect::from_center_size(slot_rect.center(), fitted);
                                    ui.put(rect, egui::Image::from_texture((texture.id(), fitted)));
                                }
                                None => {
                                    ui.painter().rect_filled(
                                        slot_rect,
                                        2.0,
                                        egui::Color32::from_gray(48),
                                    );
                                    self.request_thumb(&rel);
                                }
                            }
                        }
                        if slot_resp.clicked() {
                            self.select_library_file(ui.ctx(), rel.clone(), kind);
                        }
                        let resp = pointing_hand(ui.add_sized(
                            [ui.available_width(), ROW_HEIGHT],
                            egui::Button::selectable(selected, label),
                        ));
                        if resp.clicked() {
                            self.select_library_file(ui.ctx(), rel.clone(), kind);
                        }
                        let make_menu = |ui: &mut egui::Ui, pending: &mut Option<RowAction>| {
                            row_context_menu(ui, &rel, &draft, &options, pending)
                        };
                        slot_resp.context_menu(|ui| make_menu(ui, &mut pending));
                        resp.context_menu(|ui| make_menu(ui, &mut pending));
                    });
                }
            });

        match pending {
            Some(RowAction::Open(rel)) => self.open_file_dir(&rel),
            Some(RowAction::Copy(rel)) => self.copy_file_path(ui.ctx(), &rel),
            Some(RowAction::Move(rel, album)) => self.retag_to(&rel, &album),
            Some(RowAction::Remove(rel)) => self.remove_file_from_index(&rel),
            Some(RowAction::Develop(rel)) => self.develop_in_background(&rel),
            None => {}
        }
    }

    /// 右侧预览面板：展示选中文件的解码图。
    pub fn library_preview_ui(&mut self, ui: &mut egui::Ui) {
        ui.heading("预览");
        let Some(rel) = self.library.selected_rel.clone() else {
            ui.weak("在左侧文件列表中点击一行查看预览。");
            return;
        };
        ui.label(rel.display().to_string());
        ui.separator();
        match &self.library.preview {
            Some(preview) => {
                egui::ScrollArea::both().show(ui, |ui| {
                    let size = preview.texture.size_vec2();
                    let scale = (ui.available_width() / size.x).min(1.0);
                    ui.image((preview.texture.id(), size * scale));
                });
            }
            None => {
                ui.weak(
                    "该文件无可用预览（RAW 请右键选择「显影为 JPEG」，视频/文档不支持内嵌预览）。",
                );
            }
        }
    }
}
