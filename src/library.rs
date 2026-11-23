//! 媒体库：把指定目录作为库来源，按文件类型归类（v1 图片深度处理），
//! 图片在逻辑上拆分到相册（人像/风景/建筑/交通/美食/运动/冒险/探险/植物动物/
//! 生日/聚会/截图·文档/未分类，人物作为 person 子标签），
//! 支持浏览、重新打标签；相册是索引中的逻辑分组，不移动磁盘文件。
//!
//! 索引持久化在 `<库根>/.sepiola/library.json`，人物图库在
//! `<库根>/.sepiola/identities.json`，与批处理输出目录的 gallery 互不干扰。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex;

use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::book::{self, BookMeta};
use crate::config::Config;
use crate::develop;
use crate::error::{Error, Result};
use crate::identity::FaceIdentifier;
use crate::scene::face::FaceClassifier;
use crate::scene::mobileclip::MobileClipTagger;
use crate::scene::places::{album_for_places_class, PlacesTagger};
use crate::scene::tags::{album_for_class, SceneTagger};
use crate::scene::{SceneClassifier, SceneKind};

/// v1 自动相册。后续分类器（日出/天文…）以新名称接入。
pub const ALBUM_PORTRAIT: &str = "人像";
pub const ALBUM_SCENERY: &str = "风景";
pub const ALBUM_UNCLASSIFIED: &str = "未分类";
/// 场景标签模型（M29）细分出的相册。
pub const ALBUM_PLANTS_ANIMALS: &str = "植物动物";
pub const ALBUM_BIRTHDAY: &str = "生日";
pub const ALBUM_PARTY: &str = "聚会";
/// Places365 场景细分（M34）新增相册；日出/天象无对应类，留待专项分类器。
pub const ALBUM_ARCHITECTURE: &str = "建筑";
pub const ALBUM_VEHICLE: &str = "交通工具";
pub const ALBUM_FOOD: &str = "美食";
pub const ALBUM_SPORTS: &str = "运动";
pub const ALBUM_ADVENTURE: &str = "冒险";
pub const ALBUM_EXPLORE: &str = "探险";
pub const ALBUM_SCREEN_DOC: &str = "截图/文档";

/// 全部内置相册（GUI 下拉与右键菜单展示用；新增分类需同步维护）。
pub const ALL_ALBUMS: &[&str] = &[
    ALBUM_PORTRAIT,
    ALBUM_SCENERY,
    ALBUM_PLANTS_ANIMALS,
    ALBUM_BIRTHDAY,
    ALBUM_PARTY,
    ALBUM_FOOD,
    ALBUM_SCREEN_DOC,
    ALBUM_ARCHITECTURE,
    ALBUM_VEHICLE,
    ALBUM_SPORTS,
    ALBUM_ADVENTURE,
    ALBUM_EXPLORE,
    ALBUM_UNCLASSIFIED,
];

/// 库元数据目录（扫描时跳过）。
pub const LIBRARY_DIR: &str = ".sepiola";

/// 显影工作目录序号，保证同进程内并行显影互不干扰。
static DEVELOP_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

const VIDEO_EXTENSIONS: &[&str] = &["mp4", "mov", "mkv", "avi", "wmv", "flv", "webm", "m4v"];
const AUDIO_EXTENSIONS: &[&str] = &["mp3", "wav", "flac", "aac", "ogg", "m4a", "wma", "opus", "amr"];
const DOCUMENT_EXTENSIONS: &[&str] = &[
    "doc", "docx", "xls", "xlsx", "ppt", "pptx", "wps", "md", "markdown", "rtf", "odt",
];
const BOOK_EXTENSIONS: &[&str] = &[
    "epub", "pdf", "mobi", "azw3", "asw3", "azw", "fb2", "djvu", "txt",
];
const DRIVER_EXTENSIONS: &[&str] = &["inf", "sys", "cat"];
const INSTALLER_EXTENSIONS: &[&str] = &[
    "exe", "msi", "deb", "rpm", "apk", "dmg", "pkg", "appx", "snap", "flatpak",
];
const ARCHIVE_EXTENSIONS: &[&str] = &["zip", "rar", "7z", "tar", "gz", "bz2", "xz"];
/// 音频扩展名 + 文件名含以下关键词时判为"录音"（语音备忘、通话记录等）。
const RECORDING_KEYWORDS: &[&str] = &["录音", "record", "rec_", "voice", "speech", "通话", "voicemail"];
/// 图片相册分类规则版本；规则变化时递增以触发旧库重归类。
pub const IMAGE_CLASSIFICATION_VERSION: u32 = 4;

/// 项目目录标记（全小写，与目录项小写名比较）：命中其一即把整个目录折叠为一个
/// "项目"实体，内部文件不再单独入库。
const PROJECT_MARKERS: &[&str] = &[
    ".git", ".hg", ".svn", "cargo.toml", "package.json", "go.mod", "pom.xml", "build.gradle",
    "cmakelists.txt", "pyproject.toml",
];
const PROJECT_SUFFIXES: &[&str] = &[".sln", ".csproj"];
/// 游戏目录标记：Steam 库/manifest、Godot 导出包；优先于项目标记判定。
const GAME_DIR_NAMES: &[&str] = &["steamapps"];
const GAME_FILE_PREFIXES: &[&str] = &["appmanifest_"];
const GAME_FILE_SUFFIXES: &[&str] = &[".acf", ".pck"];

/// 文件大类。仅 Image 做深度相册归类，其余类型入库分桶；Project/Game 为目录实体。
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FileKind {
    Image,
    Video,
    Audio,
    Recording,
    Document,
    Book,
    Driver,
    Installer,
    Archive,
    Project,
    Game,
    Other,
}

/// 总览页展示顺序。
pub const ALL_KINDS: &[FileKind] = &[
    FileKind::Image,
    FileKind::Video,
    FileKind::Audio,
    FileKind::Recording,
    FileKind::Document,
    FileKind::Book,
    FileKind::Driver,
    FileKind::Installer,
    FileKind::Archive,
    FileKind::Project,
    FileKind::Game,
    FileKind::Other,
];

impl FileKind {
    pub fn as_str(self) -> &'static str {
        match self {
            FileKind::Image => "image",
            FileKind::Video => "video",
            FileKind::Audio => "audio",
            FileKind::Recording => "recording",
            FileKind::Document => "document",
            FileKind::Book => "book",
            FileKind::Driver => "driver",
            FileKind::Installer => "installer",
            FileKind::Archive => "archive",
            FileKind::Project => "project",
            FileKind::Game => "game",
            FileKind::Other => "other",
        }
    }

    /// 中文显示名（库总览页卡片标题）。
    pub fn label(self) -> &'static str {
        match self {
            FileKind::Image => "图片",
            FileKind::Video => "视频",
            FileKind::Audio => "音频",
            FileKind::Recording => "录音",
            FileKind::Document => "文档",
            FileKind::Book => "书籍",
            FileKind::Driver => "驱动",
            FileKind::Installer => "安装包",
            FileKind::Archive => "压缩包",
            FileKind::Project => "项目",
            FileKind::Game => "游戏",
            FileKind::Other => "其他",
        }
    }

    /// 通用图片扩展名（RAW 之外）。
    fn is_raster_image(ext: &str) -> bool {
        matches!(
            ext,
            "jpg" | "jpeg" | "png" | "tif" | "tiff" | "webp" | "gif" | "bmp" | "heic" | "heif"
        )
    }
}

/// 按扩展名归类（大小写不敏感）；音频扩展名 + 文件名关键词判为"录音"。
pub fn classify_kind(path: &Path) -> FileKind {
    let Some(ext) = path.extension().and_then(|ext| ext.to_str()) else {
        return FileKind::Other;
    };
    let ext = ext.to_ascii_lowercase();
    if develop::is_raw_format(path) || FileKind::is_raster_image(&ext) {
        FileKind::Image
    } else if VIDEO_EXTENSIONS.contains(&ext.as_str()) {
        FileKind::Video
    } else if AUDIO_EXTENSIONS.contains(&ext.as_str()) {
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .to_lowercase();
        if RECORDING_KEYWORDS.iter().any(|kw| name.contains(kw)) {
            FileKind::Recording
        } else {
            FileKind::Audio
        }
    } else if BOOK_EXTENSIONS.contains(&ext.as_str()) {
        FileKind::Book
    } else if DOCUMENT_EXTENSIONS.contains(&ext.as_str()) {
        FileKind::Document
    } else if DRIVER_EXTENSIONS.contains(&ext.as_str()) {
        FileKind::Driver
    } else if INSTALLER_EXTENSIONS.contains(&ext.as_str()) {
        FileKind::Installer
    } else if ARCHIVE_EXTENSIONS.contains(&ext.as_str()) {
        FileKind::Archive
    } else {
        FileKind::Other
    }
}

/// 目录是否为实体（项目/游戏）：命中游戏标记优先判游戏，否则项目标记判项目。
fn entity_kind_of_dir(dir: &Path) -> Option<FileKind> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return None;
    };
    let mut project = false;
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_lowercase();
        let is_dir = entry.path().is_dir();
        if (is_dir && GAME_DIR_NAMES.contains(&name.as_str()))
            || (GAME_FILE_PREFIXES.iter().any(|p| name.starts_with(p))
                && GAME_FILE_SUFFIXES.iter().any(|s| name.ends_with(s)))
        {
            return Some(FileKind::Game);
        }
        if (is_dir && PROJECT_MARKERS.contains(&name.as_str()))
            || (!is_dir
                && (PROJECT_MARKERS.contains(&name.as_str())
                    || PROJECT_SUFFIXES.iter().any(|s| name.ends_with(s))))
        {
            project = true;
        }
    }
    project.then_some(FileKind::Project)
}

/// 库中的一个文件条目。`album`/`person` 仅对 Image 有意义。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LibraryEntry {
    /// 相对库根的路径。
    pub rel_path: PathBuf,
    pub kind: FileKind,
    /// 所属相册；非图片为空串。
    pub album: String,
    /// 人物子标签（"某人"），仅人像相册有值。
    #[serde(default)]
    pub person: Option<String>,
    /// 用户手动打标签后为 true，自动归类不再覆盖。
    #[serde(default)]
    pub manual: bool,
    /// 书籍内容级元数据（M36），仅 Book 类型扫描时抽取。
    #[serde(default)]
    pub book_meta: Option<BookMeta>,
    /// 音频标签元数据（M41），仅 Audio/Recording 类型扫描时抽取。
    #[serde(default)]
    pub audio_meta: Option<crate::audio::AudioMeta>,
    /// 图片相册分类器版本；旧索引默认 0，进入图片库时按需补分类。
    #[serde(default)]
    pub image_classification_version: u32,
    /// 书籍元数据是否已经尝试提取（失败也记录，避免反复解析/告警）。
    #[serde(default)]
    pub book_metadata_attempted: bool,
    /// 音频元数据是否已经尝试提取。
    #[serde(default)]
    pub audio_metadata_attempted: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct LibraryIndex {
    /// 文件类型索引格式版本；图片与书籍处理状态记录在各自条目上。
    /// v3：v2 可能由 Places365/ImageNet 模型缺失的构建写出，标签仍偏旧，强制重归类一次。
    #[serde(default)]
    pub version: u32,
    pub entries: Vec<LibraryEntry>,
}

impl LibraryIndex {
    pub fn index_path(root: &Path) -> PathBuf {
        root.join(LIBRARY_DIR).join("library.json")
    }

    pub fn load(root: &Path) -> Result<Self> {
        let path = Self::index_path(root);
        if !path.is_file() {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(&path)
            .map_err(|e| Error::Config(format!("failed to read {}: {e}", path.display())))?;
        serde_json::from_str(&text).map_err(|e| Error::Config(format!("invalid library.json: {e}")))
    }

    pub fn save(&self, root: &Path) -> Result<()> {
        let path = Self::index_path(root);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| Error::Config(format!("failed to create {}: {e}", parent.display())))?;
        }
        let text = serde_json::to_string_pretty(self)
            .map_err(|e| Error::Config(format!("failed to serialize library: {e}")))?;
        std::fs::write(&path, text)
            .map_err(|e| Error::Config(format!("failed to write {}: {e}", path.display())))
    }

    pub fn find(&self, rel: &Path) -> Option<&LibraryEntry> {
        self.entries.iter().find(|entry| entry.rel_path == rel)
    }

    pub fn find_mut(&mut self, rel: &Path) -> Option<&mut LibraryEntry> {
        self.entries.iter_mut().find(|entry| entry.rel_path == rel)
    }

    /// 第一级文件类型 → 条目数（总览页卡片计数，含实体）。
    pub fn kind_counts(&self) -> BTreeMap<FileKind, usize> {
        let mut counts: BTreeMap<FileKind, usize> = BTreeMap::new();
        for entry in &self.entries {
            *counts.entry(entry.kind).or_default() += 1;
        }
        counts
    }

    /// 图片相册 → 条目数（按相册名排序）。
    pub fn albums(&self) -> BTreeMap<String, usize> {
        let mut counts: BTreeMap<String, usize> = BTreeMap::new();
        for entry in &self.entries {
            if entry.kind == FileKind::Image && !entry.album.is_empty() {
                *counts.entry(entry.album.clone()).or_default() += 1;
            }
        }
        counts
    }

    pub fn images_in(&self, album: &str) -> Vec<&LibraryEntry> {
        self.entries
            .iter()
            .filter(|entry| entry.kind == FileKind::Image && entry.album == album)
            .collect()
    }
}

/// 扫描进度事件（供 GUI/CLI 展示）。
#[derive(Clone, Debug)]
pub enum LibraryEvent {
    Discovered { total: usize },
    Processed { completed: usize, total: usize, rel_path: PathBuf },
}

/// GUI 后台线程 → 界面的消息。
pub enum LibraryMessage {
    Event(LibraryEvent),
    Done(std::result::Result<ScanReport, String>),
}

/// 一次扫描的结果统计。
#[derive(Debug, Default)]
pub struct ScanReport {
    pub added: usize,
    pub removed: usize,
    pub classified: usize,
    pub metadata_attempted: usize,
    pub cancelled: bool,
    /// 人脸模型缺失、图片落入"未分类"时为 true。
    pub detector_missing: bool,
}

/// 增量扫描库目录并识别文件类型；不执行图片推理或书籍内容解析。
pub fn scan<F>(
    root: &Path,
    _config: &Config,
    on_event: F,
    cancel: &AtomicBool,
) -> Result<ScanReport>
where
    F: Fn(LibraryEvent),
{
    if !root.is_dir() {
        return Err(Error::Config(format!("library root is not a directory: {}", root.display())));
    }
    let mut files: Vec<PathBuf> = Vec::new();
    let mut entities: Vec<(PathBuf, FileKind)> = Vec::new();
    if walk(root, root, &mut files, &mut entities, cancel)? {
        on_event(LibraryEvent::Discovered { total: 0 });
        return Ok(ScanReport {
            cancelled: true,
            ..ScanReport::default()
        });
    }
    files.sort();
    entities.sort_by(|a, b| a.0.cmp(&b.0));

    let mut index = LibraryIndex::load(root)?;
    // v9 迁移文件类型（TXT 归入书籍）；重型任务由类型页按条目状态触发。
    let reclass_file_kinds = index.version < 9;
    let known: std::collections::HashSet<PathBuf> =
        index.entries.iter().map(|entry| entry.rel_path.clone()).collect();
    let still_present: std::collections::HashSet<PathBuf> =
        files.iter().cloned().chain(entities.iter().map(|(rel, _)| rel.clone())).collect();
    let removed_before = index.entries.len();
    index.entries.retain(|entry| still_present.contains(&entry.rel_path));
    let removed = removed_before - index.entries.len();

    // 待处理 = 新文件/实体 + 需重归类的已有条目（非 manual）。
    let mut pending: Vec<(PathBuf, Option<FileKind>, bool)> = Vec::new();
    for path in &files {
        let rel = path.as_path();
        if known.contains(rel) {
            let needs_reclass = index
                .find(rel)
                .map(|entry| {
                    !entry.manual && reclass_file_kinds && entry.kind != classify_kind(rel)
                })
                .unwrap_or(false);
            if needs_reclass {
                pending.push((rel.to_path_buf(), None, false));
            }
        } else {
            pending.push((rel.to_path_buf(), None, true));
        }
    }
    for (rel, kind) in &entities {
        if known.contains(rel.as_path()) {
            let needs_reclass = index
                .find(rel)
                .map(|entry| !entry.manual && entry.kind != *kind)
                .unwrap_or(false);
            if needs_reclass {
                pending.push((rel.clone(), Some(*kind), false));
            }
        } else {
            pending.push((rel.clone(), Some(*kind), true));
        }
    }
    let total = pending.len();
    on_event(LibraryEvent::Discovered { total });

    let mut report = ScanReport {
        added: 0,
        removed,
        classified: 0,
        metadata_attempted: 0,
        detector_missing: false,
        cancelled: false,
    };
    let mut interrupted = false;
    for (completed, (rel, entity, is_new)) in pending.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            interrupted = true;
            break;
        }
        let (kind, album, person, book_meta) = if let Some(entity_kind) = entity {
            // 项目/游戏实体：相册名即目录名，内部文件不入库。
            let dir_name = rel
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or_default()
                .to_string();
            (*entity_kind, dir_name, None, None)
        } else {
            let kind = classify_kind(rel);
            let album = if kind == FileKind::Image {
                ALBUM_UNCLASSIFIED.to_string()
            } else {
                String::new()
            };
            let person = None;
            let book_meta = None;
            (kind, album, person, book_meta)
        };
        if *is_new {
            index.entries.push(LibraryEntry {
                rel_path: rel.clone(),
                kind,
                album,
                person,
                manual: false,
                book_meta,
                audio_meta: None,
                image_classification_version: 0,
                book_metadata_attempted: false,
                audio_metadata_attempted: false,
            });
            report.added += 1;
        } else if let Some(entry) = index.find_mut(rel) {
            entry.kind = kind;
            entry.album = album;
            entry.person = person;
            entry.book_meta = book_meta;
            entry.audio_meta = None;
            entry.image_classification_version = 0;
            entry.book_metadata_attempted = false;
            entry.audio_metadata_attempted = false;
        }
        on_event(LibraryEvent::Processed {
            completed: completed + 1,
            total,
            rel_path: rel.clone(),
        });
    }
    report.cancelled = interrupted;

    index.entries.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));
    if !interrupted {
        index.version = if reclass_file_kinds {
            9
        } else {
            index.version.max(9)
        };
    }
    index.save(root)?;
    Ok(report)
}

/// 对待处理图片执行人像检测与相册分类；已完成和手动标记的条目保持不动。
pub fn classify_images<F>(
    root: &Path,
    config: &Config,
    force: bool,
    on_event: F,
    cancel: &AtomicBool,
) -> Result<ScanReport>
where
    F: Fn(LibraryEvent),
{
    let mut index = LibraryIndex::load(root)?;
    let pending: Vec<PathBuf> = index
        .entries
        .iter()
        .filter(|entry| {
            entry.kind == FileKind::Image
                && !entry.manual
                && (force
                    || entry.image_classification_version < IMAGE_CLASSIFICATION_VERSION)
        })
        .map(|entry| entry.rel_path.clone())
        .collect();
    let total = pending.len();
    on_event(LibraryEvent::Discovered { total });

    let detector = if total > 0 && !cancel.load(Ordering::Relaxed) {
        FaceClassifier::new(&config.scene_settings).ok()
    } else {
        None
    };
    let detector_missing = total > 0 && !cancel.load(Ordering::Relaxed) && detector.is_none();
    let mut report = ScanReport {
        detector_missing,
        cancelled: cancel.load(Ordering::Relaxed),
        ..ScanReport::default()
    };
    if detector.is_none() {
        return Ok(report);
    }

    let mut identity_settings = config.identity.clone();
    identity_settings.enabled = true;
    identity_settings.gallery = LibraryIndex::index_path(root)
        .parent()
        .map(|dir| dir.join("identities.json").display().to_string())
        .unwrap_or_default();
    let identifier = FaceIdentifier::new(&identity_settings).ok();
    let mobileclip = if config.scene_tags.enabled && config.scene_tags.mobileclip_enabled {
        match MobileClipTagger::new(&config.scene_tags) {
            Ok(tagger) => Some(tagger),
            Err(error) => {
                tracing::warn!(%error, "MobileCLIP unavailable; falling back to ImageNet and Places365");
                None
            }
        }
    } else {
        None
    };
    let tagger = if config.scene_tags.enabled {
        SceneTagger::new(&config.scene_tags).ok()
    } else {
        None
    };
    let places = if config.scene_tags.enabled {
        PlacesTagger::new(&config.scene_tags).ok()
    } else {
        None
    };

    for (completed, rel) in pending.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        let (album, person) = auto_album(
            &root.join(rel),
            detector.as_ref(),
            identifier.as_ref(),
            mobileclip.as_ref(),
            tagger.as_ref(),
            places.as_ref(),
            config.scene_tags.min_confidence,
            config.scene_tags.places_min_confidence,
        );
        if let Some(entry) = index.find_mut(rel) {
            entry.album = album;
            entry.person = person;
            entry.image_classification_version = IMAGE_CLASSIFICATION_VERSION;
            report.classified += 1;
        }
        on_event(LibraryEvent::Processed {
            completed: completed + 1,
            total,
            rel_path: rel.clone(),
        });
    }
    report.cancelled = cancel.load(Ordering::Relaxed);
    index.save(root)?;
    Ok(report)
}

/// 对待处理书籍抽取内容级元数据；失败也记录为已尝试，避免每次进入页面重复解析。
pub fn extract_book_metadata<F>(
    root: &Path,
    retry_failed: bool,
    on_event: F,
    cancel: &AtomicBool,
) -> Result<ScanReport>
where
    F: Fn(LibraryEvent) + Sync,
{
    let mut index = LibraryIndex::load(root)?;
    let pending: Vec<PathBuf> = index
        .entries
        .iter()
        .filter(|entry| {
            entry.kind == FileKind::Book
                && !entry.manual
                && entry.book_meta.is_none()
                && (!entry.book_metadata_attempted || retry_failed)
        })
        .map(|entry| entry.rel_path.clone())
        .collect();
    let total = pending.len();
    on_event(LibraryEvent::Discovered { total });
    let mut report = ScanReport::default();

    let available_threads = std::thread::available_parallelism()
        .map(usize::from)
        .unwrap_or(1);
    // 书籍解析（尤其大 PDF）内存与耗时都很高，限制并发以缩短取消等待并控制峰值内存。
    let worker_count = (available_threads / 2).max(1).min(2);
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(worker_count)
        .build()
        .map_err(|error| Error::ThreadPool(error.to_string()))?;
    let completed = AtomicUsize::new(0);
    let progress_lock = Mutex::new(());
    let extracted: Vec<_> = pool.install(|| {
        pending
            .par_iter()
            .filter_map(|rel| {
                if cancel.load(Ordering::Relaxed) {
                    return None;
                }
                let metadata = book::extract_cancellable(&root.join(rel), cancel);
                // 取消后丢弃收尾中的结果，不再推进进度。
                if cancel.load(Ordering::Relaxed) {
                    return None;
                }
                let _progress_guard = progress_lock.lock().expect("progress lock poisoned");
                let done = completed.fetch_add(1, Ordering::Relaxed) + 1;
                on_event(LibraryEvent::Processed {
                    completed: done,
                    total,
                    rel_path: rel.clone(),
                });
                Some((rel, metadata))
            })
            .collect()
    });
    for (rel, metadata) in extracted {
        if let Some(entry) = index.find_mut(rel) {
            entry.book_meta = metadata;
            entry.book_metadata_attempted = true;
            report.metadata_attempted += 1;
        }
    }
    report.cancelled = cancel.load(Ordering::Relaxed);
    index.save(root)?;
    Ok(report)
}

/// Extract metadata for audio and recording entries; parsing is deferred until requested.
pub fn extract_audio_metadata<F>(
    root: &Path,
    retry_failed: bool,
    on_event: F,
    cancel: &AtomicBool,
) -> Result<ScanReport>
where
    F: Fn(LibraryEvent) + Sync,
{
    let mut index = LibraryIndex::load(root)?;
    let pending: Vec<PathBuf> = index
        .entries
        .iter()
        .filter(|entry| {
            matches!(entry.kind, FileKind::Audio | FileKind::Recording)
                && entry.audio_meta.is_none()
                && (!entry.audio_metadata_attempted || retry_failed)
        })
        .map(|entry| entry.rel_path.clone())
        .collect();
    let total = pending.len();
    on_event(LibraryEvent::Discovered { total });
    let mut report = ScanReport::default();

    let worker_count = std::thread::available_parallelism()
        .map(usize::from)
        .unwrap_or(1)
        .div_ceil(2)
        .max(1);
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(worker_count)
        .build()
        .map_err(|error| Error::ThreadPool(error.to_string()))?;
    let completed = AtomicUsize::new(0);
    let progress_lock = Mutex::new(());
    let extracted: Vec<_> = pool.install(|| {
        pending
            .par_iter()
            .filter_map(|rel| {
                if cancel.load(Ordering::Relaxed) {
                    return None;
                }
                let metadata = crate::audio::AudioMeta::extract(&root.join(rel));
                let _progress_guard = progress_lock.lock().expect("progress lock poisoned");
                let done = completed.fetch_add(1, Ordering::Relaxed) + 1;
                on_event(LibraryEvent::Processed {
                    completed: done,
                    total,
                    rel_path: rel.clone(),
                });
                Some((rel, metadata))
            })
            .collect()
    });
    for (rel, metadata) in extracted {
        if let Some(entry) = index.find_mut(rel) {
            entry.audio_meta = metadata;
            entry.audio_metadata_attempted = true;
            report.metadata_attempted += 1;
        }
    }
    report.cancelled = cancel.load(Ordering::Relaxed);
    index.save(root)?;
    Ok(report)
}

/// 单图自动归类：最大人脸判人像；非人像先试 ImageNet 物体类（植物动物/生日/
/// 聚会/美食/截图·文档），再试 Places365 场景类（风景/建筑/交通/运动/冒险/探险），
/// 均未命中兜底"未分类"。
fn auto_album(
    path: &Path,
    detector: Option<&FaceClassifier>,
    identifier: Option<&FaceIdentifier>,
    mobileclip: Option<&MobileClipTagger>,
    tagger: Option<&SceneTagger>,
    places: Option<&PlacesTagger>,
    min_confidence: f32,
    places_min_confidence: f32,
) -> (String, Option<String>) {
    let Some(detector) = detector else {
        return (ALBUM_UNCLASSIFIED.to_string(), None);
    };
    let image = match crate::imageio::load(path) {
        Ok(image) => image,
        Err(_) => return (ALBUM_UNCLASSIFIED.to_string(), None),
    };
    let Ok(info) = detector.classify(&image) else {
        return (ALBUM_UNCLASSIFIED.to_string(), None);
    };
    let face = info.faces.iter().max_by(|a, b| {
        a.area()
            .partial_cmp(&b.area())
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    if info.kind == SceneKind::Portrait && face.is_some() {
        let person = identifier
            .and_then(|identifier| identifier.identify(&image, face.expect("checked")).ok());
        return (ALBUM_PORTRAIT.to_string(), person);
    }
    let unclassified = || (ALBUM_UNCLASSIFIED.to_string(), None);
    // 书页线稿/图表：近白底 + 少量深色笔画，场景模型常误判为建筑/风景。
    if looks_like_printed_page(&image) {
        return (ALBUM_SCREEN_DOC.to_string(), None);
    }
    if let Some(mobileclip) = mobileclip {
        match mobileclip.tag(&image) {
            Ok(Some(tag)) => return (tag.album.to_string(), None),
            Ok(None) => {}
            Err(error) => {
                tracing::warn!(
                    %error,
                    path = %path.display(),
                    "MobileCLIP inference failed; falling back to legacy classifiers"
                );
            }
        }
    }
    if let Some(tagger) = tagger {
        if let Ok(tag) = tagger.tag(&image) {
            if tag.confidence >= min_confidence {
                if let Some(album) = album_for_class(tag.index, &tag.class) {
                    return (album.to_string(), None);
                }
            }
        }
    }
    if let Some(places) = places {
        if let Ok(tag) = places.tag(&image) {
            if tag.confidence >= places_min_confidence {
                if let Some(album) = album_for_places_class(&tag.class) {
                    return (album.to_string(), None);
                }
            }
        }
    }
    unclassified()
}

/// 判断是否像印刷页/线稿：几乎无彩色、大面积浅底、深色笔画占比较少。
/// 用于避免 Places365 把白底线框图误判为建筑（如“skyscraper”）。
fn looks_like_printed_page(image: &crate::imageio::ImageF32) -> bool {
    let pixels = image.width as usize * image.height as usize;
    if pixels == 0 || image.data.len() < pixels * crate::imageio::CHANNELS {
        return false;
    }
    let mut colored = 0usize;
    let mut light = 0usize;
    let mut dark = 0usize;
    for i in 0..pixels {
        let base = i * crate::imageio::CHANNELS;
        let r = image.data[base];
        let g = image.data[base + 1];
        let b = image.data[base + 2];
        let max = r.max(g).max(b);
        let min = r.min(g).min(b);
        if max - min > 0.06 {
            colored += 1;
        }
        let lum = (r + g + b) / 3.0;
        if lum > 0.8 {
            light += 1;
        } else if lum < 0.35 {
            dark += 1;
        }
    }
    let pixel_count = pixels as f32;
    colored as f32 / pixel_count <= 0.02
        && light as f32 / pixel_count >= 0.5
        && dark as f32 / pixel_count <= 0.35
}

/// 从索引移除条目（不删除磁盘文件）。返回是否实际移除。
pub fn remove_from_index(root: &Path, rel: &Path) -> Result<bool> {
    let mut index = LibraryIndex::load(root)?;
    let before = index.entries.len();
    index.entries.retain(|entry| entry.rel_path != rel);
    if index.entries.len() == before {
        return Ok(false);
    }
    index.save(root)?;
    Ok(true)
}

/// 一键显影产物路径：`<root>/.sepiola/developed/<镜像相对路径>.jpg`
/// （如 `sub/a.nef` → `developed/sub/a.nef.jpg`）。
/// 镜像子目录结构并保留源扩展名，避免同名/同 stem 的 RAW 互相覆盖。
pub fn developed_path(root: &Path, rel: &Path) -> PathBuf {
    let name = rel
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    root.join(LIBRARY_DIR)
        .join("developed")
        .join(rel.parent().unwrap_or(Path::new("")))
        .join(format!("{name}.jpg"))
}

/// 把库内 RAW 显影为 JPEG（8bit，质量取 `config.jpeg_quality`）。
///
/// 产物已存在且不早于源文件 mtime 时直接复用（幂等）；
/// 源缺失或显影失败返回 `Err`。
pub fn develop_file(root: &Path, rel: &Path, config: &Config) -> Result<PathBuf> {
    let src = root.join(rel);
    if !src.is_file() {
        return Err(Error::Develop(format!(
            "源文件不存在：{}",
            src.display()
        )));
    }
    let out = developed_path(root, rel);
    let fresh = matches!(
        (src.metadata().and_then(|m| m.modified()), out.metadata().and_then(|m| m.modified())),
        (Ok(src_t), Ok(out_t)) if out_t >= src_t
    );
    if fresh {
        return Ok(out);
    }
    let backend = develop::Backend::from_settings(&config.develop)?;
    let dir = out
        .parent()
        .ok_or_else(|| Error::Develop("developed path has no parent".to_string()))?;
    std::fs::create_dir_all(dir)?;
    // 中间 16-bit TIFF 放独立工作目录（同 stem 的不同 RAW 并行显影不互踩），完成后清理。
    let seq = DEVELOP_SEQ.fetch_add(1, Ordering::Relaxed);
    let work = dir.join(format!(".work-{}-{seq}", std::process::id()));
    let tiff = develop::raw_to_tiff(&src, &work, backend);
    let encoded = tiff
        .and_then(|tiff| {
            let img = crate::imageio::load(&tiff)?;
            crate::imageio::save_jpeg(&img, &out, config.jpeg_quality)
        })
        .and_then(|()| {
            let _ = std::fs::remove_dir_all(&work);
            Ok(out.clone())
        });
    if encoded.is_err() {
        let _ = std::fs::remove_dir_all(&work);
    }
    encoded
}

/// 重新打标签：移动到指定相册并标记 manual（自动归类不再覆盖）。
pub fn retag(root: &Path, rel: &Path, album: &str) -> Result<()> {
    let mut index = LibraryIndex::load(root)?;
    let entry = index
        .find_mut(rel)
        .ok_or_else(|| Error::Config(format!("not in library index: {}", rel.display())))?;
    entry.album = album.to_string();
    entry.manual = true;
    index.save(root)
}

fn walk(
    root: &Path,
    dir: &Path,
    files: &mut Vec<PathBuf>,
    entities: &mut Vec<(PathBuf, FileKind)>,
    cancel: &AtomicBool,
) -> Result<bool> {
    if cancel.load(Ordering::Relaxed) {
        return Ok(true);
    }
    let entries = std::fs::read_dir(dir)
        .map_err(|e| Error::Config(format!("failed to read {}: {e}", dir.display())))?;
    for entry in entries {
        if cancel.load(Ordering::Relaxed) {
            return Ok(true);
        }
        let path = entry
            .map_err(|e| Error::Config(format!("failed to read entry in {}: {e}", dir.display())))?
            .path();
        if path.is_dir() {
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default();
            if name.starts_with('.') {
                continue; // 跳过 .sepiola 等隐藏目录
            }
            // 项目/游戏目录折叠为单个实体条目，内部文件不再入库。
            if let Some(kind) = entity_kind_of_dir(&path) {
                entities.push((
                    path.strip_prefix(root).unwrap_or(&path).to_path_buf(),
                    kind,
                ));
                continue;
            }
            if walk(root, &path, files, entities, cancel)? {
                return Ok(true);
            }
        } else if path.is_file() {
            files.push(path.strip_prefix(root).unwrap_or(&path).to_path_buf());
        }
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!("sepiola-lib-{tag}-{nanos}"));
        std::fs::create_dir_all(&dir).expect("create temp root");
        dir
    }

    #[test]
    fn printed_page_detection_rejects_line_art_on_white() {
        let size = 64usize;
        let mut data = vec![0.95f32; size * size * crate::imageio::CHANNELS];
        // 横竖若干细线模拟线框图。
        for y in 8..size - 8 {
            if y % 8 == 0 {
                for x in 8..size - 8 {
                    let i = (y * size + x) * crate::imageio::CHANNELS;
                    data[i..i + 3].fill(0.1);
                }
            }
        }
        let image = crate::imageio::ImageF32 {
            width: size as u32,
            height: size as u32,
            data,
        };
        assert!(looks_like_printed_page(&image));

        let colored = crate::imageio::ImageF32 {
            width: 2,
            height: 2,
            data: vec![0.9, 0.2, 0.1, 0.1, 0.6, 0.2, 0.2, 0.3, 0.8, 0.9, 0.7, 0.1],
        };
        assert!(!looks_like_printed_page(&colored));
    }

    #[test]
    fn classify_kind_buckets_by_extension() {
        assert_eq!(classify_kind(Path::new("a.JPG")), FileKind::Image);
        assert_eq!(classify_kind(Path::new("a.CR2")), FileKind::Image);
        assert_eq!(classify_kind(Path::new("a.mp4")), FileKind::Video);
        assert_eq!(classify_kind(Path::new("a.flac")), FileKind::Audio);
        assert_eq!(classify_kind(Path::new("a.pdf")), FileKind::Book);
        assert_eq!(classify_kind(Path::new("a.zip")), FileKind::Archive);
        assert_eq!(classify_kind(Path::new("notes")), FileKind::Other);
    }

    #[test]
    fn classify_kind_new_buckets() {
        assert_eq!(classify_kind(Path::new("a.epub")), FileKind::Book);
        assert_eq!(classify_kind(Path::new("a.mobi")), FileKind::Book);
        assert_eq!(classify_kind(Path::new("a.azw3")), FileKind::Book);
        assert_eq!(classify_kind(Path::new("a.asw3")), FileKind::Book);
        assert_eq!(classify_kind(Path::new("novel.txt")), FileKind::Book);
        assert_eq!(classify_kind(Path::new("a.doc")), FileKind::Document);
        assert_eq!(classify_kind(Path::new("a.xls")), FileKind::Document);
        assert_eq!(classify_kind(Path::new("a.wps")), FileKind::Document);
        assert_eq!(classify_kind(Path::new("a.markdown")), FileKind::Document);
        assert_eq!(classify_kind(Path::new("a.exe")), FileKind::Installer);
        assert_eq!(classify_kind(Path::new("a.deb")), FileKind::Installer);
        assert_eq!(classify_kind(Path::new("driver.inf")), FileKind::Driver);
        assert_eq!(classify_kind(Path::new("录音_01.m4a")), FileKind::Recording);
        assert_eq!(
            classify_kind(Path::new("Recording_20240101.wav")),
            FileKind::Recording
        );
        assert_eq!(classify_kind(Path::new("song.mp3")), FileKind::Audio);
    }

    #[test]
    fn old_audio_index_entries_default_new_metadata_fields() {
        let json = r#"{"rel_path":"song.flac","kind":"audio","album":"","person":null,"manual":false,"book_meta":null,"image_classification_version":0,"book_metadata_attempted":false}"#;
        let entry: LibraryEntry = serde_json::from_str(json).expect("legacy entry");
        assert!(entry.audio_meta.is_none());
        assert!(!entry.audio_metadata_attempted);
    }

    #[test]
    fn audio_metadata_task_is_lazy_and_retries_failed_entries() {
        let root = temp_root("audio-meta");
        std::fs::write(root.join("track.flac"), b"invalid audio").expect("write audio");
        let mut index = LibraryIndex::default();
        index.entries.push(LibraryEntry {
            rel_path: PathBuf::from("track.flac"),
            kind: FileKind::Audio,
            album: String::new(),
            person: None,
            manual: true,
            book_meta: None,
            audio_meta: None,
            image_classification_version: 0,
            book_metadata_attempted: false,
            audio_metadata_attempted: false,
        });
        index.save(&root).expect("save index");
        let cancel = AtomicBool::new(false);

        let first = extract_audio_metadata(&root, false, |_| (), &cancel).expect("first scan");
        assert_eq!(first.metadata_attempted, 1);
        let index = LibraryIndex::load(&root).expect("load index");
        let entry = index.find(Path::new("track.flac")).expect("audio entry");
        assert!(entry.audio_meta.is_none());
        assert!(entry.audio_metadata_attempted);
        drop(index);

        let skipped = extract_audio_metadata(&root, false, |_| (), &cancel).expect("skip failed");
        assert_eq!(skipped.metadata_attempted, 0);
        let retried = extract_audio_metadata(&root, true, |_| (), &cancel).expect("retry failed");
        assert_eq!(retried.metadata_attempted, 1);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn scan_collapses_project_and_game_dirs() {
        let root = temp_root("entity");
        std::fs::create_dir_all(root.join("proj/src")).expect("mkdir");
        std::fs::write(root.join("proj/Cargo.toml"), b"[package]").expect("write");
        std::fs::write(root.join("proj/src/main.rs"), b"fn main(){}").expect("write");
        std::fs::create_dir_all(root.join("game/steamapps")).expect("mkdir");
        std::fs::write(root.join("game/data.pck"), b"x").expect("write");
        std::fs::write(root.join("top.jpg"), b"x").expect("write");

        let cancel = AtomicBool::new(true);
        let report = scan(&root, &Config::default(), |_| (), &cancel).expect("cancelled scan");
        assert!(report.cancelled);
        let index = LibraryIndex::load(&root).expect("load after cancellation");
        assert_eq!(index.version, 0);
        assert!(index.entries.is_empty());

        let cancel = AtomicBool::new(false);
        scan(&root, &Config::default(), |_| (), &cancel).expect("retry scan");
        let index = LibraryIndex::load(&root).expect("load");
        let proj = index.find(Path::new("proj")).expect("project entry");
        assert_eq!(proj.kind, FileKind::Project);
        assert_eq!(proj.album, "proj");
        assert_eq!(
            index.find(Path::new("game")).expect("game entry").kind,
            FileKind::Game
        );
        // 实体目录内部文件不单独入库。
        assert!(index.find(Path::new("proj/src/main.rs")).is_none());
        assert!(index.find(Path::new("game/data.pck")).is_none());
        let counts = index.kind_counts();
        assert_eq!(counts.get(&FileKind::Project), Some(&1));
        assert_eq!(counts.get(&FileKind::Game), Some(&1));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn migration_v4_reclassifies_old_kinds() {
        let root = temp_root("migrate4");
        std::fs::write(root.join("a.epub"), b"x").expect("write");
        // 手工构造 v3 旧索引：书籍被标为文档，首次扫描应改判并升版。
        let index = LibraryIndex {
            version: 3,
            entries: vec![LibraryEntry {
                rel_path: PathBuf::from("a.epub"),
                kind: FileKind::Document,
                album: String::new(),
                person: None,
                manual: false,
                book_meta: None,
                audio_meta: None,
                image_classification_version: 0,
                book_metadata_attempted: false,
                audio_metadata_attempted: false,
            }],
        };
        index.save(&root).expect("save");
        let cancel = AtomicBool::new(false);
        scan(&root, &Config::default(), |_| (), &cancel).expect("scan");
        let index = LibraryIndex::load(&root).expect("load");
        assert_eq!(
            index.find(Path::new("a.epub")).expect("entry").kind,
            FileKind::Book
        );
        assert_eq!(index.version, 9);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn migration_v9_reclassifies_book_and_document_formats() {
        let root = temp_root("migrate8-formats");
        std::fs::write(
            root.join("book.pdf"),
            book::build_test_pdf(&["ISBN 978-0-00-000000-1"]),
        )
        .expect("write book pdf");
        std::fs::write(
            root.join("report.pdf"),
            book::build_test_pdf(&["Annual report Copyright 2024"]),
        )
        .expect("write report pdf");
        std::fs::write(root.join("book.asw3"), b"kind-only fixture").expect("write asw3");
        std::fs::write(root.join("notes.markdown"), "notes").expect("write markdown");
        let index = LibraryIndex {
            version: 7,
            entries: [
                ("book.pdf", FileKind::Document),
                ("report.pdf", FileKind::Document),
                ("book.asw3", FileKind::Other),
                ("notes.markdown", FileKind::Other),
            ]
                .into_iter()
                .map(|(path, kind)| LibraryEntry {
                    rel_path: PathBuf::from(path),
                    kind,
                    album: String::new(),
                    person: None,
                    manual: false,
                    book_meta: None,
                    audio_meta: None,
                    image_classification_version: 0,
                    book_metadata_attempted: false,
                    audio_metadata_attempted: false,
                })
                .collect(),
        };
        index.save(&root).expect("save");

        let cancel = AtomicBool::new(false);
        scan(&root, &Config::default(), |_| (), &cancel).expect("scan");
        let index = LibraryIndex::load(&root).expect("load");
        assert_eq!(
            index.find(Path::new("book.pdf")).expect("book entry").kind,
            FileKind::Book
        );
        assert_eq!(
            index.find(Path::new("report.pdf")).expect("report entry").kind,
            FileKind::Book
        );
        assert_eq!(
            index.find(Path::new("book.asw3")).expect("asw3 entry").kind,
            FileKind::Book
        );
        assert_eq!(
            index.find(Path::new("notes.markdown")).expect("markdown entry").kind,
            FileKind::Document
        );
        assert_eq!(index.version, 9);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn book_metadata_is_extracted_only_by_book_task() {
        let root = temp_root("migrate8-book-meta");
        std::fs::write(
            root.join("book.fb2"),
            r#"<?xml version="1.0"?><FictionBook><description><title-info>
<book-title>回填测试书</book-title><author><last-name>测试作者</last-name></author>
</title-info></description></FictionBook>"#,
        )
        .expect("write fb2");
        let index = LibraryIndex {
            version: 8,
            entries: vec![LibraryEntry {
                rel_path: PathBuf::from("book.fb2"),
                kind: FileKind::Book,
                album: String::new(),
                person: None,
                manual: false,
                book_meta: None,
                audio_meta: None,
                image_classification_version: 0,
                book_metadata_attempted: false,
                audio_metadata_attempted: false,
            }],
        };
        index.save(&root).expect("save");

        let cancel = AtomicBool::new(false);
        scan(&root, &Config::default(), |_| (), &cancel).expect("scan");
        let index = LibraryIndex::load(&root).expect("load");
        let entry = index.find(Path::new("book.fb2")).expect("book entry");
        assert!(entry.book_meta.is_none());
        assert!(!entry.book_metadata_attempted);
        drop(index);

        let report = extract_book_metadata(&root, false, |_| (), &cancel).expect("extract");
        assert_eq!(report.metadata_attempted, 1);
        let index = LibraryIndex::load(&root).expect("load after extraction");
        let meta = index
            .find(Path::new("book.fb2"))
            .and_then(|entry| entry.book_meta.as_ref())
            .expect("backfilled book metadata");
        assert_eq!(meta.title.as_deref(), Some("回填测试书"));
        assert_eq!(meta.author.as_deref(), Some("测试作者"));
        assert!(index
            .find(Path::new("book.fb2"))
            .expect("book entry")
            .book_metadata_attempted);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn migration_v5_backfills_book_meta() {
        let root = temp_root("migrate5");
        // v4 索引已有书籍条目但无元数据；类型扫描不应解析正文。
        std::fs::write(
            root.join("a.fb2"),
            r#"<?xml version="1.0"?><FictionBook><description><title-info>
<book-title>测试书</book-title><author><last-name>测试者</last-name></author>
</title-info></description></FictionBook>"#,
        )
        .expect("write fb2");
        let index = LibraryIndex {
            version: 4,
            entries: vec![LibraryEntry {
                rel_path: PathBuf::from("a.fb2"),
                kind: FileKind::Book,
                album: String::new(),
                person: None,
                manual: false,
                book_meta: None,
                audio_meta: None,
                image_classification_version: 0,
                book_metadata_attempted: false,
                audio_metadata_attempted: false,
            }],
        };
        index.save(&root).expect("save");
        let cancel = AtomicBool::new(false);
        scan(&root, &Config::default(), |_| (), &cancel).expect("scan");
        let index = LibraryIndex::load(&root).expect("load");
        let entry = index.find(Path::new("a.fb2")).expect("entry");
        assert_eq!(entry.kind, FileKind::Book);
        assert!(entry.book_meta.is_none());
        assert!(!entry.book_metadata_attempted);
        assert_eq!(index.version, 9);
        drop(index);

        extract_book_metadata(&root, false, |_| (), &cancel).expect("extract book meta");
        let index = LibraryIndex::load(&root).expect("load after extraction");
        let entry = index.find(Path::new("a.fb2")).expect("entry");
        let meta = entry.book_meta.as_ref().expect("book meta backfilled");
        assert_eq!(meta.title.as_deref(), Some("测试书"));
        assert_eq!(meta.author.as_deref(), Some("测试者"));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn type_scan_defers_image_and_book_processing() {
        let root = temp_root("type-scan-only");
        std::fs::write(root.join("photo.jpg"), b"not an image").expect("write image");
        std::fs::write(
            root.join("book.fb2"),
            r#"<?xml version="1.0"?><FictionBook><description><title-info>
<book-title>Deferred</book-title></title-info></description></FictionBook>"#,
        )
        .expect("write book");

        let cancel = AtomicBool::new(false);
        let report = scan(&root, &Config::default(), |_| (), &cancel).expect("type scan");
        assert_eq!(report.added, 2);
        let index = LibraryIndex::load(&root).expect("load index");
        let image = index.find(Path::new("photo.jpg")).expect("image entry");
        assert_eq!(image.album, ALBUM_UNCLASSIFIED);
        assert_eq!(image.image_classification_version, 0);
        let book = index.find(Path::new("book.fb2")).expect("book entry");
        assert!(book.book_meta.is_none());
        assert!(!book.book_metadata_attempted);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn failed_book_metadata_is_not_retried_without_request() {
        let root = temp_root("book-meta-retry");
        std::fs::write(root.join("broken.fb2"), b"not valid fb2").expect("write book");
        let cancel = AtomicBool::new(false);
        scan(&root, &Config::default(), |_| (), &cancel).expect("type scan");

        let first = extract_book_metadata(&root, false, |_| (), &cancel).expect("first attempt");
        assert_eq!(first.metadata_attempted, 1);
        let index = LibraryIndex::load(&root).expect("load index");
        let entry = index.find(Path::new("broken.fb2")).expect("book entry");
        assert!(entry.book_meta.is_none());
        assert!(entry.book_metadata_attempted);
        drop(index);

        let skipped = extract_book_metadata(&root, false, |_| (), &cancel).expect("skip attempt");
        assert_eq!(skipped.metadata_attempted, 0);
        let retried = extract_book_metadata(&root, true, |_| (), &cancel).expect("retry attempt");
        assert_eq!(retried.metadata_attempted, 1);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn scan_adds_removes_and_skips_hidden_dirs() {
        let root = temp_root("scan");
        std::fs::create_dir_all(root.join("sub")).expect("mkdir");
        std::fs::create_dir_all(root.join(LIBRARY_DIR)).expect("mkdir meta");
        std::fs::write(root.join("a.jpg"), b"x").expect("write");
        std::fs::write(root.join("sub/b.mp4"), b"x").expect("write");
        std::fs::write(root.join(LIBRARY_DIR).join("notes.txt"), b"meta").expect("write");

        let cancel = AtomicBool::new(false);
        let report = scan(&root, &Config::default(), |_| (), &cancel).expect("scan");
        assert_eq!(report.added, 2);
        assert_eq!(report.removed, 0);

        let index = LibraryIndex::load(&root).expect("load");
        assert_eq!(index.entries.len(), 2);
        assert!(!index
            .entries
            .iter()
            .any(|entry| entry.rel_path.starts_with(LIBRARY_DIR)));
        let video = index.find(Path::new("sub/b.mp4")).expect("video entry");
        assert_eq!(video.kind, FileKind::Video);
        assert!(video.album.is_empty());

        std::fs::remove_file(root.join("a.jpg")).expect("remove");
        let report = scan(&root, &Config::default(), |_| (), &cancel).expect("rescan");
        assert_eq!(report.added, 0);
        assert_eq!(report.removed, 1);
        assert_eq!(LibraryIndex::load(&root).expect("load").entries.len(), 1);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn retag_persists_and_protects_manual_entries() {
        let root = temp_root("retag");
        std::fs::write(root.join("a.jpg"), b"x").expect("write");
        let cancel = AtomicBool::new(false);
        scan(&root, &Config::default(), |_| (), &cancel).expect("scan");
        retag(&root, Path::new("a.jpg"), "生日").expect("retag");

        let index = LibraryIndex::load(&root).expect("load");
        let entry = index.find(Path::new("a.jpg")).expect("entry");
        assert_eq!(entry.album, "生日");
        assert!(entry.manual);

        // 重扫不覆盖 manual 条目。
        drop(index);
        scan(&root, &Config::default(), |_| (), &cancel).expect("rescan");
        let index = LibraryIndex::load(&root).expect("load");
        assert_eq!(index.find(Path::new("a.jpg")).expect("entry").album, "生日");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn developed_path_mirrors_and_keeps_source_extension() {
        let root = Path::new("/lib");
        assert_eq!(
            developed_path(root, Path::new("sub/a.nef")),
            root.join(LIBRARY_DIR)
                .join("developed")
                .join("sub")
                .join("a.nef.jpg")
        );
        // 同 stem 不同扩展的 RAW 产物不冲突。
        assert_ne!(
            developed_path(root, Path::new("a.nef")),
            developed_path(root, Path::new("a.dng"))
        );
    }

    #[test]
    fn develop_file_creates_jpeg_and_is_idempotent() {
        let sample = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("test_data/raw/nikon_d70.nef");
        if !sample.is_file() {
            eprintln!("no raw sample, skipping");
            return;
        }
        let root = temp_root("develop");
        std::fs::create_dir_all(root.join("sub")).expect("mkdir");
        std::fs::copy(&sample, root.join("sub/a.nef")).expect("copy");

        let mut config = Config::default();
        config.develop.backend = "builtin".to_string();
        let out = develop_file(&root, Path::new("sub/a.nef"), &config).expect("develop");
        assert!(out.is_file());
        assert!(out.starts_with(root.join(LIBRARY_DIR).join("developed")));
        assert!(image::open(&out).is_ok(), "产物应可被 image 解码");

        let mtime = std::fs::metadata(&out).expect("meta").modified().expect("mtime");
        let again = develop_file(&root, Path::new("sub/a.nef"), &config).expect("reuse");
        assert_eq!(again, out);
        assert_eq!(
            std::fs::metadata(&out).expect("meta").modified().expect("mtime"),
            mtime,
            "产物新鲜时不应重新显影"
        );
        // 工作目录已清理。
        let leftovers: Vec<_> = std::fs::read_dir(root.join(LIBRARY_DIR).join("developed").join("sub"))
            .expect("read developed dir")
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().starts_with(".work-"))
            .collect();
        assert!(leftovers.is_empty(), "工作目录未清理");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn develop_file_missing_source_errors() {
        let root = temp_root("develop-missing");
        let mut config = Config::default();
        config.develop.backend = "builtin".to_string();
        assert!(develop_file(&root, Path::new("nope.nef"), &config).is_err());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn albums_counts_images_only() {
        let mut index = LibraryIndex::default();
        index.entries.push(LibraryEntry {
            rel_path: PathBuf::from("p.jpg"),
            kind: FileKind::Image,
            album: ALBUM_PORTRAIT.to_string(),
            person: Some("person_1".to_string()),
            manual: false,
            book_meta: None,
            audio_meta: None,
            image_classification_version: 0,
            book_metadata_attempted: false,
            audio_metadata_attempted: false,
        });
        index.entries.push(LibraryEntry {
            rel_path: PathBuf::from("s.jpg"),
            kind: FileKind::Image,
            album: ALBUM_SCENERY.to_string(),
            person: None,
            manual: false,
            book_meta: None,
            audio_meta: None,
            image_classification_version: 0,
            book_metadata_attempted: false,
            audio_metadata_attempted: false,
        });
        index.entries.push(LibraryEntry {
            rel_path: PathBuf::from("v.mp4"),
            kind: FileKind::Video,
            album: String::new(),
            person: None,
            manual: false,
            book_meta: None,
            audio_meta: None,
            image_classification_version: 0,
            book_metadata_attempted: false,
            audio_metadata_attempted: false,
        });
        let albums = index.albums();
        assert_eq!(albums.get(ALBUM_PORTRAIT), Some(&1));
        assert_eq!(albums.get(ALBUM_SCENERY), Some(&1));
        assert_eq!(albums.len(), 2);
        assert_eq!(index.images_in(ALBUM_PORTRAIT).len(), 1);
    }

    #[test]
    fn remove_from_index_drops_only_target_entry() {
        let root = temp_root("remove");
        std::fs::write(root.join("a.jpg"), b"x").expect("write");
        std::fs::write(root.join("b.jpg"), b"x").expect("write");
        let cancel = AtomicBool::new(false);
        scan(&root, &Config::default(), |_| (), &cancel).expect("scan");

        assert!(remove_from_index(&root, Path::new("a.jpg")).expect("remove"));
        assert!(!remove_from_index(&root, Path::new("a.jpg")).expect("idempotent"));
        let index = LibraryIndex::load(&root).expect("load");
        assert_eq!(index.entries.len(), 1);
        assert!(index.find(Path::new("b.jpg")).is_some());
        assert!(root.join("a.jpg").is_file()); // 磁盘文件不动
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn cancelled_scan_leaves_existing_entries_saved() {
        let root = temp_root("cancel");
        for name in ["a.jpg", "b.jpg", "c.jpg"] {
            std::fs::write(root.join(name), b"x").expect("write");
        }
        let cancel = AtomicBool::new(true);
        let report = scan(&root, &Config::default(), |_| (), &cancel).expect("scan");
        assert_eq!(report.added, 0);
        std::fs::remove_dir_all(&root).ok();
    }
}
