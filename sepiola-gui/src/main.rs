//! Sepiola 桌面图形界面（eframe/egui）。
//!
//! 模块划分：`state` 状态与配置、`worker` 后台批处理、`actions` 结果/人物操作、
//! `views` 面板视图、`library`/`library_ops`/`library_view` 媒体库页签、
//! `dialogs` 弹窗、`assets` 共享小工具。

mod actions;
mod assets;
mod books_view;
mod dialogs;
mod library;
mod library_develop;
mod library_ops;
mod library_overview;
mod library_thumbs;
mod library_view;
mod state;
mod user_settings;
mod views;
mod worker;

use eframe::egui;
use assets::pointing_hand;
use state::SepiolaApp;

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1080.0, 720.0])
            .with_min_inner_size([860.0, 560.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Sepiola",
        options,
        Box::new(|cc| Ok(Box::new(SepiolaApp::new(cc)))),
    )
}

impl eframe::App for SepiolaApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.poll_events();
        self.poll_library();
        self.poll_thumbs(ui.ctx());
        self.poll_develop(ui.ctx());
        if self.running
            || self.library.scanning
            || self.thumbs_pending()
            || !self.library.dev_inflight.is_empty()
        {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(100));
        }
        // 支持从文件管理器拖入目录/图片：处理页设为输入目录，库页设为库目录。
        let dropped: Vec<std::path::PathBuf> = ui.ctx().input(|input| {
            input
                .raw
                .dropped_files
                .iter()
                .map(|file| file.path().to_path_buf())
                .collect()
        });
        if !dropped.is_empty() {
            if self.tab == library::Tab::Library {
                if let Some(dir) = dropped.iter().find(|path| path.is_dir()) {
                    self.select_library_root(dir.clone());
                }
            } else {
                self.accept_dropped_paths(&dropped);
            }
        }

        egui::Panel::top("title").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.heading("Sepiola");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let tab = |ui: &mut egui::Ui, checked: bool, text: &str| {
                        pointing_hand(ui.add_sized(
                            [72.0, 30.0],
                            egui::Button::selectable(
                                checked,
                                egui::RichText::new(text).size(14.0),
                            ),
                        ))
                        .clicked()
                    };
                    if tab(ui, self.tab == library::Tab::Library, "库") {
                        self.tab = library::Tab::Library;
                    }
                    if tab(ui, self.tab == library::Tab::Process, "RAW调色") {
                        self.tab = library::Tab::Process;
                    }
                });
            });
        });

        egui::Panel::bottom("status").show(ui, |ui| {
            ui.horizontal(|ui| {
                if self.running || self.library.scanning {
                    ui.spinner();
                }
                let message = if self.tab == library::Tab::Library && !self.library.message.is_empty() {
                    self.library.message.as_str()
                } else {
                    self.message.as_str()
                };
                ui.label(message);
            });
        });

        match self.tab {
            library::Tab::Process => {
                egui::Panel::left("settings")
                    .resizable(true)
                    .default_size(340.0)
                    .show(ui, |ui| {
                        egui::ScrollArea::vertical().show(ui, |ui| {
                            self.settings_ui(ui);
                        });
                    });

                egui::Panel::right("preview")
                    .resizable(true)
                    .default_size(360.0)
                    .show(ui, |ui| {
                        self.preview_ui(ui);
                    });

                egui::CentralPanel::default().show(ui, |ui| {
                    self.center_ui(ui);
                });
            }
            library::Tab::Library => match self.library.page {
                library::LibraryPage::Images => {
                    egui::Panel::right("lib_preview")
                        .resizable(true)
                        .default_size(360.0)
                        .show(ui, |ui| {
                            egui::ScrollArea::vertical().show(ui, |ui| {
                                self.library_preview_ui(ui);
                            });
                        });

                    egui::CentralPanel::default().show(ui, |ui| {
                        self.library_ui(ui);
                    });
                }
                library::LibraryPage::Books => {
                    egui::CentralPanel::default().show(ui, |ui| {
                        self.books_ui(ui);
                    });
                }
                library::LibraryPage::Overview => {
                    egui::CentralPanel::default().show(ui, |ui| {
                        self.overview_ui(ui);
                    });
                }
            }
        }

        self.zoom_window(ui.ctx());
        self.person_window(ui.ctx());
        self.persist_user_settings_if_changed();
    }
}
