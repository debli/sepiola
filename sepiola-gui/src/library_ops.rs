//! 库页签文件操作：点击预览、移动相册、打开目录、复制路径、从索引移除。
//! 状态与扫描见 `library.rs`，视图见 `library_view.rs`，显影队列见 `library_develop.rs`。

use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, TryRecvError};

use eframe::egui;
use sepiola::develop::is_developable_format;
use sepiola::library::{self, FileKind, LibraryIndex};

use crate::actions::open_in_file_manager;
use crate::assets::pointing_hand;
use crate::library::{non_empty_root, PreviewPixels};
use crate::state::{Preview, SepiolaApp};

/// 行右键菜单动作：延迟到列表循环结束后执行，避免迭代中重载索引导致越界。
pub enum RowAction {
    Open(PathBuf),
    Copy(PathBuf),
    Move(PathBuf, String),
    Remove(PathBuf),
    Develop(PathBuf),
}

/// 文件行的右键菜单内容；缩略图槽与标签按钮共用同一份动作。
pub fn row_context_menu(
    ui: &mut egui::Ui,
    rel: &PathBuf,
    draft: &str,
    options: &[String],
    pending: &mut Option<RowAction>,
) {
    if pointing_hand(ui.button("打开所在目录")).clicked() {
        *pending = Some(RowAction::Open(rel.clone()));
        ui.close();
    }
    if pointing_hand(ui.button("复制绝对路径")).clicked() {
        *pending = Some(RowAction::Copy(rel.clone()));
        ui.close();
    }
    if is_developable_format(rel) && pointing_hand(ui.button("显影为 JPEG")).clicked() {
        *pending = Some(RowAction::Develop(rel.clone()));
        ui.close();
    }
    ui.separator();
    let menu = ui.menu_button("移动到相册", |ui| {
        if pointing_hand(ui.button(format!("「{draft}」（草稿）"))).clicked() {
            *pending = Some(RowAction::Move(rel.clone(), draft.to_string()));
            ui.close();
        }
        ui.separator();
        for name in options {
            if pointing_hand(ui.button(name.as_str())).clicked() {
                *pending = Some(RowAction::Move(rel.clone(), name.clone()));
                ui.close();
            }
        }
    });
    pointing_hand(menu.response);
    ui.separator();
    if pointing_hand(ui.button("从索引移除")).clicked() {
        *pending = Some(RowAction::Remove(rel.clone()));
        ui.close();
    }
}

impl SepiolaApp {
    pub fn retag_selected(&mut self) {
        let album = self.library.album_draft.trim().to_string();
        if let Some(rel) = self.library.selected_rel.clone() {
            self.retag_to(&rel, &album);
        }
    }

    /// 把 `rel` 移动到相册 `album`（置 manual），并重载索引。
    pub fn retag_to(&mut self, rel: &PathBuf, album: &str) {
        let Some(root) = non_empty_root(&self.library.root) else {
            return;
        };
        if album.trim().is_empty() {
            return;
        }
        match library::retag(&root, rel, album.trim()) {
            Ok(()) => {
                self.library.index = LibraryIndex::load(&root).unwrap_or_default();
                self.library.message = format!("已将 {} 移动到「{}」", rel.display(), album.trim());
            }
            Err(error) => self.library.message = format!("重打标签失败：{error}"),
        }
    }

    /// 点击文件行：选中并尝试解码预览纹理（同路径复用，避免重复解码）。
    pub fn select_library_file(&mut self, ctx: &egui::Context, rel: PathBuf, kind: FileKind) {
        self.library.selected_rel = Some(rel.clone());
        if kind != FileKind::Image {
            self.library.preview = None;
            self.library.preview_receiver = None;
            self.library.preview_loading = None;
            return;
        }
        let Some(root) = non_empty_root(&self.library.root) else {
            return;
        };
        let abs = root.join(&rel);
        if self
            .library
            .preview
            .as_ref()
            .is_some_and(|preview| preview.path == abs)
            || self.library.preview_loading.as_ref() == Some(&abs)
        {
            return;
        }
        let max_side = ctx
            .input(|input| input.max_texture_side)
            .clamp(1, PREVIEW_MAX_SIDE as usize) as u32;
        let developed = library::developed_path(&root, &rel);
        let request_path = abs.clone();
        let (sender, receiver) = channel();
        self.library.preview = None;
        self.library.preview_loading = Some(abs.clone());
        self.library.preview_receiver = Some(receiver);
        std::thread::spawn(move || {
            let result = decode_preview(&abs, &developed, max_side);
            let _ = sender.send(PreviewPixels {
                path: request_path,
                result,
            });
        });
    }

    /// 把后台生成的像素缓冲上传为纹理；已切换选择的结果会被丢弃。
    pub fn poll_library_preview(&mut self, ctx: &egui::Context) {
        let requested = self.library.preview_loading.clone();
        if requested.as_ref().is_some_and(|path| {
            self.library
                .selected_rel
                .as_ref()
                .and_then(|rel| non_empty_root(&self.library.root).map(|root| root.join(rel)))
                .as_ref()
                != Some(path)
        }) {
            self.library.preview_receiver = None;
            self.library.preview_loading = None;
            return;
        }
        let result = self
            .library
            .preview_receiver
            .as_ref()
            .map(|receiver| receiver.try_recv());
        match result {
            Some(Ok(result)) => {
                self.library.preview_receiver = None;
                self.library.preview_loading = None;
                match result.result {
                    Ok((width, height, pixels)) => {
                        let image = egui::ColorImage::from_rgb([width, height], &pixels);
                        let texture = ctx.load_texture(
                            result.path.to_string_lossy(),
                            image,
                            egui::TextureOptions::LINEAR,
                        );
                        self.library.preview = Some(Preview {
                            path: result.path,
                            texture,
                        });
                    }
                    Err(error) => {
                        self.library.message = format!("无法预览所选图片：{error}");
                    }
                }
            }
            Some(Err(TryRecvError::Disconnected)) => {
                self.library.preview_receiver = None;
                self.library.preview_loading = None;
            }
            Some(Err(TryRecvError::Empty)) | None => {}
        }
    }

    /// 在系统文件管理器中打开 `rel` 所在目录。
    pub fn open_file_dir(&mut self, rel: &PathBuf) {
        let Some(root) = non_empty_root(&self.library.root) else {
            return;
        };
        let abs = root.join(rel);
        let dir = abs.parent().map(|p| p.to_path_buf()).unwrap_or(abs);
        match open_in_file_manager(&dir) {
            Ok(()) => self.library.message = format!("已在文件管理器中打开 {}", dir.display()),
            Err(error) => self.library.message = format!("打开目录失败：{error}"),
        }
    }

    pub fn copy_file_path(&mut self, ctx: &egui::Context, rel: &PathBuf) {
        let Some(root) = non_empty_root(&self.library.root) else {
            return;
        };
        ctx.copy_text(root.join(rel).display().to_string());
        self.library.message = "绝对路径已复制到剪贴板".to_string();
    }

    /// 从索引移除 `rel`（不删除磁盘文件），并重载索引与清空选中。
    pub fn remove_file_from_index(&mut self, rel: &PathBuf) {
        let Some(root) = non_empty_root(&self.library.root) else {
            return;
        };
        match library::remove_from_index(&root, rel) {
            Ok(true) => {
                self.library.index = LibraryIndex::load(&root).unwrap_or_default();
                self.library.selected_rel = None;
                self.library.preview = None;
                self.library.message = format!("已从索引移除 {}", rel.display());
            }
            Ok(false) => self.library.message = "该文件已不在索引中".to_string(),
            Err(error) => self.library.message = format!("移除失败：{error}"),
        }
    }
}

const PREVIEW_MAX_SIDE: u32 = 2048;

fn decode_preview(
    path: &Path,
    developed_path: &Path,
    max_side: u32,
) -> Result<(usize, usize, Vec<u8>), String> {
    let decoded = load_preview_image(path, max_side)
        .or_else(|_| load_preview_image(developed_path, max_side))?;
    let rgb = decoded.thumbnail(max_side, max_side).to_rgb8();
    Ok((rgb.width() as usize, rgb.height() as usize, rgb.into_raw()))
}

fn load_preview_image(path: &Path, max_side: u32) -> Result<image::DynamicImage, String> {
    let is_jpeg = path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            extension.eq_ignore_ascii_case("jpg") || extension.eq_ignore_ascii_case("jpeg")
        });
    if is_jpeg {
        if let Ok(file) = File::open(path) {
            if let Ok(mut decoder) = image::codecs::jpeg::JpegDecoder::new(BufReader::new(file)) {
                let requested = max_side.min(u16::MAX as u32) as u16;
                if decoder.scale(requested, requested).is_ok() {
                    if let Ok(image) = image::DynamicImage::from_decoder(decoder) {
                        return Ok(image);
                    }
                }
            }
        }
    }
    sepiola::imageio::load_dynamic(path).map_err(|error| error.to_string())
}

impl SepiolaApp {
    /// 双击图片行时打开相册浏览窗口：收集当前视图所有图片的绝对路径，从 `pos` 位置开始。
    pub fn open_album_for_visible(&mut self, _ctx: &egui::Context, visible: &[usize], pos: usize) {
        let Some(root) = non_empty_root(&self.library.root) else {
            return;
        };
        let images: Vec<PathBuf> = visible
            .iter()
            .map(|index| root.join(&self.library.index.entries[*index].rel_path))
            .collect();
        self.album.lock().unwrap().open_album(root, images, pos);
    }
}
