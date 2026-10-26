//! 库页签缩略图：后台单线程生成 + 磁盘缓存 + 纹理轮询。
//! 状态字段见 `library.rs`，视图接入见 `library_view.rs`。

use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};

use eframe::egui;
use sepiola::thumb;

use crate::library::non_empty_root;
use crate::state::SepiolaApp;

/// 缩略图长边像素（列表槽位 36px，高 DPI 下留余量）。
pub const THUMB_MAX_SIDE: u32 = 128;

/// 后台 worker 发回的缩略图结果；`pixels` 为 None 表示不可解码。
pub struct ThumbResult {
    pub rel: PathBuf,
    pub pixels: Option<(usize, usize, Vec<u8>)>,
}

impl SepiolaApp {
    /// 切换库目录时清空缩略图状态（drop sender 使旧 worker 退出）。
    pub fn reset_thumbs(&mut self) {
        self.library.thumb_tx = None;
        self.library.thumb_rx = None;
        self.library.thumb_requested.clear();
        self.library.thumb_textures.clear();
    }

    /// 惰性启动缩略图 worker：逐条取任务→生成→读回像素发回；sender dropped 时退出。
    fn ensure_thumb_worker(&mut self) {
        if self.library.thumb_tx.is_some() {
            return;
        }
        let Some(root) = non_empty_root(&self.library.root) else {
            return;
        };
        let (task_tx, task_rx): (Sender<PathBuf>, Receiver<PathBuf>) = channel();
        let (result_tx, result_rx) = channel::<ThumbResult>();
        std::thread::spawn(move || {
            for rel in task_rx.iter() {
                let pixels = match thumb::generate(&root, &rel, THUMB_MAX_SIDE) {
                    Ok(Some(path)) => image::open(&path).ok().map(|decoded| {
                        let rgb = decoded.to_rgb8();
                        (rgb.width() as usize, rgb.height() as usize, rgb.into_raw())
                    }),
                    _ => None,
                };
                if result_tx.send(ThumbResult { rel, pixels }).is_err() {
                    break;
                }
            }
        });
        self.library.thumb_tx = Some(task_tx);
        self.library.thumb_rx = Some(result_rx);
    }

    /// 为可见行投递缩略图任务（每个 rel 只请求一次，含失败占位）。
    pub fn request_thumb(&mut self, rel: &PathBuf) {
        if self.library.thumb_requested.contains(rel) {
            return;
        }
        self.ensure_thumb_worker();
        let Some(tx) = &self.library.thumb_tx else {
            return;
        };
        if tx.send(rel.clone()).is_ok() {
            self.library.thumb_requested.insert(rel.clone());
        }
    }

    /// 每帧轮询缩略图结果：解码像素加载纹理，有更新则请求重绘。
    pub fn poll_thumbs(&mut self, ctx: &egui::Context) {
        let mut updated = false;
        if let Some(rx) = &self.library.thumb_rx {
            while let Ok(result) = rx.try_recv() {
                if let Some((width, height, pixels)) = result.pixels {
                    let image = egui::ColorImage::from_rgb([width, height], &pixels);
                    let texture = ctx.load_texture(
                        result.rel.to_string_lossy(),
                        image,
                        egui::TextureOptions::LINEAR,
                    );
                    self.library.thumb_textures.insert(result.rel, texture);
                    updated = true;
                }
            }
        }
        if updated {
            ctx.request_repaint();
        }
    }

    /// 是否仍有在途缩略图任务（用于安排低频重绘）。
    pub fn thumbs_pending(&self) -> bool {
        self.library.thumb_requested.len() > self.library.thumb_textures.len()
    }
}
