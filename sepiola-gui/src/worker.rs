//! 后台批处理线程与事件通道。

use std::sync::atomic::Ordering;
use std::sync::mpsc::{channel, TryRecvError};
use std::sync::{Arc, Mutex};

use eframe::egui;
use sepiola::pipeline::{self, BatchEvent, BatchSummary};

use crate::actions::comparison_path;
use crate::state::{Preview, Row, SepiolaApp};

pub enum UiMessage {
    Batch(BatchEvent),
    Finished(Result<BatchSummary, String>),
}

impl SepiolaApp {
    pub fn start(&mut self) {
        let config = match self.build_config() {
            Ok(config) => config,
            Err(error) => {
                self.message = format!("配置有误：{error}");
                return;
            }
        };

        self.rows.clear();
        self.selected = None;
        self.preview = None;
        self.zoom_open = false;
        self.person_edit = None;
        self.progress = (0, 0);
        self.message = "处理中…".to_string();
        self.running = true;
        self.cancelling = false;

        let (sender, receiver) = channel();
        self.receiver = Some(receiver);
        let sender = Arc::new(Mutex::new(sender));
        let hook = Arc::clone(&sender);
        let cancel = Arc::clone(&self.cancel_flag);

        std::thread::spawn(move || {
            let result = pipeline::run_batch_cancellable(
                &config,
                move |event| {
                    if let Ok(sender) = hook.lock() {
                        let _ = sender.send(UiMessage::Batch(event));
                    }
                },
                &cancel,
            )
            .map_err(|error| error.to_string());
            if let Ok(sender) = sender.lock() {
                let _ = sender.send(UiMessage::Finished(result));
            }
        });
    }

    /// 请求取消：置位标志，尚未开始处理的图片会被跳过。
    pub fn request_cancel(&mut self) {
        if !self.running {
            return;
        }
        self.cancel_flag.store(true, Ordering::Relaxed);
        self.cancelling = true;
        self.message = "正在取消…（已开始的图片会跑完）".to_string();
    }

    pub fn poll_events(&mut self) {
        let mut finished = false;
        if let Some(receiver) = &self.receiver {
            loop {
                match receiver.try_recv() {
                    Ok(UiMessage::Batch(BatchEvent::Discovered { total })) => {
                        self.progress = (0, total);
                    }
                    Ok(UiMessage::Batch(BatchEvent::Processed {
                        completed,
                        total,
                        input,
                        output,
                        scene,
                        person,
                        ok,
                        error,
                    })) => {
                        self.progress = (completed, total);
                        self.rows.push(Row {
                            input,
                            output,
                            ok,
                            scene,
                            person,
                            error,
                            kept: true,
                            deleted: false,
                        });
                    }
                    Ok(UiMessage::Batch(BatchEvent::Cancelled { remaining })) => {
                        self.message = format!("已取消，跳过 {remaining} 张未处理的图片");
                    }
                    Ok(UiMessage::Finished(result)) => {
                        self.running = false;
                        self.cancelling = false;
                        self.message = match result {
                            Ok(summary) if summary.cancelled > 0 => format!(
                                "已取消：成功 {}，失败 {}，跳过 {}；日志 {}",
                                summary.succeeded,
                                summary.failed,
                                summary.cancelled,
                                summary.csv_path.display()
                            ),
                            Ok(summary) => format!(
                                "完成：共 {} 张，成功 {}，失败 {}；日志 {}",
                                summary.total,
                                summary.succeeded,
                                summary.failed,
                                summary.csv_path.display()
                            ),
                            Err(error) => format!("处理失败：{error}"),
                        };
                        finished = true;
                        break;
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        self.running = false;
                        self.cancelling = false;
                        if self.message.starts_with("处理中") {
                            self.message = "处理线程已退出".to_string();
                        }
                        finished = true;
                        break;
                    }
                }
            }
        }
        if finished {
            self.receiver = None;
        }
    }

    pub fn select(&mut self, ctx: &egui::Context, index: usize) {
        self.selected = Some(index);
        let comparison = match self.rows.get(index) {
            Some(row) => comparison_path(&row.output),
            None => return,
        };
        if self
            .preview
            .as_ref()
            .is_some_and(|preview| preview.path == comparison)
        {
            return;
        }
        match image::open(&comparison) {
            Ok(image) => {
                let rgb = image.to_rgb8();
                let size = [rgb.width() as usize, rgb.height() as usize];
                let pixels = egui::ColorImage::from_rgb(size, rgb.as_raw());
                let texture = ctx.load_texture(
                    comparison.to_string_lossy(),
                    pixels,
                    egui::TextureOptions::LINEAR,
                );
                self.preview = Some(Preview {
                    path: comparison,
                    texture,
                });
            }
            Err(error) => {
                self.preview = None;
                self.message = format!("无法打开对比图 {}: {error}", comparison.display());
            }
        }
    }
}
