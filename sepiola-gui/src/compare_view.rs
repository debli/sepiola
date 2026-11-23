use std::collections::HashSet;
use std::path::{Path, PathBuf};

use eframe::egui;

use crate::assets::pointing_hand;
use crate::compare::ConfirmAction;
use crate::compare_engine::{CompareItem, CompareMode};
use crate::state::SepiolaApp;

impl SepiolaApp {
    pub fn compare_ui(&mut self, ui: &mut egui::Ui) {
        ui.heading("比较目录");
        let inputs_enabled = !self.compare.running && self.compare.confirm.is_none();
        let previous_roots = (self.compare.root_a.clone(), self.compare.root_b.clone());
        let previous_mode = self.compare.mode;
        ui.add_enabled_ui(inputs_enabled, |ui| {
            ui.horizontal(|ui| {
                ui.label("A");
                ui.text_edit_singleline(&mut self.compare.root_a);
                if pointing_hand(ui.button("选择…")).clicked() {
                    if let Some(path) = rfd::FileDialog::new().pick_folder() {
                        self.compare.root_a = path.display().to_string();
                    }
                }
                ui.label("B");
                ui.text_edit_singleline(&mut self.compare.root_b);
                if pointing_hand(ui.button("选择…")).clicked() {
                    if let Some(path) = rfd::FileDialog::new().pick_folder() {
                        self.compare.root_b = path.display().to_string();
                    }
                }
            });
        });
        ui.horizontal(|ui| {
            ui.add_enabled_ui(inputs_enabled, |ui| {
                egui::ComboBox::from_id_salt("compare_mode")
                    .selected_text(self.compare.mode.label())
                    .show_ui(ui, |ui| {
                        for mode in [
                            CompareMode::Files,
                            CompareMode::Images,
                            CompareMode::Books,
                            CompareMode::Music,
                        ] {
                            ui.selectable_value(&mut self.compare.mode, mode, mode.label());
                        }
                    });
            });
            if previous_roots != (self.compare.root_a.clone(), self.compare.root_b.clone())
                || previous_mode != self.compare.mode
            {
                self.compare.report = None;
                self.compare.selected_items.clear();
                self.compare.message.clear();
            }
            let complete_report = self
                .compare
                .report
                .as_ref()
                .is_some_and(|report| !report.cancelled);
            let compare = ui.add_enabled(!self.compare.running, egui::Button::new("开始比较"));
            if pointing_hand(compare).clicked() {
                self.start_compare();
            }
            if self.compare.running && pointing_hand(ui.button("取消")).clicked() {
                self.compare
                    .cancel
                    .store(true, std::sync::atomic::Ordering::Relaxed);
            }
            if ui
                .add_enabled(
                    !self.compare.running && complete_report,
                    egui::Button::new("A → B"),
                )
                .on_hover_text("复制当前分类文件；同路径文件将被覆盖")
                .clicked()
            {
                self.compare.confirm = Some(ConfirmAction::Sync { a_to_b: true });
            }
            if ui
                .add_enabled(
                    !self.compare.running && complete_report,
                    egui::Button::new("B → A"),
                )
                .on_hover_text("复制当前分类文件；同路径文件将被覆盖")
                .clicked()
            {
                self.compare.confirm = Some(ConfirmAction::Sync { a_to_b: false });
            }
            if ui
                .add_enabled(
                    !self.compare.running && complete_report,
                    egui::Button::new("保留 A，清理 B 重复项"),
                )
                .clicked()
            {
                self.start_delete_preview(true);
            }
            if ui
                .add_enabled(
                    !self.compare.running && complete_report,
                    egui::Button::new("保留 B，清理 A 重复项"),
                )
                .clicked()
            {
                self.start_delete_preview(false);
            }
        });
        if self.compare.running && self.compare.progress.1 > 0 {
            ui.add(
                egui::ProgressBar::new(
                    self.compare.progress.0 as f32 / self.compare.progress.1 as f32,
                )
                .text(format!(
                    "{} / {}",
                    self.compare.progress.0, self.compare.progress.1
                )),
            );
        }
        if !self.compare.message.is_empty() {
            ui.label(&self.compare.message);
        }
        ui.separator();
        self.compare_results(ui);
        self.compare_confirmation(ui.ctx());
    }

    fn compare_results(&mut self, ui: &mut egui::Ui) {
        let Some(report) = &self.compare.report else {
            if self.compare.running && !self.compare.scanned_files.is_empty() {
                ui.weak("正在读取文件，稍后开始配对比较：");
                let available_width = ui.available_width();
                let spacing = ui.spacing().item_spacing.x;
                let path_width = ((available_width - spacing * 2.0) * 0.4).max(80.0);
                let status_width = (available_width - path_width * 2.0 - spacing * 2.0).max(80.0);
                let row_height = ui.spacing().interact_size.y;
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
                    .show(ui, |ui| {
                        egui::Grid::new("compare_scan_preview")
                            .striped(true)
                            .num_columns(3)
                            .show(ui, |ui| {
                                ui.add_sized(
                                    [path_width, row_height],
                                    egui::Label::new(egui::RichText::new("A 已读取").strong()),
                                );
                                ui.add_sized(
                                    [path_width, row_height],
                                    egui::Label::new(egui::RichText::new("B 已读取").strong()),
                                );
                                ui.add_sized(
                                    [status_width, row_height],
                                    egui::Label::new(egui::RichText::new("状态").strong()),
                                );
                                ui.end_row();
                                for (relative, (seen_a, seen_b)) in &self.compare.scanned_files {
                                    let path = path_text(relative);
                                    let a_path = if *seen_a {
                                        path.clone()
                                    } else {
                                        "—".to_string()
                                    };
                                    let b_path = if *seen_b { path } else { "—".to_string() };
                                    ui.add_sized(
                                        [path_width, row_height],
                                        egui::Label::new(&a_path).truncate(),
                                    )
                                    .on_hover_text(a_path);
                                    ui.add_sized(
                                        [path_width, row_height],
                                        egui::Label::new(&b_path).truncate(),
                                    )
                                    .on_hover_text(b_path);
                                    let status = if *seen_a && *seen_b {
                                        "等待比较结果"
                                    } else if *seen_a {
                                        "已读取 A"
                                    } else {
                                        "已读取 B"
                                    };
                                    ui.add_sized(
                                        [status_width, row_height],
                                        egui::Label::new(status),
                                    );
                                    ui.end_row();
                                }
                            });
                    });
                return;
            }
            ui.weak(if self.compare.running {
                "比较中，匹配项会逐步显示。"
            } else {
                "选择 A、B 目录和比较类型后开始比较。"
            });
            return;
        };
        let mut select_all = false;
        let mut clear_selection = false;
        let mut copy_selected_a_to_b = false;
        let mut copy_selected_b_to_a = false;
        ui.horizontal_wrapped(|ui| {
            ui.label(format!(
                "{} 项，已选 {} 项",
                report.items.len(),
                self.compare.selected_items.len()
            ));
            if ui.button("全选").clicked() {
                select_all = true;
            }
            if ui.button("清除选择").clicked() {
                clear_selection = true;
            }
            ui.separator();
            ui.add_enabled_ui(
                !self.compare.running && !self.compare.selected_items.is_empty(),
                |ui| {
                    copy_selected_a_to_b = ui.button("复制所选 A → B").clicked();
                    copy_selected_b_to_a = ui.button("复制所选 B → A").clicked();
                },
            );
        });
        if select_all {
            self.compare.selected_items = report.items.iter().map(item_key).collect();
        } else if clear_selection {
            self.compare.selected_items.clear();
        }
        if copy_selected_a_to_b || copy_selected_b_to_a {
            let a_to_b = copy_selected_a_to_b;
            let files = selected_copy_plans(&report.items, &self.compare.selected_items, a_to_b);
            if !files.is_empty() {
                self.compare.confirm = Some(ConfirmAction::Copy { a_to_b, files });
            }
        }

        let report = self.compare.report.as_ref().unwrap();
        let selected_items = &mut self.compare.selected_items;
        let can_copy = !self.compare.running;
        let mut row_action = None;
        let available_width = ui.available_width();
        let spacing = ui.spacing().item_spacing.x;
        let check_width = 24.0;
        let similarity_width = 70.0;
        let actions_width = 150.0;
        let flexible_width =
            (available_width - check_width - similarity_width - actions_width - spacing * 5.0)
                .max(150.0);
        let path_width = flexible_width * 0.29;
        let details_width = flexible_width * 0.42;
        let row_height = ui.spacing().interact_size.y;
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
            .show(ui, |ui| {
                egui::Grid::new("compare_results")
                    .striped(true)
                    .num_columns(6)
                    .show(ui, |ui| {
                        ui.add_sized([check_width, row_height], egui::Label::new(""));
                        ui.add_sized(
                            [path_width, row_height],
                            egui::Label::new(egui::RichText::new("A").strong()),
                        );
                        ui.add_sized(
                            [path_width, row_height],
                            egui::Label::new(egui::RichText::new("B").strong()),
                        );
                        ui.add_sized(
                            [details_width, row_height],
                            egui::Label::new(egui::RichText::new("差异").strong()),
                        );
                        ui.add_sized(
                            [similarity_width, row_height],
                            egui::Label::new(egui::RichText::new("相似度").strong()),
                        );
                        ui.add_sized(
                            [actions_width, row_height],
                            egui::Label::new(egui::RichText::new("操作").strong()),
                        );
                        ui.end_row();
                        for item in &report.items {
                            let key = item_key(item);
                            let mut selected = selected_items.contains(&key);
                            if ui
                                .add_sized(
                                    [check_width, row_height],
                                    egui::Checkbox::new(&mut selected, ""),
                                )
                                .changed()
                            {
                                if selected {
                                    selected_items.insert(key);
                                } else {
                                    selected_items.remove(&key);
                                }
                            }
                            let (a_color, b_color) = item_colors(ui, item);
                            let a_path = item
                                .a
                                .as_deref()
                                .map(path_text)
                                .unwrap_or_else(|| "—".to_string());
                            ui.add_sized(
                                [path_width, row_height],
                                egui::Label::new(egui::RichText::new(&a_path).color(a_color))
                                    .truncate(),
                            )
                            .on_hover_text(a_path);
                            let b_path = item
                                .b
                                .as_deref()
                                .map(path_text)
                                .unwrap_or_else(|| "—".to_string());
                            ui.add_sized(
                                [path_width, row_height],
                                egui::Label::new(egui::RichText::new(&b_path).color(b_color))
                                    .truncate(),
                            )
                            .on_hover_text(b_path);
                            let mut details = item.details.join("；");
                            if item.exact_duplicate {
                                if !details.is_empty() {
                                    details.push_str("；");
                                }
                                details.push_str("内容完全相同");
                            }
                            let similarity = item
                                .similarity
                                .map(|value| format!("{:.1}%", value * 100.0))
                                .unwrap_or_default();
                            let mut tooltip = details.clone();
                            if !similarity.is_empty() {
                                if !tooltip.is_empty() {
                                    tooltip.push('\n');
                                }
                                tooltip.push_str("相似度：");
                                tooltip.push_str(&similarity);
                            }
                            ui.add_sized(
                                [details_width, row_height],
                                egui::Label::new(details).truncate(),
                            )
                            .on_hover_text(if tooltip.is_empty() {
                                "无差异详情"
                            } else {
                                &tooltip
                            });
                            ui.add_sized(
                                [similarity_width, row_height],
                                egui::Label::new(&similarity).truncate(),
                            )
                            .on_hover_text(&similarity);
                            ui.add_enabled_ui(can_copy, |ui| {
                                ui.allocate_ui_with_layout(
                                    egui::vec2(actions_width, row_height),
                                    egui::Layout::left_to_right(egui::Align::Min),
                                    |ui| {
                                        ui.spacing_mut().item_spacing.x = 2.0;
                                        if item.a.is_some()
                                            && ui
                                                .button("A → B")
                                                .on_hover_text("复制 A 文件到 B")
                                                .clicked()
                                        {
                                            row_action = copy_plan(item, true).map(|file| {
                                                ConfirmAction::Copy {
                                                    a_to_b: true,
                                                    files: vec![file],
                                                }
                                            });
                                        }
                                        if item.b.is_some()
                                            && ui
                                                .button("B → A")
                                                .on_hover_text("复制 B 文件到 A")
                                                .clicked()
                                        {
                                            row_action = copy_plan(item, false).map(|file| {
                                                ConfirmAction::Copy {
                                                    a_to_b: false,
                                                    files: vec![file],
                                                }
                                            });
                                        }
                                    },
                                );
                            });
                            ui.end_row();
                        }
                    });
            });
        if let Some(action) = row_action {
            self.compare.confirm = Some(action);
        }
    }

    fn compare_confirmation(&mut self, ctx: &egui::Context) {
        let Some(action) = self.compare.confirm.clone() else {
            return;
        };
        let (title, text, can_confirm) = match &action {
            ConfirmAction::Sync { a_to_b } => (
                "确认同步",
                if *a_to_b {
                    format!(
                        "将 {} 类文件从 A 复制到 B，同相对路径文件会被覆盖。",
                        self.compare.mode.label()
                    )
                } else {
                    format!(
                        "将 {} 类文件从 B 复制到 A，同相对路径文件会被覆盖。",
                        self.compare.mode.label()
                    )
                },
                true,
            ),
            ConfirmAction::Delete { keep_a, paths } => (
                "确认清理重复文件",
                format!(
                    "保留 {}，将从另一侧删除 {} 个 SHA-256 完全相同的文件。感知相似项不会删除。",
                    if *keep_a { "A" } else { "B" },
                    paths.len()
                ),
                !paths.is_empty(),
            ),
            ConfirmAction::Copy { a_to_b, files } => (
                "确认复制比较项",
                format!(
                    "将 {} 个文件从 {} 复制到 {}；目标文件存在时会覆盖。",
                    files.len(),
                    if *a_to_b { "A" } else { "B" },
                    if *a_to_b { "B" } else { "A" }
                ),
                !files.is_empty(),
            ),
        };
        let mut confirm = false;
        let mut cancel = false;
        egui::Window::new(title)
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.label(text);
                ui.horizontal(|ui| {
                    if pointing_hand(ui.add_enabled(can_confirm, egui::Button::new("执行")))
                        .clicked()
                    {
                        confirm = true;
                    }
                    if pointing_hand(ui.button("取消")).clicked() {
                        cancel = true;
                    }
                });
            });
        if confirm {
            self.execute_confirmed(action);
        } else if cancel {
            self.compare.confirm = None;
        }
    }
}

fn path_text(path: &Path) -> String {
    path.to_string_lossy().to_string()
}

fn item_key(item: &CompareItem) -> (Option<PathBuf>, Option<PathBuf>) {
    (item.a.clone(), item.b.clone())
}

fn item_colors(ui: &egui::Ui, item: &CompareItem) -> (egui::Color32, egui::Color32) {
    let normal = ui.visuals().text_color();
    let muted = ui.visuals().weak_text_color();
    let removed = egui::Color32::from_rgb(220, 95, 95);
    let added = egui::Color32::from_rgb(90, 185, 120);
    match (item.a.is_some(), item.b.is_some()) {
        (true, false) => (removed, muted),
        (false, true) => (muted, added),
        (true, true) if item.exact_duplicate && item.details.is_empty() => (normal, normal),
        (true, true) => (removed, added),
        (false, false) => (muted, muted),
    }
}

fn copy_plan(item: &CompareItem, a_to_b: bool) -> Option<(PathBuf, PathBuf)> {
    let (source, target) = if a_to_b {
        (item.a.as_ref()?, item.b.as_ref().or(item.a.as_ref())?)
    } else {
        (item.b.as_ref()?, item.a.as_ref().or(item.b.as_ref())?)
    };
    Some((source.clone(), target.clone()))
}

fn selected_copy_plans(
    items: &[CompareItem],
    selected: &HashSet<(Option<PathBuf>, Option<PathBuf>)>,
    a_to_b: bool,
) -> Vec<(PathBuf, PathBuf)> {
    let mut targets = HashSet::new();
    items
        .iter()
        .filter(|item| selected.contains(&item_key(item)))
        .filter_map(|item| copy_plan(item, a_to_b))
        .filter(|(_, target)| targets.insert(target.clone()))
        .collect()
}
