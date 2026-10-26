//! 控制面板、结果表格与预览视图。

use eframe::egui;

use crate::assets::{base_name, pick_dir, pick_row, pointing_hand};
use crate::state::{BackendChoice, ClassifierChoice, PixelFormat, SepiolaApp};

impl SepiolaApp {
    pub fn settings_ui(&mut self, ui: &mut egui::Ui) {
        ui.heading("目录");
        if let Some(dir) = pick_dir(ui, "输入", &mut self.input_dir) {
            if self.output_dir.trim().is_empty() {
                self.output_dir = dir.join("out").display().to_string();
            }
        }
        pick_dir(ui, "输出", &mut self.output_dir);

        ui.separator();
        ui.heading("处理参数");
        let response = egui::ComboBox::from_label("显影后端")
            .selected_text(self.backend.as_str())
            .show_ui(ui, |ui| {
                for (choice, text) in [
                    (BackendChoice::Auto, "auto（自动）"),
                    (BackendChoice::Darktable, "darktable（高质量）"),
                    (BackendChoice::Builtin, "builtin（内置兜底）"),
                ] {
                    pointing_hand(ui.selectable_value(&mut self.backend, choice, text));
                }
            });
        pointing_hand(response.response);
        let response = egui::ComboBox::from_label("场景分类")
            .selected_text(self.classifier.as_str())
            .show_ui(ui, |ui| {
                pointing_hand(ui.selectable_value(
                    &mut self.classifier,
                    ClassifierChoice::Face,
                    "face（人脸）",
                ));
                pointing_hand(ui.selectable_value(
                    &mut self.classifier,
                    ClassifierChoice::Stub,
                    "stub（固定）",
                ));
            });
        pointing_hand(response.response);
        if self.classifier == ClassifierChoice::Face {
            pick_row(ui, "模型", &mut self.model_path, &["onnx"]);
        }
        ui.horizontal(|ui| {
            ui.label("并行线程");
            ui.add(egui::DragValue::new(&mut self.workers).range(1..=64));
            ui.label("JPEG 质量");
            ui.add(egui::DragValue::new(&mut self.jpeg_quality).range(1..=100));
        });
        ui.horizontal(|ui| {
            ui.label("高光裁切阈值");
            ui.add(
                egui::DragValue::new(&mut self.max_clipped_high)
                    .range(0.0..=1.0)
                    .speed(0.005),
            );
        });

        ui.separator();
        ui.heading("导出选项");
        let response = egui::ComboBox::from_label("像素格式")
            .selected_text(self.pixel_format.as_str())
            .show_ui(ui, |ui| {
                for format in [PixelFormat::Bit8, PixelFormat::Bit16] {
                    pointing_hand(ui.selectable_value(
                        &mut self.pixel_format,
                        format,
                        format.label(),
                    ));
                }
            });
        pointing_hand(response.response);
        if self.pixel_format == PixelFormat::Bit8 {
            pointing_hand(ui.checkbox(&mut self.keep_metadata, "保留元数据（EXIF/ICC）"));
        } else {
            ui.weak("16bit TIFF 导出不携带元数据");
        }

        ui.separator();
        ui.heading("人物识别");
        pointing_hand(ui.checkbox(&mut self.identity_enabled, "启用人物识别与按人归档"));
        if self.identity_enabled {
            pick_row(ui, "识别模型", &mut self.identity_model, &["onnx"]);
            pick_row(ui, "关键点模型", &mut self.landmark_model, &["onnx"]);
            ui.horizontal(|ui| {
                ui.label("相似度阈值");
                ui.add(
                    egui::DragValue::new(&mut self.identity_threshold)
                        .range(0.0..=1.0)
                        .speed(0.01),
                );
            });
            ui.weak("模型缺失时自动回退；点击人物列标签可重命名/合并/删除。");
        }

        ui.separator();
        ui.heading("LUT（可选）");
        ui.label("留空则不调色，仅做 RAW 显影。");
        pick_row(ui, "人像", &mut self.portrait_lut, &["cube"]);
        ui.add(egui::Slider::new(&mut self.portrait_strength, 0.0..=1.0).text("人像强度"));
        pick_row(ui, "其他", &mut self.other_lut, &["cube"]);
        ui.add(egui::Slider::new(&mut self.other_strength, 0.0..=1.0).text("其他强度"));

        ui.separator();
        ui.add_enabled_ui(!self.running, |ui| {
            if pointing_hand(ui
                .add_sized([ui.available_width(), 32.0], egui::Button::new("开始处理"))
            )
            .clicked()
            {
                self.start();
            }
        });
        if self.running {
            let cancel = egui::Button::new(if self.cancelling {
                "正在取消…"
            } else {
                "取消处理"
            })
            .min_size(egui::vec2(ui.available_width(), 28.0));
            if pointing_hand(ui
                .add_enabled(!self.cancelling, cancel)
                .on_hover_text("已开始的图片会跑完，未开始的跳过")
            )
            .clicked()
            {
                self.request_cancel();
            }
        }
        let open_button =
            egui::Button::new("打开输出位置").min_size(egui::vec2(ui.available_width(), 28.0));
        if pointing_hand(ui.add(open_button)).clicked() {
            self.open_output_location();
        }
        ui.horizontal(|ui| {
            if pointing_hand(ui.button("保存参数…")).clicked() {
                if let Some(path) = rfd::FileDialog::new()
                    .add_filter("TOML", &["toml"])
                    .set_file_name("config.toml")
                    .save_file()
                {
                    self.save_config_to(&path);
                }
            }
            if pointing_hand(ui.button("载入参数…")).clicked() {
                if let Some(path) = rfd::FileDialog::new()
                    .add_filter("TOML", &["toml"])
                    .pick_file()
                {
                    self.load_config_from(&path);
                }
            }
        });
    }

    pub fn results_ui(&mut self, ui: &mut egui::Ui) {
        if self.rows.is_empty() {
            ui.weak("还没有处理结果。");
            return;
        }
        ui.horizontal(|ui| {
            if pointing_hand(ui.button("全选")).clicked() {
                self.set_all_kept(true);
            }
            if pointing_hand(ui.button("全不选")).clicked() {
                self.set_all_kept(false);
            }
            if pointing_hand(ui
                .add_enabled(!self.running, egui::Button::new("删除未保留"))
            )
            .clicked()
            {
                self.delete_unkept();
            }
        });
        ui.separator();

        let mut clicked = None;
        let mut edit_person = None;
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                egui::Grid::new("results")
                    .striped(true)
                    .num_columns(5)
                    .show(ui, |ui| {
                        ui.strong("文件");
                        ui.strong("保留");
                        ui.strong("结果");
                        ui.strong("人物");
                        ui.strong("输出");
                        ui.end_row();

                        for index in 0..self.rows.len() {
                            let row = &mut self.rows[index];
                            if pointing_hand(ui
                                .selectable_label(self.selected == Some(index), base_name(&row.input))
                            )
                            .clicked()
                            {
                                clicked = Some(index);
                            }

                            let mut kept = row.kept;
                            let changed = ui
                                .add_enabled_ui(row.ok && !row.deleted, |ui| {
                                    pointing_hand(ui.checkbox(&mut kept, "")).changed()
                                })
                                .inner;
                            if changed {
                                row.kept = kept;
                            }

                            if row.deleted {
                                ui.colored_label(egui::Color32::GRAY, "已删除");
                            } else if row.ok {
                                let scene = row.scene.map(|kind| kind.as_str()).unwrap_or("-");
                                ui.colored_label(egui::Color32::from_rgb(60, 160, 90), scene);
                            } else {
                                ui.colored_label(egui::Color32::from_rgb(200, 70, 70), "失败");
                            }

                            match &row.person {
                                Some(label) => {
                                    let button = pointing_hand(ui
                                        .selectable_label(
                                            self.person_edit == Some(index),
                                            label.as_str(),
                                        )
                                        .on_hover_text("点击可重命名/合并/删除人物"));
                                    if button.clicked() {
                                        edit_person = Some(index);
                                    }
                                }
                                None => {
                                    ui.label("-");
                                }
                            }

                            let label = ui.label(base_name(&row.output));
                            if let Some(error) = &row.error {
                                label.on_hover_text(error.as_str());
                            }
                            ui.end_row();
                        }
                    });
            });
        if let Some(index) = clicked {
            self.select(ui.ctx(), index);
        }
        if let Some(index) = edit_person {
            let label = self.rows[index].person.clone().unwrap_or_default();
            self.person_edit = Some(index);
            self.person_edit_text = label;
            self.merge_target = String::new();
        }
    }

    pub fn preview_ui(&mut self, ui: &mut egui::Ui) {
        ui.heading("前后对比");
        match &self.preview {
            Some(preview) => {
                egui::ScrollArea::both().show(ui, |ui| {
                    let size = preview.texture.size_vec2();
                    let scale = (ui.available_width() / size.x).min(1.0);
                    ui.image((preview.texture.id(), size * scale));
                });
                if pointing_hand(ui.button("放大查看")).clicked() {
                    let width = self.preview.as_ref().expect("preview").texture.size_vec2().x;
                    self.zoom_scale = (760.0 / width).clamp(0.1, 1.0);
                    self.zoom_open = true;
                }
            }
            None => {
                ui.weak("在左侧列表中点击一行查看对比图");
            }
        }
    }

    /// 中央区：进度条 + 结果表格。
    pub fn center_ui(&mut self, ui: &mut egui::Ui) {
        let (completed, total) = self.progress;
        let fraction = if total == 0 {
            0.0
        } else {
            completed as f32 / total as f32
        };
        ui.add(
            egui::ProgressBar::new(fraction)
                .show_percentage()
                .text(format!("{completed}/{total}")),
        );
        ui.separator();
        self.results_ui(ui);
    }
}
