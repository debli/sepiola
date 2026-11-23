//! 相册浏览窗口：双击图片弹出，独立 OS 原生窗口（egui 多视口），
//! 支持上一张/下一张，底部显示前后缩略图。

use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender, TryRecvError};
use std::sync::Arc;
use std::time::Duration;

use eframe::egui;

use crate::assets::pointing_hand;

const THUMB_SIDE: u32 = 280;
const MAIN_IMAGE_SIDE: u32 = 2048;
const MAX_STRIP_THUMBS: usize = 9;

// ---------------------------------------------------------------------------
// 线程间传递的解码结果
// ---------------------------------------------------------------------------

struct LoadedImage {
    path: PathBuf,
    index: usize,
    width: usize,
    height: usize,
    pixels: Vec<u8>,
}

// ---------------------------------------------------------------------------
// 相册浏览状态（由 Arc<Mutex<>> 共享于主线程与视口线程之间）
// ---------------------------------------------------------------------------

pub struct AlbumState {
    pub open: bool,
    cache_root: PathBuf,
    images: Vec<PathBuf>,
    current_index: usize,
    current_texture: Option<egui::TextureHandle>,
    current_path: Option<PathBuf>,
    loading_main: Option<PathBuf>,
    zoom: f32,
    main_rx: Option<Receiver<LoadedImage>>,
    thumb_tx: Option<Sender<(usize, PathBuf)>>,
    thumb_rx: Option<Receiver<LoadedImage>>,
    thumb_textures: Vec<Option<egui::TextureHandle>>,
    thumb_loading: Vec<bool>,
}

impl Default for AlbumState {
    fn default() -> Self {
        Self {
            open: false,
            cache_root: PathBuf::new(),
            images: Vec::new(),
            current_index: 0,
            current_texture: None,
            current_path: None,
            loading_main: None,
            zoom: 1.0,
            main_rx: None,
            thumb_tx: None,
            thumb_rx: None,
            thumb_textures: Vec::new(),
            thumb_loading: Vec::new(),
        }
    }
}

impl AlbumState {
    pub fn open_album(&mut self, cache_root: PathBuf, images: Vec<PathBuf>, start_index: usize) {
        if images.is_empty() {
            return;
        }
        let index = start_index.min(images.len().saturating_sub(1));
        self.cache_root = cache_root;
        self.images = images;
        self.current_index = index;
        self.open = true;
        self.current_texture = None;
        self.current_path = None;
        self.loading_main = None;
        self.zoom = 1.0;
        self.main_rx = None;
        self.thumb_tx = None;
        self.thumb_rx = None;
        self.thumb_textures = vec![None; self.images.len()];
        self.thumb_loading = vec![false; self.images.len()];
    }

    fn load_main(&mut self, ctx: &egui::Context) {
        if self.images.is_empty() || self.current_index >= self.images.len() {
            return;
        }
        let path = self.images[self.current_index].clone();
        if self.loading_main.as_deref() == Some(path.as_path()) {
            return;
        }
        if self.current_path.as_deref() == Some(path.as_path()) && self.current_texture.is_some() {
            return;
        }
        self.loading_main = Some(path.clone());
        self.current_path = Some(path.clone());
        self.current_texture = None;

        let (sender, receiver) = channel();
        self.main_rx = Some(receiver);

        let path_clone = path;
        let index = self.current_index;
        let max_side = ctx
            .input(|input| input.max_texture_side)
            .clamp(1, MAIN_IMAGE_SIDE as usize) as u32;
        std::thread::spawn(move || {
            let result = decode_image(&path_clone, max_side);
            let _ = sender.send(LoadedImage {
                path: path_clone,
                index,
                width: result.width,
                height: result.height,
                pixels: result.pixels,
            });
        });
    }

    fn ensure_thumb(&mut self, idx: usize) {
        if idx >= self.images.len() || self.thumb_loading[idx] || self.thumb_textures[idx].is_some()
        {
            return;
        }
        self.thumb_loading[idx] = true;

        if self.thumb_tx.is_none() {
            let (task_tx, task_rx) = channel::<(usize, PathBuf)>();
            let (result_tx, result_rx) = channel();
            let root = self.cache_root.clone();
            std::thread::spawn(move || {
                for (index, path) in task_rx {
                    let cached = path
                        .strip_prefix(&root)
                        .ok()
                        .and_then(|relative| {
                            sepiola::thumb::generate(&root, relative, THUMB_SIDE)
                                .ok()
                                .flatten()
                        })
                        .and_then(|cached_path| image::open(cached_path).ok())
                        .map(|image| image.to_rgb8());
                    let result = cached.map_or_else(
                        || decode_image(&path, THUMB_SIDE),
                        |rgb| DecodedImage {
                            width: rgb.width() as usize,
                            height: rgb.height() as usize,
                            pixels: rgb.into_raw(),
                        },
                    );
                    if result_tx
                        .send(LoadedImage {
                            path,
                            index,
                            width: result.width,
                            height: result.height,
                            pixels: result.pixels,
                        })
                        .is_err()
                    {
                        break;
                    }
                }
            });
            self.thumb_tx = Some(task_tx);
            self.thumb_rx = Some(result_rx);
        }
        let tx = self.thumb_tx.clone().unwrap();
        let path = self.images[idx].clone();
        if tx.send((idx, path)).is_err() {
            self.thumb_tx = None;
            self.thumb_rx = None;
            self.thumb_loading[idx] = false;
        }
    }

    fn poll_main(&mut self, ctx: &egui::Context) {
        let Some(rx) = &self.main_rx else {
            return;
        };
        let mut extract = Vec::new();
        loop {
            match rx.try_recv() {
                Ok(img) => extract.push(img),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    self.main_rx = None;
                    break;
                }
            }
        }
        for img in extract {
            let index_matches = img.index == self.current_index
                && self.loading_main.as_deref() == Some(img.path.as_path());
            let color = egui::ColorImage::from_rgb([img.width, img.height], &img.pixels);
            let texture = ctx.load_texture(
                img.path.to_string_lossy(),
                color,
                egui::TextureOptions::LINEAR,
            );
            if index_matches {
                self.current_texture = Some(texture);
                self.loading_main = None;
            }
        }
    }

    fn poll_thumb(&mut self, ctx: &egui::Context) {
        let Some(rx) = &self.thumb_rx else {
            return;
        };
        loop {
            match rx.try_recv() {
                Ok(img) => {
                    let color = egui::ColorImage::from_rgb([img.width, img.height], &img.pixels);
                    let texture = ctx.load_texture(
                        img.path.to_string_lossy(),
                        color,
                        egui::TextureOptions::LINEAR,
                    );
                    if img.index < self.thumb_textures.len() {
                        self.thumb_textures[img.index] = Some(texture);
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    self.thumb_tx = None;
                    self.thumb_rx = None;
                    self.thumb_loading.fill(false);
                    break;
                }
            }
        }
    }

    fn prev(&mut self, ctx: &egui::Context) {
        if self.current_index > 0 {
            self.current_index -= 1;
            self.zoom = 1.0;
            self.load_main(ctx);
        }
    }

    fn next(&mut self, ctx: &egui::Context) {
        if self.current_index + 1 < self.images.len() {
            self.current_index += 1;
            self.zoom = 1.0;
            self.load_main(ctx);
        }
    }

    fn goto(&mut self, ctx: &egui::Context, index: usize) {
        if index < self.images.len() {
            self.current_index = index;
            self.zoom = 1.0;
            self.load_main(ctx);
        }
    }
}

// ---------------------------------------------------------------------------
// 视口渲染：在独立 OS 窗口中运行
// ---------------------------------------------------------------------------

fn render_album_viewport(state: &mut AlbumState, ui: &mut egui::Ui) {
    let ctx = ui.ctx().clone();
    // 检测 OS 关闭事件（用户点击窗口 X 按钮）
    if ctx.input(|i| i.viewport().close_requested()) {
        state.open = false;
        return;
    }

    // 后台加载
    state.load_main(&ctx);
    state.poll_main(&ctx);
    state.poll_thumb(&ctx);

    // 确保临近缩略图
    let n = state.images.len();
    if n > 0 {
        let cur = state.current_index;
        let range = MAX_STRIP_THUMBS / 2 + 1;
        for offset in 0..=range {
            if offset <= cur {
                state.ensure_thumb(cur - offset);
            }
            if cur + offset < n {
                state.ensure_thumb(cur + offset);
            }
        }
    }

    // 渲染 UI
    egui::CentralPanel::default().show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.label(format!(
                "{}/{}",
                state.current_index + 1,
                state.images.len()
            ));
            if let Some(path) = &state.current_path {
                ui.label(
                    path.file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_else(|| path.display().to_string()),
                );
            }
            ui.label(format!("缩放 {:.0}%", state.zoom * 100.0));
        });
        ui.separator();

        // 主图区域
        let thumb_strip_h = 90.0;
        let arrow_w = 48.0;
        let arrow_gap = ui.spacing().item_spacing.x;
        let avail = ui.available_size_before_wrap();
        let img_h = (avail.y - thumb_strip_h).max(80.0);

        ui.horizontal(|ui| {
            // 左箭头
            let (lr, lc) = ui.allocate_exact_size(egui::vec2(arrow_w, img_h), egui::Sense::click());
            if pointing_hand(lc).clicked() {
                state.prev(ui.ctx());
            }
            draw_arrow(ui, lr, false);

            // 主图
            let main_w = (avail.x - arrow_w * 2.0 - arrow_gap * 2.0).max(120.0);
            let (mr, main_response) =
                ui.allocate_exact_size(egui::vec2(main_w, img_h), egui::Sense::click());
            if main_response.double_clicked() {
                state.zoom = 1.0;
            }
            let wheel_delta = ui.input(|input| {
                if !input
                    .pointer
                    .hover_pos()
                    .is_some_and(|position| mr.contains(position))
                {
                    return 0.0;
                }
                input
                    .raw
                    .events
                    .iter()
                    .filter_map(|event| match event {
                        egui::Event::MouseWheel { unit, delta, .. } => {
                            let notches = match unit {
                                egui::MouseWheelUnit::Point => delta.y / 100.0,
                                egui::MouseWheelUnit::Line => delta.y * 0.2,
                                egui::MouseWheelUnit::Page => delta.y,
                            };
                            Some(-notches * 0.15)
                        }
                        _ => None,
                    })
                    .sum::<f32>()
            });
            if wheel_delta != 0.0 {
                state.zoom = (state.zoom * wheel_delta.exp()).clamp(0.1, 8.0);
            }

            if let Some(tex) = &state.current_texture {
                let ts = tex.size_vec2();
                let fit = (mr.width() / ts.x).min(mr.height() / ts.y).min(1.0);
                let size = ts * fit * state.zoom;
                let image_rect = egui::Rect::from_center_size(mr.center(), size);
                ui.painter().with_clip_rect(mr).image(
                    tex.id(),
                    image_rect,
                    egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)),
                    egui::Color32::WHITE,
                );
            } else if state.loading_main.is_some() {
                ui.put(
                    mr,
                    egui::Label::new(
                        egui::RichText::new("载入中…")
                            .size(20.0)
                            .color(egui::Color32::GRAY),
                    ),
                );
            } else if !state.images.is_empty() {
                ui.put(
                    mr,
                    egui::Label::new(
                        egui::RichText::new("无图片")
                            .size(20.0)
                            .color(egui::Color32::GRAY),
                    ),
                );
            }

            // 右箭头
            let (rr, rc) = ui.allocate_exact_size(egui::vec2(arrow_w, img_h), egui::Sense::click());
            if pointing_hand(rc).clicked() {
                state.next(ui.ctx());
            }
            draw_arrow(ui, rr, true);
        });

        ui.separator();

        // 底部缩略图条
        if n > 0 {
            let half = MAX_STRIP_THUMBS / 2;
            let start = state.current_index.saturating_sub(half);
            let end = (state.current_index + half + 1).min(n);
            let count = end - start;
            let thumb_sz = 76.0;
            let gap = 4.0;
            let total_w = count as f32 * (thumb_sz + gap) - gap;

            egui::ScrollArea::horizontal()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.horizontal_centered(|ui| {
                        ui.add_space((ui.available_width() - total_w).max(0.0) / 2.0);
                        for i in start..end {
                            let is_cur = i == state.current_index;
                            let (rect, resp) = ui.allocate_exact_size(
                                egui::vec2(thumb_sz, thumb_sz),
                                egui::Sense::click(),
                            );

                            if let Some(tex) = &state.thumb_textures[i] {
                                let ts = tex.size_vec2();
                                let s = (rect.width() / ts.x).min(rect.height() / ts.y).min(1.0);
                                ui.put(
                                    egui::Rect::from_center_size(rect.center(), ts * s),
                                    egui::Image::from_texture((tex.id(), ts * s)),
                                );
                                if is_cur {
                                    ui.painter().rect_stroke(
                                        rect,
                                        0.0,
                                        egui::Stroke::new(2.0, egui::Color32::WHITE),
                                        egui::StrokeKind::Middle,
                                    );
                                }
                            } else {
                                ui.painter()
                                    .rect_filled(rect, 0.0, egui::Color32::from_gray(48));
                                state.ensure_thumb(i);
                            }

                            if pointing_hand(resp).clicked() {
                                state.goto(ui.ctx(), i);
                            }
                        }
                    });
                });
        }
    });

    // 键盘快捷键
    if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        state.open = false;
    }
    if ctx.input(|i| i.key_pressed(egui::Key::ArrowLeft)) {
        state.prev(&ctx);
    }
    if ctx.input(|i| i.key_pressed(egui::Key::ArrowRight)) {
        state.next(&ctx);
    }

    ctx.request_repaint_after(Duration::from_millis(17));
}

// ---------------------------------------------------------------------------
// 主线程入口：创建独立视口
// ---------------------------------------------------------------------------

impl crate::state::SepiolaApp {
    pub fn album_ui(&mut self, ctx: &egui::Context) {
        let album = Arc::clone(&self.album);

        // 快速检查
        if !album.lock().unwrap().open {
            return;
        }

        let viewport_id = egui::ViewportId::from_hash_of("sepiola_album_viewer");

        ctx.show_viewport_deferred(
            viewport_id,
            egui::ViewportBuilder::default()
                .with_title("相册浏览")
                .with_inner_size([920.0, 700.0])
                .with_min_inner_size([400.0, 320.0])
                .with_resizable(true),
            move |vui, _class| {
                let mut state = album.lock().unwrap();
                if !state.open {
                    return;
                }
                render_album_viewport(&mut state, vui);
                if !state.open {
                    drop(state);
                    // 通知关闭
                    vui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                }
            },
        );
    }
}

// ---------------------------------------------------------------------------
// 辅助渲染
// ---------------------------------------------------------------------------

fn draw_arrow(ui: &mut egui::Ui, rect: egui::Rect, right: bool) {
    let c = rect.center();
    let sz = 14.0;
    let pts: [egui::Pos2; 3] = if right {
        [
            c + egui::vec2(-sz * 0.5, -sz),
            c + egui::vec2(-sz * 0.5, sz),
            c + egui::vec2(sz * 0.5, 0.0),
        ]
    } else {
        [
            c + egui::vec2(sz * 0.5, -sz),
            c + egui::vec2(sz * 0.5, sz),
            c + egui::vec2(-sz * 0.5, 0.0),
        ]
    };
    ui.painter().add(egui::Shape::convex_polygon(
        pts.to_vec(),
        egui::Color32::from_gray(180),
        egui::Stroke::NONE,
    ));
}

// ---------------------------------------------------------------------------
// 图片解码
// ---------------------------------------------------------------------------

struct DecodedImage {
    width: usize,
    height: usize,
    pixels: Vec<u8>,
}

fn decode_image(path: &Path, max_side: u32) -> DecodedImage {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");

    // JPEG 快速路径：硬件级缩放解码
    if ext.eq_ignore_ascii_case("jpg") || ext.eq_ignore_ascii_case("jpeg") {
        if let Ok(f) = std::fs::File::open(path) {
            use std::io::BufReader;
            if let Ok(mut dec) = image::codecs::jpeg::JpegDecoder::new(BufReader::new(f)) {
                let scale = max_side.min(u16::MAX as u32) as u16;
                if dec.scale(scale, scale).is_ok() {
                    if let Ok(img) = image::DynamicImage::from_decoder(dec) {
                        let rgb = img.to_rgb8();
                        return DecodedImage {
                            width: rgb.width() as usize,
                            height: rgb.height() as usize,
                            pixels: rgb.into_raw(),
                        };
                    }
                }
            }
        }
    }

    // 非 JPEG：全分辨率解码 + Triangle 快速缩放
    let decoded = match sepiola::imageio::load_dynamic(path) {
        Ok(img) => img,
        Err(_) => {
            return DecodedImage {
                width: 0,
                height: 0,
                pixels: Vec::new(),
            };
        }
    };

    let (w, h) = (decoded.width(), decoded.height());
    if w <= max_side && h <= max_side {
        let rgb = decoded.to_rgb8();
        return DecodedImage {
            width: rgb.width() as usize,
            height: rgb.height() as usize,
            pixels: rgb.into_raw(),
        };
    }

    let scale = (max_side as f64 / w as f64).min(max_side as f64 / h as f64);
    let nw = (w as f64 * scale) as u32;
    let nh = (h as f64 * scale) as u32;
    let rgb = image::imageops::resize(
        &decoded.to_rgb8(),
        nw,
        nh,
        image::imageops::FilterType::Triangle,
    );
    DecodedImage {
        width: rgb.width() as usize,
        height: rgb.height() as usize,
        pixels: rgb.into_raw(),
    }
}
