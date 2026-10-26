//! 库页签 RAW 显影：单个/批量一键显影的后台队列与轮询。
//! 状态字段见 `library.rs`，右键入口见 `library_ops.rs`，按钮见 `library_view.rs`。

use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};

use eframe::egui;
use sepiola::config::Config;
use sepiola::develop::is_raw_format;
use sepiola::library::{self, FileKind};

use crate::assets::pointing_hand;
use crate::library::non_empty_root;
use crate::state::SepiolaApp;

/// 显影任务：（相对路径，显影后端与 JPEG 质量配置）。
pub type DevelopTask = (PathBuf, Config);

/// 后台 worker 发回的显影结果：（相对路径，产物绝对路径或错误信息）。
pub type DevelopEvent = (PathBuf, Result<PathBuf, String>);

impl SepiolaApp {
    /// 切换库目录时终止显影 worker 并清空在途状态（drop sender 使旧线程退出）。
    pub fn reset_develop(&mut self) {
        self.library.dev_tx = None;
        self.library.dev_rx = None;
        self.library.dev_inflight.clear();
        self.library.dev_total = 0;
        self.library.dev_done = 0;
        self.library.dev_ok = 0;
        self.library.dev_fail = 0;
    }

    /// 惰性启动显影 worker：单线程逐条消费任务，避免大批量时线程爆炸与磁盘争用。
    fn ensure_dev_worker(&mut self) {
        if self.library.dev_tx.is_some() {
            return;
        }
        let Some(root) = non_empty_root(&self.library.root) else {
            return;
        };
        let (task_tx, task_rx): (Sender<DevelopTask>, Receiver<DevelopTask>) = channel();
        let (event_tx, event_rx) = channel::<DevelopEvent>();
        std::thread::spawn(move || {
            for (rel, config) in task_rx.iter() {
                let result = library::develop_file(&root, &rel, &config)
                    .map_err(|error| error.to_string());
                if event_tx.send((rel, result)).is_err() {
                    break;
                }
            }
        });
        self.library.dev_tx = Some(task_tx);
        self.library.dev_rx = Some(event_rx);
    }

    /// 把 `rel` 排入显影队列；同文件在途或 worker 不可用时返回 false。
    fn enqueue_develop(&mut self, rel: &PathBuf, config: &Config) -> bool {
        if !self.library.dev_inflight.insert(rel.clone()) {
            return false;
        }
        self.ensure_dev_worker();
        let Some(tx) = self.library.dev_tx.clone() else {
            self.library.dev_inflight.remove(rel);
            return false;
        };
        if tx.send((rel.clone(), config.clone())).is_err() {
            self.library.dev_inflight.remove(rel);
            return false;
        }
        self.library.dev_total += 1;
        true
    }

    /// 当前视图（选中相册或全部）内的 RAW 相对路径。
    fn raw_targets(&self) -> Vec<PathBuf> {
        self.library
            .index
            .entries
            .iter()
            .filter(|entry| {
                entry.kind == FileKind::Image
                    && is_raw_format(&entry.rel_path)
                    && match &self.library.selected_album {
                        Some(album) => &entry.album == album,
                        None => true,
                    }
            })
            .map(|entry| entry.rel_path.clone())
            .collect()
    }

    /// 当前视图内尚未在途的 RAW 数（供"全部显影"按钮启用与计数）。
    pub fn pending_raw_count(&self) -> usize {
        self.raw_targets()
            .iter()
            .filter(|rel| !self.library.dev_inflight.contains(rel.as_path()))
            .count()
    }

    /// 右键单文件显影（后台执行，幂等复用产物）。
    pub fn develop_in_background(&mut self, rel: &PathBuf) {
        if non_empty_root(&self.library.root).is_none() {
            return;
        }
        let config = self.develop_config();
        if !self.enqueue_develop(rel, &config) {
            self.library.message = format!("{} 正在显影中…", rel.display());
            return;
        }
        self.library.message =
            format!("显影中：{}（后端 {}）", rel.display(), self.backend.as_str());
    }

    /// 批量一键显影：当前视图内所有未在途的 RAW 一次性入队。
    pub fn develop_all(&mut self) {
        let targets = self.raw_targets();
        let config = self.develop_config();
        let was_running = self.library.dev_total > 0;
        let mut queued = 0usize;
        for rel in &targets {
            if self.enqueue_develop(rel, &config) {
                queued += 1;
            }
        }
        if queued == 0 {
            self.library.message = "当前视图没有待显影的 RAW".to_string();
        } else if was_running {
            self.library.message = format!("已追加 {queued} 个显影任务…");
        } else {
            self.library.message = format!(
                "批量显影中 0/{queued}…（后端 {}）",
                self.backend.as_str()
            );
        }
    }

    /// 轮询显影结果：更新进度计数与状态栏；成功时刷新选中项预览并允许缩略图重新请求。
    pub fn poll_develop(&mut self, ctx: &egui::Context) {
        let mut events = Vec::new();
        if let Some(rx) = &self.library.dev_rx {
            while let Ok(event) = rx.try_recv() {
                events.push(event);
            }
        }
        for (rel, result) in events {
            self.library.dev_inflight.remove(&rel);
            let ok = result.is_ok();
            let single = self.library.dev_total <= 1;
            match result {
                Ok(out) => {
                    self.library.dev_ok += 1;
                    self.library.thumb_requested.remove(&rel);
                    if self.library.selected_rel.as_deref() == Some(rel.as_path()) {
                        self.library.preview = None;
                        self.select_library_file(ctx, rel.clone(), FileKind::Image);
                    }
                    if single {
                        self.library.message =
                            format!("已显影 {} → {}", rel.display(), out.display());
                    }
                }
                Err(error) => {
                    self.library.dev_fail += 1;
                    self.library.message = format!("显影失败 {}：{error}", rel.display());
                }
            }
            self.library.dev_done += 1;
            if self.library.dev_done >= self.library.dev_total {
                if !single {
                    self.library.message = if self.library.dev_fail > 0 {
                        format!(
                            "批量显影完成：成功 {}，失败 {}（产物在 .sepiola/developed/）",
                            self.library.dev_ok, self.library.dev_fail
                        )
                    } else {
                        format!(
                            "批量显影完成：成功 {}（产物在 .sepiola/developed/）",
                            self.library.dev_ok
                        )
                    };
                }
                self.library.dev_total = 0;
                self.library.dev_done = 0;
                self.library.dev_ok = 0;
                self.library.dev_fail = 0;
            } else if ok {
                self.library.message = format!(
                    "批量显影中 {}/{}…",
                    self.library.dev_done, self.library.dev_total
                );
            }
        }
    }

    /// 显影配置：后端与 JPEG 质量沿用处理页当前选择。
    fn develop_config(&self) -> Config {
        let mut config = Config::default();
        config.develop.backend = self.backend.as_str().to_string();
        config.jpeg_quality = self.jpeg_quality;
        config
    }

    /// 顶栏"全部显影"按钮：当前视图有未在途 RAW 且未扫描时可用。
    pub fn develop_button_ui(&mut self, ui: &mut egui::Ui) {
        let pending_raws = self.pending_raw_count();
        let developing = self.library.dev_total > 0;
        let label = if developing { "显影中…" } else { "全部显影" };
        let tip = if pending_raws == 0 {
            "当前视图没有待显影的 RAW"
        } else {
            "把当前视图内所有 RAW 一键显影为 JPEG（产物存于 .sepiola/developed/，已显影的自动复用）"
        };
        let button = egui::Button::new(label).min_size(egui::vec2(110.0, 26.0));
        if pointing_hand(ui
            .add_enabled(pending_raws > 0 && !self.library.scanning, button)
            .on_hover_text(tip)
        )
        .clicked()
        {
            self.develop_all();
        }
    }

    /// 顶栏下方显影进度条：仅队列在途时显示。
    pub fn develop_progress_ui(&mut self, ui: &mut egui::Ui) {
        if self.library.dev_total == 0 {
            return;
        }
        let (done, total) = (self.library.dev_done, self.library.dev_total);
        ui.add(
            egui::ProgressBar::new(done as f32 / total as f32)
                .show_percentage()
                .text(format!("显影 {done}/{total}")),
        );
    }
}
