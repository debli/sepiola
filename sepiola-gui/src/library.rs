//! 库页签状态、后台扫描线程与轮询。视图见 `library_view.rs`。

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex};

use eframe::egui;
use sepiola::config::Config;
use sepiola::library::{self, LibraryEvent, LibraryIndex, LibraryMessage};

use crate::book_search::BookSearchState;
use crate::library_develop::{DevelopEvent, DevelopTask};
use crate::library_thumbs::ThumbResult;
use crate::state::{Preview, SepiolaApp};

/// 顶栏页签。
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Process,
    Library,
}

/// 库页内部路由：第一级类型总览 → 各类型浏览页（当前实现图片、书籍）。
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum LibraryPage {
    Overview,
    Images,
    Books,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum LibraryTask {
    Index,
    Images,
    ReclassifyImages,
    Books,
}

/// 库页签的状态与后台扫描线程。
pub struct LibraryState {
    pub page: LibraryPage,
    pub root: String,
    pub index: LibraryIndex,
    pub selected_album: Option<String>,
    pub selected_rel: Option<PathBuf>,
    pub album_draft: String,
    pub scanning: bool,
    pub active_task: Option<LibraryTask>,
    pub cancel: Arc<AtomicBool>,
    pub progress: (usize, usize),
    pub message: String,
    pub receiver: Option<Receiver<LibraryMessage>>,
    /// 选中文件的预览纹理（仅光栅图；RAW 解码失败时为空）。
    pub preview: Option<Preview>,
    /// 缩略图任务发送端（持有相对路径）；worker 惰性启动。
    pub thumb_tx: Option<Sender<PathBuf>>,
    /// 缩略图结果接收端。
    pub thumb_rx: Option<Receiver<ThumbResult>>,
    /// 已请求过（含失败）的相对路径，避免重复投递任务。
    pub thumb_requested: HashSet<PathBuf>,
    /// 已加载的缩略图纹理，键为相对路径。
    pub thumb_textures: HashMap<PathBuf, egui::TextureHandle>,
    /// 显影任务发送端（单个后台 worker，惰性启动）。
    pub dev_tx: Option<Sender<DevelopTask>>,
    /// 显影结果接收端。
    pub dev_rx: Option<Receiver<DevelopEvent>>,
    /// 在途显影的相对路径，防同文件重复请求。
    pub dev_inflight: HashSet<PathBuf>,
    /// 本轮显影队列统计（单文件视为总数 1）。
    pub dev_total: usize,
    pub dev_done: usize,
    pub dev_ok: usize,
    pub dev_fail: usize,
    pub book_search: BookSearchState,
}

impl Default for LibraryState {
    fn default() -> Self {
        Self {
            page: LibraryPage::Overview,
            root: String::new(),
            index: LibraryIndex::default(),
            selected_album: None,
            selected_rel: None,
            album_draft: String::new(),
            scanning: false,
            active_task: None,
            cancel: Arc::new(AtomicBool::new(false)),
            progress: (0, 0),
            message: String::new(),
            receiver: None,
            preview: None,
            thumb_tx: None,
            thumb_rx: None,
            thumb_requested: HashSet::new(),
            thumb_textures: HashMap::new(),
            dev_tx: None,
            dev_rx: None,
            dev_inflight: HashSet::new(),
            dev_total: 0,
            dev_done: 0,
            dev_ok: 0,
            dev_fail: 0,
            book_search: BookSearchState::default(),
        }
    }
}

/// 库扫描只需要模型路径与阈值，输入/输出目录无关，故从默认配置拼装。
fn scan_config(app: &SepiolaApp) -> Config {
    let mut config = Config::default();
    config.scene_settings.classifier = "face".to_string();
    if !app.model_path.trim().is_empty() {
        config.scene_settings.model = app.model_path.trim().to_string();
    }
    config.identity.model = app.identity_model.trim().to_string();
    config.identity.landmark_model = app.landmark_model.trim().to_string();
    config.identity.threshold = app.identity_threshold;
    config
}

pub(crate) fn non_empty_root(raw: &str) -> Option<PathBuf> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(PathBuf::from(trimmed))
    }
}

impl SepiolaApp {
    pub fn select_library_root(&mut self, root: PathBuf) {
        if !root.is_dir() {
            self.library.message = format!("书库目录不存在：{}", root.display());
            return;
        }
        if self.library.scanning {
            self.library.cancel.store(true, Ordering::Relaxed);
            self.library.scanning = false;
            self.library.active_task = None;
            self.library.receiver = None;
        }
        self.library.root = root.display().to_string();
        self.library.index = LibraryIndex::load(&root).unwrap_or_default();
        self.library
            .book_search
            .refresh_facets(&self.library.index.entries);
        self.library.book_search.reset();
        self.library.selected_rel = None;
        self.library.preview = None;
        self.library.selected_album = None;
        self.library.page = LibraryPage::Overview;
        self.reset_thumbs();
        self.reset_develop();
        self.library.message = format!("已载入库 {}", root.display());
    }

    pub fn start_library_scan(&mut self) {
        self.start_library_task(LibraryTask::Index, false);
    }

    pub fn start_image_classification(&mut self) {
        self.start_library_task(LibraryTask::Images, false);
    }

    pub fn start_image_reclassification(&mut self) {
        self.start_library_task(LibraryTask::ReclassifyImages, false);
    }

    pub fn start_book_metadata_scan(&mut self, retry_failed: bool) {
        self.start_library_task(LibraryTask::Books, retry_failed);
    }

    fn start_library_task(&mut self, task: LibraryTask, retry_failed: bool) {
        if self.library.scanning {
            return;
        }
        let Some(root) = non_empty_root(&self.library.root) else {
            self.library.message = "请先选择库目录".to_string();
            return;
        };
        if !root.is_dir() {
            self.library.message = format!("库目录不存在：{}", root.display());
            return;
        }
        let config = scan_config(self);
        self.library.scanning = true;
        self.library.active_task = Some(task);
        self.library.progress = (0, 0);
        self.library.cancel = Arc::new(AtomicBool::new(false));
        self.library.message = match task {
            LibraryTask::Index => "扫描文件类型…".to_string(),
            LibraryTask::Images => "分类图片…".to_string(),
            LibraryTask::ReclassifyImages => "重新分类图片…".to_string(),
            LibraryTask::Books => "提取书籍元数据…".to_string(),
        };
        let (sender, receiver) = channel();
        self.library.receiver = Some(receiver);
        let sender = Arc::new(Mutex::new(sender));
        let hook = Arc::clone(&sender);
        let cancel = Arc::clone(&self.library.cancel);
        std::thread::spawn(move || {
            let on_event = move |event| {
                    if let Ok(sender) = hook.lock() {
                        let _ = sender.send(LibraryMessage::Event(event));
                    }
                };
            let result = match task {
                LibraryTask::Index => library::scan(&root, &config, on_event, &cancel),
                LibraryTask::Images => {
                    library::classify_images(&root, &config, false, on_event, &cancel)
                }
                LibraryTask::ReclassifyImages => {
                    library::classify_images(&root, &config, true, on_event, &cancel)
                }
                LibraryTask::Books => {
                    library::extract_book_metadata(&root, retry_failed, on_event, &cancel)
                }
            }
            .map_err(|error| error.to_string());
            if let Ok(sender) = sender.lock() {
                let _ = sender.send(LibraryMessage::Done(result));
            }
        });
    }

    pub fn poll_library(&mut self) {
        self.library.book_search.content.poll();
        let mut finished = false;
        if let Some(receiver) = &self.library.receiver {
            loop {
                match receiver.try_recv() {
                    Ok(LibraryMessage::Event(LibraryEvent::Discovered { total })) => {
                        self.library.progress = (0, total);
                    }
                    Ok(LibraryMessage::Event(LibraryEvent::Processed {
                        completed,
                        total,
                        ..
                    })) => {
                        self.library.progress = (completed, total);
                    }
                    Ok(LibraryMessage::Done(result)) => {
                        self.library.scanning = false;
                        let task = self.library.active_task.take().unwrap_or(LibraryTask::Index);
                        let root = PathBuf::from(self.library.root.trim());
                        self.library.index = LibraryIndex::load(&root).unwrap_or_default();
                        self.library
                            .book_search
                            .refresh_facets(&self.library.index.entries);
                        self.library.book_search.reset();
                        self.library.message = match result {
                            Ok(report) => {
                                let fallback = if report.detector_missing {
                                    "（人脸模型缺失，未处理图片保持原标签）"
                                } else {
                                    ""
                                };
                                match task {
                                    LibraryTask::Index => format!(
                                        "类型扫描完成：新增 {}，移除 {}",
                                        report.added, report.removed
                                    ),
                                    LibraryTask::Images => format!(
                                        "图片分类完成：{} 张{}",
                                        report.classified, fallback
                                    ),
                                    LibraryTask::ReclassifyImages => format!(
                                        "重新分类完成：{} 张{}",
                                        report.classified, fallback
                                    ),
                                    LibraryTask::Books => format!(
                                        "书籍元数据处理完成：{} 本",
                                        report.metadata_attempted
                                    ),
                                }
                            }
                            Err(error) => format!("扫描失败：{error}"),
                        };
                        finished = true;
                        break;
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        self.library.scanning = false;
                        self.library.active_task = None;
                        self.library.message = "扫描线程已退出".to_string();
                        finished = true;
                        break;
                    }
                }
            }
        }
        if finished {
            self.library.receiver = None;
        }
    }

}
