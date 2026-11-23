//! 弹窗：放大查看与人物管理（重命名 / 合并到… / 删除）。

use eframe::egui;
use sepiola::identity::Gallery;

use crate::assets::pointing_hand;
use crate::state::SepiolaApp;

impl SepiolaApp {
    pub fn zoom_window(&mut self, ctx: &egui::Context) {
        let texture = self.preview.as_ref().map(|preview| preview.texture.clone());
        let Some(texture) = texture else {
            self.zoom_open = false;
            return;
        };
        let mut open = self.zoom_open;
        egui::Window::new("放大查看")
            .open(&mut open)
            .resizable(true)
            .default_size([760.0, 560.0])
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.add(
                        egui::Slider::new(&mut self.zoom_scale, 0.1..=4.0)
                            .text("缩放")
                            .show_value(true),
                    );
                    if pointing_hand(ui.button("100%")).clicked() {
                        self.zoom_scale = 1.0;
                    }
                });
                egui::ScrollArea::both().show(ui, |ui| {
                    let size = texture.size_vec2() * self.zoom_scale;
                    ui.image((texture.id(), size));
                });
            });
        self.zoom_open = open;
    }

    /// 人物管理窗口：重命名、合并到其他人物、删除（可选删输出目录）。
    pub fn person_window(&mut self, ctx: &egui::Context) {
        let Some(index) = self.person_edit else {
            return;
        };
        let Some(current) = self.rows.get(index).and_then(|row| row.person.clone()) else {
            self.person_edit = None;
            return;
        };
        let others: Vec<String> = Gallery::load(&self.gallery_path())
            .map(|gallery| {
                gallery
                    .entries
                    .iter()
                    .map(|entry| entry.label.clone())
                    .filter(|label| *label != current)
                    .collect()
            })
            .unwrap_or_default();

        let mut open = true;
        let mut save = false;
        let mut cancel = false;
        let mut do_merge = false;
        let mut do_remove = false;
        egui::Window::new("人物管理")
            .open(&mut open)
            .show(ctx, |ui| {
                ui.label(format!("第 {} 行 · 当前人物：{current}", index + 1));
                ui.horizontal(|ui| {
                    ui.label("重命名");
                    ui.text_edit_singleline(&mut self.person_edit_text);
                    if pointing_hand(ui.button("保存")).clicked() {
                        save = true;
                    }
                    if pointing_hand(ui.button("取消")).clicked() {
                        cancel = true;
                    }
                });
                ui.separator();
                ui.horizontal(|ui| {
                    ui.label("合并到");
                    if others.is_empty() {
                        ui.weak("图库中没有其他人物");
                    } else {
                        let selected = if self.merge_target.is_empty() {
                            "选择人物…"
                        } else {
                            self.merge_target.as_str()
                        };
                        let response = egui::ComboBox::from_id_salt("merge_target")
                            .selected_text(selected)
                            .show_ui(ui, |ui| {
                                for label in &others {
                                    pointing_hand(ui.selectable_value(
                                        &mut self.merge_target,
                                        label.clone(),
                                        label.as_str(),
                                    ));
                                }
                            });
                        pointing_hand(response.response);
                        if pointing_hand(
                            ui.add_enabled(
                                !self.merge_target.is_empty(),
                                egui::Button::new("合并"),
                            )
                            .on_hover_text("把当前人物的嵌入与输出文件并入所选人物"),
                        )
                        .clicked()
                        {
                            do_merge = true;
                        }
                    }
                });
                ui.separator();
                ui.horizontal(|ui| {
                    pointing_hand(ui.checkbox(&mut self.remove_delete_dir, "同时删除输出目录"));
                    if pointing_hand(
                        ui.button("删除人物")
                            .on_hover_text("从图库删除该人物；勾选后同时删除其输出子目录"),
                    )
                    .clicked()
                    {
                        do_remove = true;
                    }
                });
            });

        if save {
            let text = self.person_edit_text.clone();
            self.rename_person(index, &text);
            self.person_edit = None;
        } else if do_merge {
            let target = self.merge_target.clone();
            self.merge_person(index, &target);
            self.person_edit = None;
        } else if do_remove {
            let delete_dir = self.remove_delete_dir;
            self.remove_person(index, delete_dir);
            self.person_edit = None;
        } else if cancel || !open {
            self.person_edit = None;
        }
    }
}
