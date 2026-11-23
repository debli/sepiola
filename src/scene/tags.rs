//! 基于 ONNX Runtime 的 ImageNet 场景标签（MobileNetV2），用于媒体库相册细分。
//!
//! 模型：ONNX Model Zoo `mobilenetv2-1.0.onnx`（Apache-2.0），输出 1000 类 logits。
//! 预处理与 torchvision 一致（未烘焙进图，实测验证）：短边缩放到 256 →
//! center crop 224 → RGB /255 → ImageNet mean/std 归一化 → NCHW。

use std::sync::Mutex;

use ort::session::Session;
use ort::value::Tensor;

use crate::config::SceneTagSettings;
use crate::error::{Error, Result};
use crate::imageio::{ImageF32, CHANNELS};
use crate::library::{ALBUM_ARCHITECTURE, ALBUM_BIRTHDAY, ALBUM_PARTY, ALBUM_PLANTS_ANIMALS};
use crate::scene::face::ort_environment;

/// torchvision 标准输入尺寸与归一化参数。
const SHORT_SIDE: usize = 256;
const CROP_SIDE: usize = 224;
const MEAN: [f32; 3] = [0.485, 0.456, 0.406];
const STD: [f32; 3] = [0.229, 0.224, 0.225];

/// ImageNet-1k 中 0..=397 为动物/自然物，398 起为人工物。
const NATURAL_MAX_INDEX: usize = 397;

/// 真菌类散落在人工物区段，单独列入"植物动物"。
const FUNGI_CLASSES: [&str; 7] = [
    "mushroom",
    "agaric",
    "gyromitra",
    "stinkhorn",
    "earthstar",
    "hen-of-the-woods",
    "bolete",
];

/// 近似映射到"生日"相册的类（ImageNet 无 birthday cake 类）。
const BIRTHDAY_CLASSES: [&str; 2] = ["balloon", "candle"];

/// 近似映射到"聚会"相册的类（聚餐/酒吧/餐厅场景）。
const PARTY_CLASSES: [&str; 5] = [
    "restaurant",
    "dining table",
    "wine bottle",
    "beer glass",
    "beer bottle",
];

/// 食物特写类，归入"美食"相册（M34 从聚会拆出）。
const FOOD_CLASSES: [&str; 7] = [
    "pizza",
    "cheeseburger",
    "hotdog",
    "bagel",
    "ice cream",
    "banana",
    "burrito",
];

/// 屏幕/电脑外设类：截图、PPT 照片等归入"截图/文档"（M34，修复误入风景）。
const SCREEN_DOC_CLASSES: [&str; 4] = ["screen", "desktop computer", "notebook", "computer keyboard"];

/// ImageNet 中高置信度建筑类，归入"建筑"相册。
///
/// Places365 对室外建筑照片常常给出偏低的 top-1 置信度（如大楼照片被判
/// `hotel/outdoor` 0.15），而 ImageNet 的教堂/城堡/桥/灯塔等类反而很确定。
const ARCHITECTURE_CLASSES: [&str; 15] = [
    "monastery",
    "castle",
    "palace",
    "church",
    "dome",
    "triumphal arch",
    "viaduct",
    "steel arch bridge",
    "suspension bridge",
    "barn",
    "boathouse",
    "lighthouse",
    "dam",
    "water tower",
    "mosque",
];

/// top-1 标签结果。
#[derive(Clone, Debug, PartialEq)]
pub struct SceneTag {
    /// ImageNet 类下标（与类名表行号一致）。
    pub index: usize,
    /// 类名。
    pub class: String,
    /// softmax 概率。
    pub confidence: f32,
}

/// ImageNet 场景标签器。
pub struct SceneTagger {
    session: Mutex<Session>,
    labels: Vec<String>,
}

impl SceneTagger {
    /// 创建标签器并加载 ONNX 模型与类名表。
    pub fn new(settings: &SceneTagSettings) -> Result<Self> {
        let model = crate::paths::resolve_asset(&settings.model);
        if !model.is_file() {
            return Err(Error::Model(format!(
                "scene tag model not found: {}",
                model.display()
            )));
        }
        let labels_path = crate::paths::resolve_asset(&settings.labels);
        let labels = load_labels(&labels_path)?;
        let environment = ort_environment()?;
        tracing::info!(model = %model.display(), classes = labels.len(), "loading scene tag model");
        let session = Session::builder(environment)
            .and_then(|mut builder| builder.commit_from_file(&model))
            .map_err(|e| Error::Model(format!("failed to load {}: {e}", model.display())))?;
        Ok(Self {
            session: Mutex::new(session),
            labels,
        })
    }

    /// 推理 top-1 标签。
    pub fn tag(&self, img: &ImageF32) -> Result<SceneTag> {
        let input = preprocess(img);
        let tensor = Tensor::from_array((
            [1usize, CHANNELS, CROP_SIDE, CROP_SIDE],
            input.into_boxed_slice(),
        ))
        .map_err(|e| Error::Model(format!("failed to build input tensor: {e}")))?;

        let mut session = self
            .session
            .lock()
            .map_err(|_| Error::Model("scene tagger mutex poisoned".to_string()))?;
        let outputs = session
            .run(ort::inputs![tensor])
            .map_err(|e| Error::Model(format!("scene tag inference failed: {e}")))?;
        let value = &outputs[0];
        let (_, logits) = value
            .try_extract_tensor::<f32>()
            .map_err(|e| Error::Model(format!("scene tag output: {e}")))?;
        let (index, confidence) = softmax_top1(&logits);
        let class = self
            .labels
            .get(index)
            .cloned()
            .unwrap_or_else(|| format!("class_{index}"));
        Ok(SceneTag {
            index,
            class,
            confidence,
        })
    }
}

/// 读取类名表：每行一个类，索引即模型输出下标。
fn load_labels(path: &std::path::Path) -> Result<Vec<String>> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| Error::Model(format!("failed to read {}: {e}", path.display())))?;
    let labels: Vec<String> = text.lines().map(|line| line.trim().to_string()).collect();
    if labels.len() < 2 {
        return Err(Error::Model(format!(
            "scene tag labels too short: {}",
            path.display()
        )));
    }
    Ok(labels)
}

/// 数值稳定的 softmax top-1（返回下标与概率）。
pub fn softmax_top1(logits: &[f32]) -> (usize, f32) {
    let max = logits.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
    let mut sum = 0.0f32;
    let mut best_index = 0usize;
    let mut best_exp = 0.0f32;
    for (i, logit) in logits.iter().enumerate() {
        let e = (*logit - max).exp();
        sum += e;
        if e > best_exp {
            best_exp = e;
            best_index = i;
        }
    }
    let confidence = if sum > 0.0 { best_exp / sum } else { 0.0 };
    (best_index, confidence)
}

/// ImageNet 类 → 细分相册名；None 表示不细分（由调用方继续尝试 Places 或兜底"未分类"）。
pub fn album_for_class(index: usize, class: &str) -> Option<&'static str> {
    if index <= NATURAL_MAX_INDEX || FUNGI_CLASSES.contains(&class) {
        return Some(ALBUM_PLANTS_ANIMALS);
    }
    if BIRTHDAY_CLASSES.contains(&class) {
        return Some(ALBUM_BIRTHDAY);
    }
    if PARTY_CLASSES.contains(&class) {
        return Some(ALBUM_PARTY);
    }
    if FOOD_CLASSES.contains(&class) {
        return Some(crate::library::ALBUM_FOOD);
    }
    if SCREEN_DOC_CLASSES.contains(&class) {
        return Some(crate::library::ALBUM_SCREEN_DOC);
    }
    if ARCHITECTURE_CLASSES.contains(&class) {
        return Some(ALBUM_ARCHITECTURE);
    }
    None
}

/// 短边缩放到 256 → center crop 224 → 归一化 NCHW RGB。
fn preprocess(img: &ImageF32) -> Vec<f32> {
    let resized = resize_short_side(img, SHORT_SIDE);
    let cropped = center_crop(&resized, CROP_SIDE);
    let plane = CROP_SIDE * CROP_SIDE;
    let mut data = vec![0.0f32; plane * CHANNELS];
    for y in 0..CROP_SIDE {
        for x in 0..CROP_SIDE {
            let src = (y * CROP_SIDE + x) * CHANNELS;
            let dst = y * CROP_SIDE + x;
            for c in 0..CHANNELS {
                let value = cropped.data[src + c].clamp(0.0, 1.0);
                data[c * plane + dst] = (value - MEAN[c]) / STD[c];
            }
        }
    }
    data
}

/// 双线性缩放使短边等于 `side`（放大或缩小）。
pub(crate) fn resize_short_side(img: &ImageF32, side: usize) -> ImageF32 {
    let shortest = (img.width.min(img.height) as usize).max(1);
    if shortest == side {
        return img.clone();
    }
    let scale = side as f32 / shortest as f32;
    let new_w = ((img.width as f32 * scale).round() as usize).max(1);
    let new_h = ((img.height as f32 * scale).round() as usize).max(1);
    let mut out = vec![0.0f32; new_w * new_h * CHANNELS];
    let x_ratio = img.width as f32 / new_w as f32;
    let y_ratio = img.height as f32 / new_h as f32;
    for y in 0..new_h {
        let src_y = (y as f32 + 0.5) * y_ratio - 0.5;
        let y0 = (src_y.floor().max(0.0)) as usize;
        let y0 = y0.min(img.height as usize - 1);
        let y1 = (y0 + 1).min(img.height as usize - 1);
        let wy = (src_y - y0 as f32).clamp(0.0, 1.0);
        for x in 0..new_w {
            let src_x = (x as f32 + 0.5) * x_ratio - 0.5;
            let x0 = (src_x.floor().max(0.0)) as usize;
            let x0 = x0.min(img.width as usize - 1);
            let x1 = (x0 + 1).min(img.width as usize - 1);
            let wx = (src_x - x0 as f32).clamp(0.0, 1.0);
            let idx = |px: usize, py: usize| (py * img.width as usize + px) * CHANNELS;
            let dst = (y * new_w + x) * CHANNELS;
            for c in 0..CHANNELS {
                let top = img.data[idx(x0, y0) + c] * (1.0 - wx) + img.data[idx(x1, y0) + c] * wx;
                let bottom =
                    img.data[idx(x0, y1) + c] * (1.0 - wx) + img.data[idx(x1, y1) + c] * wx;
                out[dst + c] = top * (1.0 - wy) + bottom * wy;
            }
        }
    }
    ImageF32 {
        width: new_w as u32,
        height: new_h as u32,
        data: out,
    }
}

/// 居中裁剪 `side`×`side`（短边已 ≥ side）。
pub(crate) fn center_crop(img: &ImageF32, side: usize) -> ImageF32 {
    let w = img.width as usize;
    let h = img.height as usize;
    let offset_x = w.saturating_sub(side) / 2;
    let offset_y = h.saturating_sub(side) / 2;
    let mut out = vec![0.0f32; side * side * CHANNELS];
    for y in 0..side {
        for x in 0..side {
            let src_x = (x + offset_x).min(w - 1);
            let src_y = (y + offset_y).min(h - 1);
            let src = (src_y * w + src_x) * CHANNELS;
            let dst = (y * side + x) * CHANNELS;
            out[dst..dst + CHANNELS].copy_from_slice(&img.data[src..src + CHANNELS]);
        }
    }
    ImageF32 {
        width: side as u32,
        height: side as u32,
        data: out,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn album_mapping_covers_animals_fungi_birthday_party() {
        assert_eq!(
            album_for_class(0, "tench"),
            Some(ALBUM_PLANTS_ANIMALS)
        );
        assert_eq!(
            album_for_class(207, "golden retriever"),
            Some(ALBUM_PLANTS_ANIMALS)
        );
        assert_eq!(
            album_for_class(397, "puffer"),
            Some(ALBUM_PLANTS_ANIMALS)
        );
        assert_eq!(
            album_for_class(992, "agaric"),
            Some(ALBUM_PLANTS_ANIMALS)
        );
        assert_eq!(album_for_class(417, "balloon"), Some(ALBUM_BIRTHDAY));
        assert_eq!(album_for_class(470, "candle"), Some(ALBUM_BIRTHDAY));
        assert_eq!(
            album_for_class(762, "restaurant"),
            Some(ALBUM_PARTY)
        );
        assert_eq!(
            album_for_class(532, "dining table"),
            Some(ALBUM_PARTY)
        );
        assert_eq!(album_for_class(403, "aircraft carrier"), None);
        assert_eq!(album_for_class(947, "mushroom"), Some(ALBUM_PLANTS_ANIMALS));
        // ImageNet 建筑类应归入"建筑"，避免 Places 低置信度漏检。
        assert_eq!(album_for_class(663, "monastery"), Some(ALBUM_ARCHITECTURE));
        assert_eq!(album_for_class(888, "viaduct"), Some(ALBUM_ARCHITECTURE));
        assert_eq!(
            album_for_class(483, "castle"),
            Some(ALBUM_ARCHITECTURE)
        );
    }

    #[test]
    fn softmax_top1_is_stable_and_normalized() {
        let logits = [1.0f32, 5.0, 2.0, -3.0];
        let (index, confidence) = softmax_top1(&logits);
        assert_eq!(index, 1);
        assert!(confidence > 0.8 && confidence < 1.0);
        // 极端量级不溢出。
        let (index, confidence) = softmax_top1(&[1000.0, 0.0, 0.0]);
        assert_eq!(index, 0);
        assert!((confidence - 1.0).abs() < 1e-6);
    }

    #[test]
    fn preprocess_normalizes_constant_gray() {
        let img = ImageF32 {
            width: 300,
            height: 200,
            data: vec![0.5f32; 300 * 200 * CHANNELS],
        };
        let input = preprocess(&img);
        assert_eq!(input.len(), CHANNELS * CROP_SIDE * CROP_SIDE);
        let plane = CROP_SIDE * CROP_SIDE;
        for c in 0..CHANNELS {
            let expected = (0.5 - MEAN[c]) / STD[c];
            assert!(
                (input[c * plane] - expected).abs() < 1e-5,
                "channel {c}: {} vs {expected}",
                input[c * plane]
            );
        }
    }

    #[test]
    fn resize_and_crop_shapes() {
        let img = ImageF32 {
            width: 640,
            height: 480,
            data: vec![0.25f32; 640 * 480 * CHANNELS],
        };
        let resized = resize_short_side(&img, SHORT_SIDE);
        assert_eq!(resized.height as usize, SHORT_SIDE);
        assert_eq!(resized.width as usize, (640.0f32 * 256.0 / 480.0).round() as usize);
        let crop = center_crop(&resized, CROP_SIDE);
        assert_eq!((crop.width, crop.height), (CROP_SIDE as u32, CROP_SIDE as u32));
    }

    /// 真实模型回归：样本图 top-1 应映射到预期相册（模型缺失时跳过）。
    #[test]
    fn real_model_tags_imagenet_samples() {
        let settings = SceneTagSettings::default();
        let Ok(tagger) = SceneTagger::new(&settings) else {
            return; // 模型未就位时跳过
        };
        let cases = [
            ("n02099601_golden_retriever.JPEG", ALBUM_PLANTS_ANIMALS),
            ("n02123394_Persian_cat.JPEG", ALBUM_PLANTS_ANIMALS),
            ("n02317335_starfish.JPEG", ALBUM_PLANTS_ANIMALS),
            ("n02782093_balloon.JPEG", ALBUM_BIRTHDAY),
            ("n03201208_dining_table.JPEG", ALBUM_PARTY),
            ("n04081281_restaurant.JPEG", ALBUM_PARTY),
        ];
        for (name, expected_album) in cases {
            let path = std::path::Path::new("test_data/scene").join(name);
            let img = crate::imageio::load(&path).expect("sample image");
            let tag = tagger.tag(&img).expect("tag");
            let album = album_for_class(tag.index, &tag.class);
            println!("{name} → top1 {} {:.3} → {album:?}", tag.class, tag.confidence);
            assert_eq!(album, Some(expected_album), "album for {name}");
        }
    }
}
