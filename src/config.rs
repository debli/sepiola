//! 配置结构与 TOML 读取。

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::scene::SceneKind;

/// 顶层配置。
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Config {
    /// 输入目录；命令行必须提供，配置文件可省略。
    #[serde(default)]
    pub input_dir: String,
    /// 输出目录；命令行必须提供，配置文件可省略。
    #[serde(default)]
    pub output_dir: String,
    #[serde(default = "default_jpeg_quality")]
    pub jpeg_quality: u8,
    #[serde(default = "default_workers")]
    pub workers: usize,
    #[serde(default)]
    pub portrait: SceneConfig,
    #[serde(default)]
    pub other: SceneConfig,
    /// `[scene]`：场景分类器与模型参数。
    #[serde(default, rename = "scene")]
    pub scene_settings: SceneSettings,
    #[serde(default)]
    pub safety: SafetyConfig,
    /// `[develop]`：RAW 显影后端。
    #[serde(default)]
    pub develop: DevelopSettings,
    /// `[output]`：导出像素格式与元数据选项。
    #[serde(default, rename = "output")]
    pub output: OutputSettings,
    /// `[identity]`：人物身份识别与归档。
    #[serde(default)]
    pub identity: IdentitySettings,
    /// `[scene_tags]`：ImageNet 场景标签，用于库相册细分。
    #[serde(default)]
    pub scene_tags: SceneTagSettings,
}

/// 单个场景的 Stage 列表及各自的参数。
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct SceneConfig {
    /// 按顺序执行的 Stage 名称。
    #[serde(default)]
    pub stages: Vec<String>,
    #[serde(default)]
    pub lut: Option<LutConfig>,
}

/// `lut` Stage 的参数。
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct LutConfig {
    /// `.cube` 文件路径；相对路径优先 exe 同目录，其次当前工作目录。
    pub file: String,
    #[serde(default = "default_strength")]
    pub strength: f32,
}

/// `[scene]`：场景分类器与模型参数。
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SceneSettings {
    /// `"face"` 使用 ONNX 人脸检测；`"stub"` 返回固定场景（测试用）。
    #[serde(default = "default_classifier")]
    pub classifier: String,
    /// ONNX 模型路径。
    #[serde(default = "default_model_path")]
    pub model: String,
    /// 最大人脸占整图面积达到该比例时判为人像。
    #[serde(default = "default_portrait_min_face_ratio")]
    pub portrait_min_face_ratio: f32,
    /// 人脸检测置信度阈值。
    #[serde(default = "default_score_threshold")]
    pub score_threshold: f32,
    /// 人脸框 NMS 的 IoU 阈值。
    #[serde(default = "default_nms_threshold")]
    pub nms_threshold: f32,
}

impl Default for SceneSettings {
    fn default() -> Self {
        Self {
            classifier: default_classifier(),
            model: default_model_path(),
            portrait_min_face_ratio: default_portrait_min_face_ratio(),
            score_threshold: default_score_threshold(),
            nms_threshold: default_nms_threshold(),
        }
    }
}

/// `[develop]`：RAW 显影后端。
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct DevelopSettings {
    /// `"auto"` 优先使用 darktable，不可用时回退内置后端；
    /// 也可固定为 `"darktable"` 或 `"builtin"`。
    #[serde(default = "default_develop_backend")]
    pub backend: String,
}

impl Default for DevelopSettings {
    fn default() -> Self {
        Self {
            backend: default_develop_backend(),
        }
    }
}

/// `[identity]`：人物身份识别配置。
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct IdentitySettings {
    /// 是否启用人物识别；模型缺失时自动禁用并告警。
    #[serde(default = "default_identity_enabled")]
    pub enabled: bool,
    /// ArcFace 嵌入模型路径。
    #[serde(default = "default_identity_model")]
    pub model: String,
    /// 判定为同一人的余弦相似度阈值。
    #[serde(default = "default_identity_threshold")]
    pub threshold: f32,
    /// 人物图库（gallery）JSON 文件路径。
    #[serde(default = "default_gallery_path")]
    pub gallery: String,
    /// 关键点模型（2D106）路径，用于人脸对齐；空字符串或文件缺失时回退普通裁剪。
    #[serde(default = "default_landmark_model")]
    pub landmark_model: String,
    /// 匹配成功且差异较大时，是否把新嵌入追加进图库（Immich 式增量更新）。
    #[serde(default = "default_update_gallery")]
    pub update_gallery: bool,
    /// 每个人物额外嵌入的上限。
    #[serde(default = "default_max_extra")]
    pub max_extra: usize,
    /// 新嵌入与主嵌入余弦低于该值时才追加（避免冗余样本）。
    #[serde(default = "default_update_min_sim")]
    pub update_min_sim: f32,
}

impl Default for IdentitySettings {
    fn default() -> Self {
        Self {
            enabled: default_identity_enabled(),
            model: default_identity_model(),
            threshold: default_identity_threshold(),
            gallery: default_gallery_path(),
            landmark_model: default_landmark_model(),
            update_gallery: default_update_gallery(),
            max_extra: default_max_extra(),
            update_min_sim: default_update_min_sim(),
        }
    }
}

/// `[scene_tags]`：ImageNet 场景标签模型（库相册细分用）。
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SceneTagSettings {
    /// 是否启用场景标签细分相册；模型缺失时自动跳过并保持"风景"。
    #[serde(default = "default_scene_tags_enabled")]
    pub enabled: bool,
    /// ImageNet 分类 ONNX 模型路径。
    #[serde(default = "default_scene_tag_model")]
    pub model: String,
    /// 类名表路径（每行一个类，索引即模型输出下标）。
    #[serde(default = "default_scene_tag_labels")]
    pub labels: String,
    /// top-1 置信度低于该值时不细分（保持默认相册）。
    #[serde(default = "default_scene_tag_min_confidence")]
    pub min_confidence: f32,
    /// Places365 场景模型路径（M34 相册细分）；缺失时静默跳过 Places 细分。
    #[serde(default = "default_places_model")]
    pub places_model: String,
    /// Places365 类表路径（每行 `/x/name 0`，顺序即模型输出下标）。
    #[serde(default = "default_places_classes")]
    pub places_classes: String,
    /// Places top-1 置信度低于该值时不细分。
    #[serde(default = "default_places_min_confidence")]
    pub places_min_confidence: f32,
}

impl Default for SceneTagSettings {
    fn default() -> Self {
        Self {
            enabled: default_scene_tags_enabled(),
            model: default_scene_tag_model(),
            labels: default_scene_tag_labels(),
            min_confidence: default_scene_tag_min_confidence(),
            places_model: default_places_model(),
            places_classes: default_places_classes(),
            places_min_confidence: default_places_min_confidence(),
        }
    }
}

/// `[output]`：导出选项。
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct OutputSettings {
    /// `"8bit"`（默认，输出 JPEG）或 `"16bit"`（输出 TIFF）。
    #[serde(default = "default_pixel_format")]
    pub pixel_format: String,
    /// 是否保留源图元数据（EXIF/ICC/注释）；默认 true。
    #[serde(default = "default_keep_metadata")]
    pub keep_metadata: bool,
}

impl Default for OutputSettings {
    fn default() -> Self {
        Self {
            pixel_format: default_pixel_format(),
            keep_metadata: default_keep_metadata(),
        }
    }
}

/// 安全检查参数。
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SafetyConfig {
    /// 处理后高光裁切比例超过该值时，`strength_scale` 减半并重做一次。
    #[serde(default = "default_max_clipped_high")]
    pub max_clipped_high: f32,
}

impl Default for SafetyConfig {
    fn default() -> Self {
        Self {
            max_clipped_high: default_max_clipped_high(),
        }
    }
}

fn default_jpeg_quality() -> u8 {
    92
}

fn default_workers() -> usize {
    4
}

fn default_strength() -> f32 {
    1.0
}

fn default_develop_backend() -> String {
    "auto".to_string()
}

fn default_max_clipped_high() -> f32 {
    0.02
}

fn default_pixel_format() -> String {
    "8bit".to_string()
}

fn default_keep_metadata() -> bool {
    true
}

fn default_identity_enabled() -> bool {
    true
}

fn default_identity_model() -> String {
    "models/arcfaceresnet100-11-int8.onnx".to_string()
}

fn default_identity_threshold() -> f32 {
    0.35
}

fn default_gallery_path() -> String {
    "identities.json".to_string()
}

fn default_landmark_model() -> String {
    "models/2d106det.onnx".to_string()
}

fn default_scene_tags_enabled() -> bool {
    true
}

fn default_scene_tag_model() -> String {
    "models/mobilenetv2-1.0.onnx".to_string()
}

fn default_scene_tag_labels() -> String {
    "models/imagenet_classes.txt".to_string()
}

fn default_scene_tag_min_confidence() -> f32 {
    0.15
}

fn default_places_model() -> String {
    "models/resnet18_places365.onnx".to_string()
}

fn default_places_classes() -> String {
    "models/places365_classes.txt".to_string()
}

fn default_places_min_confidence() -> f32 {
    0.25
}

fn default_update_gallery() -> bool {
    true
}

fn default_max_extra() -> usize {
    4
}

fn default_update_min_sim() -> f32 {
    0.5
}

fn default_classifier() -> String {
    "face".to_string()
}

fn default_model_path() -> String {
    "models/face_detection_yunet_2023mar.onnx".to_string()
}

fn default_portrait_min_face_ratio() -> f32 {
    0.02
}

fn default_score_threshold() -> f32 {
    0.6
}

fn default_nms_threshold() -> f32 {
    0.3
}

impl Default for Config {
    fn default() -> Self {
        Self {
            input_dir: String::new(),
            output_dir: String::new(),
            jpeg_quality: default_jpeg_quality(),
            workers: default_workers(),
            portrait: SceneConfig::default(),
            other: SceneConfig::default(),
            scene_settings: SceneSettings::default(),
            safety: SafetyConfig::default(),
            develop: DevelopSettings::default(),
            output: OutputSettings::default(),
            identity: IdentitySettings::default(),
            scene_tags: SceneTagSettings::default(),
        }
    }
}

impl Config {
    /// 读取配置文件，并用命令行提供的输入/输出目录覆盖后再校验。
    /// 这样配置文件可以省略 `input_dir` / `output_dir`。
    pub fn load_with_paths(path: &Path, input_dir: &str, output_dir: &str) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| Error::Config(format!("failed to read {}: {e}", path.display())))?;
        Self::from_toml_with_paths(&text, input_dir, output_dir)
    }

    /// 从 TOML 文本解析并校验（配置文件需自带路径，仅测试使用）。
    #[cfg(test)]
    pub fn from_toml(text: &str) -> Result<Self> {
        let config: Config = toml::from_str(text)
            .map_err(|e| Error::Config(format!("failed to parse config: {e}")))?;
        config.validate()?;
        Ok(config)
    }

    /// 解析 TOML 文本，用命令行路径覆盖后校验；其余字段取默认值。
    pub fn from_toml_with_paths(text: &str, input_dir: &str, output_dir: &str) -> Result<Self> {
        let mut config: Config = toml::from_str(text)
            .map_err(|e| Error::Config(format!("failed to parse config: {e}")))?;
        config.input_dir = input_dir.to_string();
        config.output_dir = output_dir.to_string();
        config.validate()?;
        Ok(config)
    }

    /// 读取配置文件并保留其中的输入/输出目录（图形界面加载参数用）。
    /// 文件不存在时返回 Err。
    pub fn load_file(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| Error::Config(format!("failed to read {}: {e}", path.display())))?;
        let config: Config = toml::from_str(&text)
            .map_err(|e| Error::Config(format!("failed to parse config: {e}")))?;
        Ok(config)
    }

    /// 序列化为 TOML 文本（保存参数用），并做一次校验避免写出非法配置。
    pub fn to_toml(&self) -> Result<String> {
        self.validate()?;
        toml::to_string_pretty(self).map_err(|e| Error::Config(format!("failed to serialize config: {e}")))
    }

    /// 写出配置文件。
    pub fn save_file(&self, path: &Path) -> Result<()> {
        let text = self.to_toml()?;
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        std::fs::write(path, text)?;
        Ok(())
    }

    /// 校验取值范围。
    pub fn validate(&self) -> Result<()> {
        if self.input_dir.trim().is_empty() {
            return Err(Error::Config("input_dir must not be empty".to_string()));
        }
        if self.output_dir.trim().is_empty() {
            return Err(Error::Config("output_dir must not be empty".to_string()));
        }
        if !(1..=100).contains(&self.jpeg_quality) {
            return Err(Error::Config(format!(
                "jpeg_quality must be 1..=100, got {}",
                self.jpeg_quality
            )));
        }
        if self.workers == 0 {
            return Err(Error::Config("workers must be >= 1".to_string()));
        }
        if !self.safety.max_clipped_high.is_finite() || self.safety.max_clipped_high < 0.0 {
            return Err(Error::Config(format!(
                "safety.max_clipped_high must be >= 0, got {}",
                self.safety.max_clipped_high
            )));
        }
        for (name, scene) in [("portrait", &self.portrait), ("other", &self.other)] {
            if let Some(lut) = &scene.lut {
                if !lut.strength.is_finite() || !(0.0..=1.0).contains(&lut.strength) {
                    return Err(Error::Config(format!(
                        "{name}.lut.strength must be 0.0..=1.0, got {}",
                        lut.strength
                    )));
                }
                if lut.file.trim().is_empty() {
                    return Err(Error::Config(format!("{name}.lut.file must not be empty")));
                }
            }
        }
        if self.identity.model.trim().is_empty() {
            return Err(Error::Config("identity.model must not be empty".to_string()));
        }
        if self.identity.gallery.trim().is_empty() {
            return Err(Error::Config("identity.gallery must not be empty".to_string()));
        }
        if !self.identity.threshold.is_finite() || !(0.0..=1.0).contains(&self.identity.threshold)
        {
            return Err(Error::Config(format!(
                "identity.threshold must be 0.0..=1.0, got {}",
                self.identity.threshold
            )));
        }
        if !self.identity.update_min_sim.is_finite()
            || !(0.0..=1.0).contains(&self.identity.update_min_sim)
        {
            return Err(Error::Config(format!(
                "identity.update_min_sim must be 0.0..=1.0, got {}",
                self.identity.update_min_sim
            )));
        }
        if self.identity.max_extra > 16 {
            return Err(Error::Config(
                "identity.max_extra must be <= 16".to_string(),
            ));
        }
        match self.output.pixel_format.as_str() {
            "8bit" | "16bit" => {}
            other => {
                return Err(Error::Config(format!(
                    "output.pixel_format must be '8bit' or '16bit', got '{other}'"
                )));
            }
        }
        match self.develop.backend.as_str() {
            "auto" | "darktable" | "builtin" => {}
            other => {
                return Err(Error::Config(format!(
                    "develop.backend must be 'auto', 'darktable' or 'builtin', got '{other}'"
                )));
            }
        }
        match self.scene_settings.classifier.as_str() {
            "face" | "stub" => {}
            other => {
                return Err(Error::Config(format!(
                    "scene.classifier must be 'face' or 'stub', got '{other}'"
                )));
            }
        }
        if self.scene_settings.model.trim().is_empty() {
            return Err(Error::Config(
                "scene.model must not be empty".to_string(),
            ));
        }
        if !(0.0..=1.0).contains(&self.scene_tags.min_confidence) {
            return Err(Error::Config(format!(
                "scene_tags.min_confidence must be 0.0..=1.0, got {}",
                self.scene_tags.min_confidence
            )));
        }
        if !(0.0..=1.0).contains(&self.scene_tags.places_min_confidence) {
            return Err(Error::Config(format!(
                "scene_tags.places_min_confidence must be 0.0..=1.0, got {}",
                self.scene_tags.places_min_confidence
            )));
        }
        for (name, value) in [
            (
                "scene.portrait_min_face_ratio",
                self.scene_settings.portrait_min_face_ratio,
            ),
            ("scene.score_threshold", self.scene_settings.score_threshold),
            ("scene.nms_threshold", self.scene_settings.nms_threshold),
        ] {
            if !value.is_finite() || !(0.0..=1.0).contains(&value) {
                return Err(Error::Config(format!(
                    "{name} must be 0.0..=1.0, got {value}"
                )));
            }
        }
        Ok(())
    }

    /// 取某个场景的配置。
    pub fn scene(&self, kind: SceneKind) -> &SceneConfig {
        match kind {
            SceneKind::Portrait => &self.portrait,
            SceneKind::Other => &self.other,
        }
    }

    /// 是否以 16-bit TIFF 导出。
    pub fn is_16bit_output(&self) -> bool {
        self.output.pixel_format == "16bit"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXAMPLE: &str = include_str!("../config.example.toml");

    #[test]
    fn example_config_parses_and_validates() {
        let config = Config::from_toml(EXAMPLE).expect("example config should parse");
        assert_eq!(config.portrait.stages, vec!["lut".to_string()]);
        assert_eq!(config.other.stages, vec!["lut".to_string()]);
        assert!(config.safety.max_clipped_high > 0.0);
        assert!(config.workers >= 1);
    }

    #[test]
    fn config_toml_roundtrip_preserves_all_settings() {
        let config = Config::from_toml(EXAMPLE).expect("parse");
        let text = config.to_toml().expect("serialize");
        let again = Config::from_toml(&text).expect("re-parse");

        assert_eq!(again.input_dir, config.input_dir);
        assert_eq!(again.output_dir, config.output_dir);
        assert_eq!(again.jpeg_quality, config.jpeg_quality);
        assert_eq!(again.workers, config.workers);
        assert_eq!(again.develop.backend, config.develop.backend);
        assert_eq!(again.scene_settings.classifier, config.scene_settings.classifier);
        assert_eq!(again.scene_settings.model, config.scene_settings.model);
        assert_eq!(again.output.pixel_format, config.output.pixel_format);
        assert_eq!(again.output.keep_metadata, config.output.keep_metadata);
        assert_eq!(again.identity.model, config.identity.model);
        assert_eq!(again.identity.landmark_model, config.identity.landmark_model);
        assert_eq!(again.identity.max_extra, config.identity.max_extra);
        assert_eq!(again.identity.update_min_sim, config.identity.update_min_sim);
        assert_eq!(again.portrait.lut.as_ref().map(|l| l.file.clone()),
                   config.portrait.lut.as_ref().map(|l| l.file.clone()));
        assert_eq!(again.other.lut.as_ref().map(|l| l.strength),
                   config.other.lut.as_ref().map(|l| l.strength));
        // `[scene]` 重命名对序列化同样生效。
        assert!(text.contains("[scene]"));
    }

    #[test]
    fn saved_config_can_be_loaded_back() {
        let dir = std::env::temp_dir().join(format!("sepiola-cfg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create dir");
        let path = dir.join("nested/params.toml");
        let config = Config::from_toml(EXAMPLE).expect("parse");
        config.save_file(&path).expect("save");
        let loaded = Config::load_file(&path).expect("load");
        assert_eq!(loaded.workers, config.workers);
        assert_eq!(loaded.identity.threshold, config.identity.threshold);
        // 非法配置不写出。
        let broken = Config {
            input_dir: String::new(),
            ..Config::default()
        };
        assert!(broken.save_file(&dir.join("bad.toml")).is_err());
        assert!(!dir.join("bad.toml").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn scene_lookup_uses_kind() {
        let config = Config::from_toml(EXAMPLE).expect("parse");
        assert_eq!(
            config.scene(SceneKind::Portrait).lut.as_ref().map(|l| l.strength),
            Some(0.4)
        );
        assert_eq!(
            config.scene(SceneKind::Other).lut.as_ref().map(|l| l.strength),
            Some(0.7)
        );
    }

    #[test]
    fn invalid_quality_is_rejected() {
        let text = r#"
input_dir = "in"
output_dir = "out"
jpeg_quality = 0
[portrait]
stages = []
[other]
stages = []
"#;
        assert!(Config::from_toml(text).is_err());
    }

    #[test]
    fn invalid_lut_strength_is_rejected() {
        let text = r#"
input_dir = "in"
output_dir = "out"
[portrait]
stages = ["lut"]
[portrait.lut]
file = "a.cube"
strength = 1.5
[other]
stages = []
"#;
        assert!(Config::from_toml(text).is_err());
    }

    #[test]
    fn missing_file_is_config_error() {
        let path = std::env::temp_dir().join("sepiola-does-not-exist.toml");
        let _ = std::fs::remove_file(&path);
        assert!(Config::load_with_paths(&path, "in", "out").is_err());
    }

    #[test]
    fn default_config_is_valid_with_cli_paths() {
        let config = Config {
            input_dir: "in".to_string(),
            output_dir: "out".to_string(),
            ..Config::default()
        };
        config.validate().expect("default config should be valid");
        assert_eq!(config.jpeg_quality, 92);
        assert_eq!(config.workers, 4);
        assert!(config.portrait.stages.is_empty());
        assert_eq!(config.scene_settings.classifier, "face");
    }

    #[test]
    fn output_defaults_and_validation() {
        let config = Config::from_toml_with_paths("", "in", "out").expect("empty toml");
        assert_eq!(config.output.pixel_format, "8bit");
        assert!(config.output.keep_metadata);
        assert!(!config.is_16bit_output());

        let config =
            Config::from_toml_with_paths("[output]\npixel_format = \"16bit\"\n", "in", "out")
                .expect("16bit config");
        assert!(config.is_16bit_output());

        assert!(
            Config::from_toml_with_paths("[output]\npixel_format = \"32bit\"\n", "in", "out")
                .is_err()
        );
    }

    #[test]
    fn partial_config_uses_defaults_and_cli_paths() {
        let config = Config::from_toml_with_paths("[scene]\nclassifier = \"stub\"\n", "in", "out")
            .expect("partial config should parse");
        assert_eq!(config.input_dir, "in");
        assert_eq!(config.output_dir, "out");
        assert_eq!(config.scene_settings.classifier, "stub");
        assert_eq!(config.workers, 4);
        assert!(config.other.stages.is_empty());
    }
}
