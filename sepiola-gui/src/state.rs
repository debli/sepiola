//! App 状态、结果行与配置组装。

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::Receiver;
use std::sync::Arc;

use eframe::egui;
use sepiola::config::{Config, IdentitySettings, SceneConfig};
use sepiola::scene::SceneKind;

use crate::album_view::AlbumState;
use crate::assets::{
    build_look_previews, discover_model, install_cjk_font, scene_config, LookPreview,
};
use crate::compare::CompareState;
use crate::library::{non_empty_root, LibraryState, Tab};
use crate::user_settings::UserSettings;
use crate::worker::UiMessage;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum BackendChoice {
    Auto,
    Darktable,
    Builtin,
}

impl BackendChoice {
    pub fn as_str(self) -> &'static str {
        match self {
            BackendChoice::Auto => "auto",
            BackendChoice::Darktable => "darktable",
            BackendChoice::Builtin => "builtin",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ClassifierChoice {
    Face,
    Stub,
}

impl ClassifierChoice {
    pub fn as_str(self) -> &'static str {
        match self {
            ClassifierChoice::Face => "face",
            ClassifierChoice::Stub => "stub",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum PixelFormat {
    Bit8,
    Bit16,
}

impl PixelFormat {
    pub fn as_str(self) -> &'static str {
        match self {
            PixelFormat::Bit8 => "8bit",
            PixelFormat::Bit16 => "16bit",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            PixelFormat::Bit8 => "8bit（JPEG）",
            PixelFormat::Bit16 => "16bit（TIFF）",
        }
    }
}

/// 每张图的处理结果，供界面列表展示。
pub struct Row {
    pub input: PathBuf,
    pub output: PathBuf,
    pub ok: bool,
    pub scene: Option<SceneKind>,
    pub person: Option<String>,
    pub error: Option<String>,
    /// 用户是否保留该输出（默认 true）。
    pub kept: bool,
    /// 输出文件已被删除。
    pub deleted: bool,
}

pub struct Preview {
    pub path: PathBuf,
    pub texture: egui::TextureHandle,
}

pub struct SepiolaApp {
    pub tab: Tab,
    pub library: LibraryState,
    pub compare: CompareState,
    pub album: Arc<std::sync::Mutex<AlbumState>>,
    pub user_settings: UserSettings,
    pub input_dir: String,
    pub output_dir: String,
    pub backend: BackendChoice,
    pub classifier: ClassifierChoice,
    pub workers: usize,
    pub jpeg_quality: u8,
    pub max_clipped_high: f32,
    pub model_path: String,
    pub portrait_lut: String,
    pub portrait_strength: f32,
    pub portrait_look: String,
    pub portrait_look_strength: f32,
    pub portrait_exposure_ev: f32,
    pub other_lut: String,
    pub other_strength: f32,
    pub other_look: String,
    pub other_look_strength: f32,
    pub other_exposure_ev: f32,
    pub look_previews: Vec<LookPreview>,
    pub pixel_format: PixelFormat,
    pub keep_metadata: bool,
    pub identity_enabled: bool,
    pub identity_model: String,
    pub landmark_model: String,
    pub identity_threshold: f32,
    pub gallery_file: String,

    pub running: bool,
    /// 已请求取消（等待处理线程收尾）。
    pub cancelling: bool,
    /// 传给批处理线程的取消标志。
    pub cancel_flag: Arc<AtomicBool>,
    pub progress: (usize, usize),
    pub message: String,
    pub rows: Vec<Row>,
    pub receiver: Option<Receiver<UiMessage>>,
    pub selected: Option<usize>,
    pub preview: Option<Preview>,
    pub zoom_open: bool,
    pub zoom_scale: f32,
    /// 阅读窗口是否打开（独立窗口，不影响库页当前页签）。
    pub reader_window: bool,
    /// AI 设置页的 CLI 探测结果（后台线程）。
    pub ai_probe: Option<Receiver<Result<String, String>>>,
    /// AI 用量缓存（日志修改时间, 汇总），避免设置页每帧重读日志。
    pub ai_usage: Option<(std::time::SystemTime, sepiola::ai::UsageSummary)>,
    /// 记忆卡片笔记本。
    pub notebook: crate::notebook_view::NotebookState,
    /// 正在管理人物标签的行号。
    pub person_edit: Option<usize>,
    pub person_edit_text: String,
    /// 人物合并窗口的目标标签草稿。
    pub merge_target: String,
    /// 删除人物时是否同时删除输出目录。
    pub remove_delete_dir: bool,
    /// 弹窗内 Markdown 文字的 label 选区（由 egui 插件在帧末写入）。
    pub label_selection: crate::reader_selection::LabelSelectionSink,
}

impl SepiolaApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        install_cjk_font(&cc.egui_ctx);
        cc.egui_ctx.all_styles_mut(|style| {
            style.spacing.item_spacing = egui::vec2(14.0, 10.0);
            // 按钮/下拉框/输入框的内边距：直接决定这些控件的高度与观感大小。
            style.spacing.button_padding = egui::vec2(14.0, 8.0);
            // 悬停可点击按钮时统一显示手型光标（egui 0.37 的 Button 原生支持）。
            style.visuals.interact_cursor = Some(egui::CursorIcon::PointingHand);
        });
        let user_settings = UserSettings::load_or_default();
        let tab = if user_settings.library_root.trim().is_empty() {
            Tab::Process
        } else {
            Tab::Library
        };
        let mut library = LibraryState::default();
        library.root = user_settings.library_root.clone();
        if let Some(root) = non_empty_root(&library.root) {
            if root.is_dir() {
                library.index = sepiola::library::LibraryIndex::load(&root).unwrap_or_default();
            } else {
                library.message = format!("上次书库目录不存在：{}", root.display());
            }
        }
        let model_path = if user_settings.model_path.trim().is_empty() {
            discover_model(
                "models/face_detection_yunet_2023mar.onnx",
                "face_detection_yunet",
            )
        } else {
            discover_model(&user_settings.model_path, "face_detection_yunet")
        };
        let identity_model = if user_settings.identity_model.trim().is_empty() {
            discover_model("models/arcfaceresnet100-11-int8.onnx", "arcfaceresnet100")
        } else {
            discover_model(&user_settings.identity_model, "arcfaceresnet100")
        };
        let landmark_model = if user_settings.landmark_model.trim().is_empty() {
            discover_model("models/2d106det.onnx", "2d106det")
        } else {
            discover_model(&user_settings.landmark_model, "2d106det")
        };
        let look_previews = build_look_previews(&cc.egui_ctx);
        let merged = crate::reader::merged_reader_settings(&user_settings);
        library.reader.ai = merged.ai.clone();
        library.reader.dictionary = merged.dictionary;
        let mut notebook = crate::notebook_view::NotebookState::default();
        if let Some(root) = non_empty_root(&library.root) {
            notebook.reload(&root, merged.ai);
        }
        let label_selection = crate::reader_selection::LabelSelectionSink::default();
        cc.egui_ctx.add_plugin(label_selection.clone());
        Self {
            label_selection,
            tab,
            library,
            compare: CompareState::default(),
            album: Arc::new(std::sync::Mutex::new(AlbumState::default())),
            user_settings: user_settings.clone(),
            input_dir: user_settings.input_dir.clone(),
            output_dir: user_settings.output_dir.clone(),
            backend: match user_settings.backend.as_str() {
                "darktable" => BackendChoice::Darktable,
                "builtin" => BackendChoice::Builtin,
                _ => BackendChoice::Auto,
            },
            classifier: match user_settings.classifier.as_str() {
                "stub" => ClassifierChoice::Stub,
                _ => ClassifierChoice::Face,
            },
            workers: user_settings.workers.clamp(1, 64),
            jpeg_quality: user_settings.jpeg_quality.clamp(1, 100),
            max_clipped_high: user_settings.max_clipped_high.clamp(0.0, 1.0),
            model_path,
            portrait_lut: user_settings.portrait_lut.clone(),
            portrait_strength: user_settings.portrait_strength.clamp(0.0, 1.0),
            portrait_look: user_settings.portrait_look.clone(),
            portrait_look_strength: user_settings.portrait_look_strength.clamp(0.0, 1.0),
            portrait_exposure_ev: user_settings.portrait_exposure_ev.clamp(-5.0, 5.0),
            other_lut: user_settings.other_lut.clone(),
            other_strength: user_settings.other_strength.clamp(0.0, 1.0),
            other_look: user_settings.other_look.clone(),
            other_look_strength: user_settings.other_look_strength.clamp(0.0, 1.0),
            other_exposure_ev: user_settings.other_exposure_ev.clamp(-5.0, 5.0),
            look_previews,
            pixel_format: match user_settings.pixel_format.as_str() {
                "16bit" => PixelFormat::Bit16,
                _ => PixelFormat::Bit8,
            },
            keep_metadata: user_settings.keep_metadata,
            identity_enabled: user_settings.identity_enabled,
            identity_model,
            landmark_model,
            identity_threshold: user_settings.identity_threshold.clamp(0.0, 1.0),
            gallery_file: user_settings.gallery_file.clone(),

            running: false,
            cancelling: false,
            cancel_flag: Arc::new(AtomicBool::new(false)),
            progress: (0, 0),
            message: String::new(),
            rows: Vec::new(),
            receiver: None,
            selected: None,
            preview: None,
            zoom_open: false,
            zoom_scale: 1.0,
            reader_window: false,
            ai_probe: None,
            ai_usage: None,
            notebook,
            person_edit: None,
            person_edit_text: String::new(),
            merge_target: String::new(),
            remove_delete_dir: true,
        }
    }

    pub fn persist_user_settings_if_changed(&mut self) {
        let mut settings = UserSettings {
            input_dir: self.input_dir.clone(),
            output_dir: self.output_dir.clone(),
            backend: self.backend.as_str().to_string(),
            classifier: self.classifier.as_str().to_string(),
            workers: self.workers,
            jpeg_quality: self.jpeg_quality,
            max_clipped_high: self.max_clipped_high,
            model_path: self.model_path.clone(),
            portrait_lut: self.portrait_lut.clone(),
            portrait_strength: self.portrait_strength,
            portrait_look: self.portrait_look.clone(),
            portrait_look_strength: self.portrait_look_strength,
            portrait_exposure_ev: self.portrait_exposure_ev,
            other_lut: self.other_lut.clone(),
            other_strength: self.other_strength,
            other_look: self.other_look.clone(),
            other_look_strength: self.other_look_strength,
            other_exposure_ev: self.other_exposure_ev,
            pixel_format: self.pixel_format.as_str().to_string(),
            keep_metadata: self.keep_metadata,
            identity_enabled: self.identity_enabled,
            identity_model: self.identity_model.clone(),
            landmark_model: self.landmark_model.clone(),
            identity_threshold: self.identity_threshold,
            gallery_file: self.gallery_file.clone(),
            library_root: self.library.root.clone(),
            recent_library_roots: self.user_settings.recent_library_roots.clone(),
            dictionary_url: self.user_settings.dictionary_url.clone(),
            ai_provider: self.user_settings.ai_provider.clone(),
            proxy_http: self.user_settings.proxy_http.clone(),
            proxy_https: self.user_settings.proxy_https.clone(),
            ai_base_url: self.user_settings.ai_base_url.clone(),
            ai_api_key: self.user_settings.ai_api_key.clone(),
            ai_model: self.user_settings.ai_model.clone(),
            ai_cli_path: self.user_settings.ai_cli_path.clone(),
            ai_cli_args: self.user_settings.ai_cli_args.clone(),
            ai_cli_model: self.user_settings.ai_cli_model.clone(),
            ai_prompt_translate: self.user_settings.ai_prompt_translate.clone(),
            ai_prompt_explain: self.user_settings.ai_prompt_explain.clone(),
            ai_prompt_analyze: self.user_settings.ai_prompt_analyze.clone(),
            ai_prompt_exercise: self.user_settings.ai_prompt_exercise.clone(),
            ai_prompt_sentence: self.user_settings.ai_prompt_sentence.clone(),
            ai_prompt_card: self.user_settings.ai_prompt_card.clone(),
            ai_prompt_chat: self.user_settings.ai_prompt_chat.clone(),
        };
        settings.record_active_library_if_changed(&self.user_settings.library_root);
        if settings != self.user_settings {
            self.user_settings = settings.clone();
            if let Err(error) = settings.save() {
                eprintln!("Unable to save user settings: {error}");
            }
        }
    }

    pub fn build_config(&self) -> Result<Config, String> {
        let mut config = Config {
            input_dir: self.input_dir.trim().to_string(),
            output_dir: self.output_dir.trim().to_string(),
            workers: self.workers,
            jpeg_quality: self.jpeg_quality,
            ..Config::default()
        };
        config.develop.backend = self.backend.as_str().to_string();
        config.scene_settings.classifier = self.classifier.as_str().to_string();
        config.scene_settings.model = self.model_path.trim().to_string();
        config.safety.max_clipped_high = self.max_clipped_high;
        config.portrait = scene_config(
            &self.portrait_lut,
            self.portrait_strength,
            &self.portrait_look,
            self.portrait_look_strength,
            self.portrait_exposure_ev,
        );
        config.other = scene_config(
            &self.other_lut,
            self.other_strength,
            &self.other_look,
            self.other_look_strength,
            self.other_exposure_ev,
        );
        config.output.pixel_format = self.pixel_format.as_str().to_string();
        config.output.keep_metadata = self.keep_metadata;
        config.identity.enabled = self.identity_enabled;
        config.identity.model = self.identity_model.trim().to_string();
        config.identity.landmark_model = self.landmark_model.trim().to_string();
        config.identity.threshold = self.identity_threshold;
        config.identity.gallery = self.gallery_file.trim().to_string();
        config.validate().map_err(|error| error.to_string())?;
        Ok(config)
    }

    /// 把已加载的配置写回界面状态（未提供的路径保持原值，便于先选目录再套参数）。
    pub fn apply_config(&mut self, config: &Config) {
        if !config.input_dir.trim().is_empty() {
            self.input_dir = config.input_dir.clone();
        }
        if !config.output_dir.trim().is_empty() {
            self.output_dir = config.output_dir.clone();
        }
        self.workers = config.workers;
        self.jpeg_quality = config.jpeg_quality;
        self.backend = match config.develop.backend.as_str() {
            "darktable" => BackendChoice::Darktable,
            "builtin" => BackendChoice::Builtin,
            _ => BackendChoice::Auto,
        };
        self.classifier = match config.scene_settings.classifier.as_str() {
            "stub" => ClassifierChoice::Stub,
            _ => ClassifierChoice::Face,
        };
        apply_scene(
            &config.portrait,
            &mut self.portrait_lut,
            &mut self.portrait_strength,
            &mut self.portrait_look,
            &mut self.portrait_look_strength,
            &mut self.portrait_exposure_ev,
        );
        apply_scene(
            &config.other,
            &mut self.other_lut,
            &mut self.other_strength,
            &mut self.other_look,
            &mut self.other_look_strength,
            &mut self.other_exposure_ev,
        );
        self.max_clipped_high = config.safety.max_clipped_high;
        self.pixel_format = match config.output.pixel_format.as_str() {
            "16bit" => PixelFormat::Bit16,
            _ => PixelFormat::Bit8,
        };
        self.keep_metadata = config.output.keep_metadata;
        self.apply_identity(&config.identity);
        if !config.scene_settings.model.trim().is_empty() {
            self.model_path = discover_model(&config.scene_settings.model, "face_detection_yunet");
        }
    }

    /// 身份设置回填；模型路径为相对路径时仍尝试按 exe/工作目录发现。
    fn apply_identity(&mut self, identity: &IdentitySettings) {
        self.identity_enabled = identity.enabled;
        if !identity.model.trim().is_empty() {
            self.identity_model = discover_model(&identity.model, "arcfaceresnet100");
        }
        if !identity.landmark_model.trim().is_empty() {
            self.landmark_model = discover_model(&identity.landmark_model, "2d106det");
        }
        self.identity_threshold = identity.threshold;
        if !identity.gallery.trim().is_empty() {
            self.gallery_file = identity.gallery.clone();
        }
    }
}

/// 场景配置 → 界面字段（无 LUT 时清空路径与强度）。
fn apply_scene(
    scene: &SceneConfig,
    lut: &mut String,
    strength: &mut f32,
    preset: &mut String,
    look_strength: &mut f32,
    exposure_ev: &mut f32,
) {
    match &scene.lut {
        Some(config) => {
            *lut = config.file.clone();
            *strength = config.strength;
        }
        None => {
            lut.clear();
        }
    }
    if let Some(config) = &scene.look {
        *preset = config.preset.clone();
        *look_strength = config.strength;
        *exposure_ev = config.exposure_ev;
    } else {
        preset.clear();
    }
}
