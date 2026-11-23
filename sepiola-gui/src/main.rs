//! Sepiola 桌面图形界面（eframe/egui）。
//!
//! 模块划分：`state` 状态与配置、`worker` 后台批处理、`actions` 结果/人物操作、
//! `views` 面板视图、`library`/`library_ops`/`library_view` 媒体库页签、
//! `dialogs` 弹窗、`assets` 共享小工具。

mod actions;
mod album_view;
mod assets;
mod book_filters;
mod book_search;
mod book_search_view;
mod books_table;
mod books_view;
mod compare;
mod compare_engine;
mod compare_view;
mod dialogs;
mod library;
mod library_develop;
mod library_ops;
mod library_overview;
mod library_thumbs;
mod library_view;
mod music_view;
mod notebook_view;
mod reader;
mod reader_selection;
mod reader_stream;
mod reader_theme;
mod settings_view;
mod state;
mod user_settings;
mod views;
mod worker;

use assets::pointing_hand;
use eframe::egui;
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
        self.poll_library_preview(ui.ctx());
        self.poll_compare();
        self.poll_thumbs(ui.ctx());
        self.poll_develop(ui.ctx());
        self.poll_reader();
        self.poll_notebook();
        if self.running
            || self.library.scanning
            || self.compare.running
            || self.library.book_search.content.is_searching()
            || self.library.preview_loading.is_some()
            || self.thumbs_pending()
            || !self.library.dev_inflight.is_empty()
            || self.library.reader.loading
            || self.library.reader.assist.loading
            || self.library.reader.analysis_loading.is_some()
            || self.library.reader.cards_loading
            || self
                .library
                .reader
                .item_chat
                .as_ref()
                .is_some_and(|chat| chat.loading)
            || self.notebook.exercises.loading
            || self.notebook.sentence.loading
            || self.ai_probe.is_some()
        {
            let repaint_interval = if self.compare.running { 30 } else { 100 };
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(repaint_interval));
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
            } else if self.tab == library::Tab::Compare {
                if self.compare.root_a.is_empty() {
                    self.compare.root_a = dropped
                        .iter()
                        .find(|path| path.is_dir())
                        .map(|path| path.display().to_string())
                        .unwrap_or_default();
                } else if self.compare.root_b.is_empty() {
                    self.compare.root_b = dropped
                        .iter()
                        .find(|path| path.is_dir())
                        .map(|path| path.display().to_string())
                        .unwrap_or_default();
                }
            } else if self.tab == library::Tab::Process {
                self.accept_dropped_paths(&dropped);
            }
        }

        // 关闭窗口时落盘阅读进度，避免最后半屏的滚动丢失。
        if ui.ctx().input(|input| input.viewport().close_requested()) {
            self.save_reader_position();
        }
        egui::Panel::top("title").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.heading("Sepiola");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let tab = |ui: &mut egui::Ui, checked: bool, text: &str| {
                        pointing_hand(ui.add_sized(
                            [84.0, 30.0],
                            egui::Button::selectable(checked, egui::RichText::new(text).size(14.0)),
                        ))
                        .clicked()
                    };
                    if tab(ui, self.tab == library::Tab::Library, "库") {
                        self.tab = library::Tab::Library;
                    }
                    let due = self.notebook.due_count(sepiola::notebook::now_unix());
                    let notebook_label = if due > 0 {
                        format!("笔记本 {due}")
                    } else {
                        "笔记本".to_string()
                    };
                    if tab(ui, self.tab == library::Tab::Notebook, &notebook_label) {
                        self.tab = library::Tab::Notebook;
                    }
                    if tab(ui, self.tab == library::Tab::Process, "RAW调色") {
                        self.tab = library::Tab::Process;
                    }
                    if tab(ui, self.tab == library::Tab::Compare, "比较") {
                        self.tab = library::Tab::Compare;
                    }
                    if tab(ui, self.tab == library::Tab::Settings, "设置") {
                        self.tab = library::Tab::Settings;
                    }
                });
            });
        });

        if self.tab == library::Tab::Library && !self.notebook.dismissed_reminder {
            let due = self.notebook.due_count(sepiola::notebook::now_unix());
            if due > 0 {
                egui::Panel::top("notebook_reminder").show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(format!("有 {due} 张记忆卡片待复习"));
                        if pointing_hand(ui.button("去复习")).clicked() {
                            self.tab = library::Tab::Notebook;
                        }
                        ui.with_layout(
                            egui::Layout::right_to_left(egui::Align::Center),
                            |ui| {
                                if pointing_hand(ui.button("×")).clicked() {
                                    self.notebook.dismissed_reminder = true;
                                }
                            },
                        );
                    });
                });
            }
        }

        egui::Panel::bottom("status").show(ui, |ui| {
            ui.horizontal(|ui| {
                if self.running || self.library.scanning || self.compare.running {
                    ui.spinner();
                }
                let message = match self.tab {
                    library::Tab::Library if !self.library.message.is_empty() => {
                        self.library.message.as_str()
                    }
                    library::Tab::Compare if !self.compare.message.is_empty() => {
                        self.compare.message.as_str()
                    }
                    library::Tab::Notebook if !self.notebook.message.is_empty() => {
                        self.notebook.message.as_str()
                    }
                    _ => self.message.as_str(),
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
                library::LibraryPage::Music => {
                    egui::CentralPanel::default().show(ui, |ui| {
                        self.music_ui(ui);
                    });
                }
                library::LibraryPage::Overview => {
                    egui::CentralPanel::default().show(ui, |ui| {
                        self.overview_ui(ui);
                    });
                }
            },
            library::Tab::Compare => {
                egui::CentralPanel::default().show(ui, |ui| {
                    self.compare_ui(ui);
                });
            }
            library::Tab::Notebook => {
                egui::CentralPanel::default().show(ui, |ui| {
                    self.notebook_ui(ui);
                });
            }
            library::Tab::Settings => {
                egui::CentralPanel::default().show(ui, |ui| {
                    self.app_settings_ui(ui);
                });
            }
        }

        // 阅读页独立窗口：关闭窗口或点「关闭阅读」都会回到库页并保存进度。
        if self.reader_window {
            let title = format!("Sepiola — {}", self.library.reader.title);
            let builder = egui::ViewportBuilder::default()
                .with_title(title)
                .with_inner_size([900.0, 720.0])
                .with_min_inner_size([520.0, 420.0]);
            ui.ctx().show_viewport_immediate(
                egui::ViewportId::from_hash_of("reader_window"),
                builder,
                |ui, _class| {
                    egui::CentralPanel::default().show(ui, |ui| {
                        self.reader_ui(ui);
                    });
                    if ui.ctx().input(|input| input.viewport().close_requested()) {
                        self.close_reader();
                    }
                },
            );
        }

        self.zoom_window(ui.ctx());
        self.person_window(ui.ctx());
        self.album_ui(ui.ctx());
        self.persist_user_settings_if_changed();
        // 每分钟刷新到期角标与提醒横幅。
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_secs(60));
    }
}
