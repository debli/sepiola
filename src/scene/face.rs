//! 基于 ONNX Runtime 与 YuNet 的真实人脸检测。
//!
//! 模型：`face_detection_yunet_2023mar.onnx`（OpenCV Zoo，Apache-2.0）。
//! 预处理与解码方式与 OpenCV `FaceDetectorYN` 保持一致：
//! BGR、0..255、NCHW，右/下补零到 32 的倍数。

use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use ort::environment::Environment;
use ort::session::Session;
use ort::value::Tensor;

use crate::config::SceneSettings;
use crate::error::{Error, Result};
use crate::imageio::{ImageF32, CHANNELS};
use crate::scene::{FaceBox, SceneClassifier, SceneInfo, SceneKind};

/// YuNet 模型要求的固定输入边长；检测前也把长边缩到这个尺寸。
const INPUT_SIDE: u32 = 640;
const STRIDES: [u32; 3] = [8, 16, 32];
const TOP_K: usize = 100;

/// YuNet 人脸检测分类器。
pub struct FaceClassifier {
    session: Mutex<Session>,
    score_threshold: f32,
    nms_threshold: f32,
    portrait_min_face_ratio: f32,
}

impl FaceClassifier {
    /// 创建分类器并加载 ONNX 模型。
    pub fn new(settings: &SceneSettings) -> Result<Self> {
        let model = crate::paths::resolve_asset(&settings.model);
        if !model.is_file() {
            return Err(Error::Model(format!(
                "face detector model not found: {}",
                model.display()
            )));
        }
        let environment = ort_environment()?;
        tracing::info!(model = %model.display(), "loading face detector model");
        let session = Session::builder(environment)
            .and_then(|mut builder| builder.commit_from_file(&model))
            .map_err(|e| Error::Model(format!("failed to load {}: {e}", model.display())))?;

        Ok(Self {
            session: Mutex::new(session),
            score_threshold: settings.score_threshold,
            nms_threshold: settings.nms_threshold,
            portrait_min_face_ratio: settings.portrait_min_face_ratio,
        })
    }

    /// 检测结果转成场景信息。
    fn into_scene_info(&self, faces: Vec<FaceBox>) -> SceneInfo {
        let info = SceneInfo {
            kind: SceneKind::Other,
            faces,
        };
        let kind = if info.largest_face_ratio() >= self.portrait_min_face_ratio {
            SceneKind::Portrait
        } else {
            SceneKind::Other
        };
        SceneInfo { kind, ..info }
    }
}

impl SceneClassifier for FaceClassifier {
    fn classify(&self, img: &ImageF32) -> Result<SceneInfo> {
        let (input, content_w, content_h) = preprocess(img);
        let tensor = Tensor::from_array((
            [1usize, 3, INPUT_SIDE as usize, INPUT_SIDE as usize],
            input.into_boxed_slice(),
        ))
        .map_err(|e| Error::Model(format!("failed to build input tensor: {e}")))?;

        let mut session = self
            .session
            .lock()
            .map_err(|_| Error::Model("face detector mutex poisoned".to_string()))?;
        let outputs = session
            .run(ort::inputs![tensor])
            .map_err(|e| Error::Model(format!("face detection inference failed: {e}")))?;

        let mut candidates: Vec<(FaceBox, f32)> = Vec::new();
        for stride in STRIDES.iter() {
            let suffix = stride.to_string();
            let cls = extract(&outputs, &format!("cls_{suffix}"))?;
            let obj = extract(&outputs, &format!("obj_{suffix}"))?;
            let bbox = extract(&outputs, &format!("bbox_{suffix}"))?;
            let rows = INPUT_SIDE / stride;
            let cols = INPUT_SIDE / stride;
            let decoded = decode_stride(
                cls,
                obj,
                bbox,
                rows as usize,
                cols as usize,
                *stride as f32,
                content_w as f32,
                content_h as f32,
                self.score_threshold,
            );
            tracing::debug!(stride, faces = decoded.len(), "yunet stride decoded");
            candidates.extend(decoded);
        }

        let faces = nms(candidates, self.nms_threshold, TOP_K);
        Ok(self.into_scene_info(faces))
    }
}

/// 全局 ONNX Runtime 环境：人脸检测与身份识别共享，避免重复初始化。
pub(crate) fn ort_environment() -> Result<&'static Environment> {
    static ENV: OnceLock<Environment> = OnceLock::new();
    static INIT_LOCK: Mutex<()> = Mutex::new(());
    if let Some(environment) = ENV.get() {
        return Ok(environment);
    }
    let _guard = INIT_LOCK
        .lock()
        .map_err(|_| Error::Model("onnxruntime init mutex poisoned".to_string()))?;
    if let Some(environment) = ENV.get() {
        return Ok(environment);
    }
    let library = runtime_library().ok_or_else(|| {
        Error::Model(
            "onnxruntime library not found; set SEPIOLA_ONNXRUNTIME, or place \
             libonnxruntime.so in <exe_dir>/lib/ or <exe_dir>/, or install it under \
             /usr/lib/x86_64-linux-gnu/"
                .to_string(),
        )
    })?;
    tracing::info!(library = %library.display(), "loading onnxruntime");
    let environment = ort::init_from(&library)
        .map_err(|e| {
            Error::Model(format!(
                "failed to load onnxruntime from {}: {e}",
                library.display()
            ))
        })?
        .with_name("sepiola")
        .build()
        .map_err(|e| Error::Model(format!("failed to init onnxruntime: {e}")))?;
    Ok(ENV.get_or_init(|| environment))
}

/// 查找 ONNX Runtime 动态库：环境变量 → 可执行文件旁的 `lib/` → 可执行文件同目录
/// → 常见系统路径。随包分发时把官方自包含的库放在 `lib/` 即可。
pub(crate) fn runtime_library() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("SEPIOLA_ONNXRUNTIME") {
        return Some(PathBuf::from(path));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let packaged = find_library_in_dir(&dir.join("lib")).or_else(|| find_library_in_dir(dir));
            if let Some(library) = packaged {
                return Some(library);
            }
        }
    }
    const CANDIDATES: [&str; 5] = [
        "/usr/lib/x86_64-linux-gnu/libonnxruntime.so.1.23",
        "/usr/lib/x86_64-linux-gnu/libonnxruntime.so",
        "/usr/local/lib/libonnxruntime.so",
        "/usr/lib/libonnxruntime.so",
        "/opt/homebrew/lib/libonnxruntime.dylib",
    ];
    CANDIDATES
        .iter()
        .map(PathBuf::from)
        .find(|path| path.is_file())
}

/// 在目录中查找 `libonnxruntime.so*`，优先无版本后缀的文件。
fn find_library_in_dir(dir: &Path) -> Option<PathBuf> {
    let mut found: Vec<PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.is_file()
                && path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .map(|name| name.starts_with("libonnxruntime.so"))
                    .unwrap_or(false)
        })
        .collect();
    found.sort();
    found.into_iter().next()
}

/// 缩放使长边不超过 `INPUT_SIDE`，再顶左对齐补零到 `INPUT_SIDE`×`INPUT_SIDE`，
/// 输出 NCHW BGR 0..255 与有效内容尺寸。
fn preprocess(img: &ImageF32) -> (Vec<f32>, u32, u32) {
    let (resized, _) = resize_for_detection(img, INPUT_SIDE);
    let side = INPUT_SIDE as usize;
    let plane = side * side;
    let mut data = vec![0.0f32; plane * CHANNELS];

    for y in 0..resized.height as usize {
        for x in 0..resized.width as usize {
            let src = (y * resized.width as usize + x) * CHANNELS;
            let dst = y * side + x;
            // BGR 顺序。
            data[dst] = resized.data[src + 2] * 255.0;
            data[plane + dst] = resized.data[src + 1] * 255.0;
            data[2 * plane + dst] = resized.data[src] * 255.0;
        }
    }
    (data, resized.width, resized.height)
}

/// 双线性缩放；长边已不超过 `max_side` 时直接返回克隆。
fn resize_for_detection(img: &ImageF32, max_side: u32) -> (ImageF32, f32) {
    let longest = img.width.max(img.height);
    if longest <= max_side {
        return (img.clone(), 1.0);
    }
    let scale = max_side as f32 / longest as f32;
    let new_w = ((img.width as f32 * scale).round() as u32).max(1);
    let new_h = ((img.height as f32 * scale).round() as u32).max(1);
    let mut out = vec![0.0f32; new_w as usize * new_h as usize * CHANNELS];

    let x_ratio = img.width as f32 / new_w as f32;
    let y_ratio = img.height as f32 / new_h as f32;
    for y in 0..new_h {
        let src_y = (y as f32 + 0.5) * y_ratio - 0.5;
        let y0 = src_y.floor().max(0.0) as u32;
        let y1 = (y0 + 1).min(img.height - 1);
        let wy = (src_y - y0 as f32).clamp(0.0, 1.0);
        for x in 0..new_w {
            let src_x = (x as f32 + 0.5) * x_ratio - 0.5;
            let x0 = src_x.floor().max(0.0) as u32;
            let x1 = (x0 + 1).min(img.width - 1);
            let wx = (src_x - x0 as f32).clamp(0.0, 1.0);

            let idx = |px: u32, py: u32| (py as usize * img.width as usize + px as usize) * CHANNELS;
            let p00 = idx(x0, y0);
            let p10 = idx(x1, y0);
            let p01 = idx(x0, y1);
            let p11 = idx(x1, y1);
            let dst = (y as usize * new_w as usize + x as usize) * CHANNELS;
            for c in 0..CHANNELS {
                let top = img.data[p00 + c] * (1.0 - wx) + img.data[p10 + c] * wx;
                let bottom = img.data[p01 + c] * (1.0 - wx) + img.data[p11 + c] * wx;
                out[dst + c] = top * (1.0 - wy) + bottom * wy;
            }
        }
    }

    (
        ImageF32 {
            width: new_w,
            height: new_h,
            data: out,
        },
        scale,
    )
}

/// 按 OpenCV `FaceDetectorYN` 的方式解码一个 stride 的输出。
#[allow(clippy::too_many_arguments)]
fn decode_stride(
    cls: &[f32],
    obj: &[f32],
    bbox: &[f32],
    rows: usize,
    cols: usize,
    stride: f32,
    img_w: f32,
    img_h: f32,
    score_threshold: f32,
) -> Vec<(FaceBox, f32)> {
    let cells = rows * cols;
    if cls.len() < cells || obj.len() < cells || bbox.len() < cells * 4 {
        return Vec::new();
    }
    let mut faces = Vec::new();
    for r in 0..rows {
        for c in 0..cols {
            let idx = r * cols + c;
            let cls_score = cls[idx].clamp(0.0, 1.0);
            let obj_score = obj[idx].clamp(0.0, 1.0);
            let score = cls_score * obj_score;
            if score < score_threshold {
                continue;
            }
            let bx = bbox[idx * 4];
            let by = bbox[idx * 4 + 1];
            let bw = bbox[idx * 4 + 2].exp();
            let bh = bbox[idx * 4 + 3].exp();
            let cx = (c as f32 + bx) * stride;
            let cy = (r as f32 + by) * stride;
            let w = bw * stride;
            let h = bh * stride;
            let x = (cx - w / 2.0) / img_w;
            let y = (cy - h / 2.0) / img_h;
            faces.push((
                FaceBox {
                    x: x.clamp(0.0, 1.0),
                    y: y.clamp(0.0, 1.0),
                    w: (w / img_w).clamp(0.0, 1.0),
                    h: (h / img_h).clamp(0.0, 1.0),
                },
                score,
            ));
        }
    }
    faces
}

/// 贪心 NMS，返回按分数排序的前 `top_k` 个框。
fn nms(mut candidates: Vec<(FaceBox, f32)>, iou_threshold: f32, top_k: usize) -> Vec<FaceBox> {
    candidates.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    let mut kept: Vec<(FaceBox, f32)> = Vec::new();
    'outer: for (face, score) in candidates {
        for (kept_face, _) in &kept {
            if iou(*kept_face, face) > iou_threshold {
                continue 'outer;
            }
        }
        kept.push((face, score));
        if kept.len() >= top_k {
            break;
        }
    }
    kept.into_iter().map(|(face, _)| face).collect()
}

fn iou(a: FaceBox, b: FaceBox) -> f32 {
    let ax2 = a.x + a.w;
    let ay2 = a.y + a.h;
    let bx2 = b.x + b.w;
    let by2 = b.y + b.h;
    let inter_w = ax2.min(bx2) - a.x.max(b.x);
    let inter_h = ay2.min(by2) - a.y.max(b.y);
    if inter_w <= 0.0 || inter_h <= 0.0 {
        return 0.0;
    }
    let inter = inter_w * inter_h;
    let union = a.w * a.h + b.w * b.h - inter;
    if union <= 0.0 {
        0.0
    } else {
        inter / union
    }
}

fn extract<'a>(outputs: &'a ort::session::SessionOutputs<'_>, name: &str) -> Result<&'a [f32]> {
    let value = outputs
        .get(name)
        .ok_or_else(|| Error::Model(format!("model output '{name}' is missing")))?;
    let (_, data) = value
        .try_extract_tensor::<f32>()
        .map_err(|e| Error::Model(format!("output '{name}': {e}")))?;
    Ok(data)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(width: u32, height: u32, color: [f32; 3]) -> ImageF32 {
        let mut data = Vec::with_capacity(width as usize * height as usize * CHANNELS);
        for _ in 0..width * height {
            data.extend_from_slice(&color);
        }
        ImageF32::from_data(width, height, data).expect("valid image")
    }

    #[test]
    fn preprocess_uses_bgr_and_padding() {
        let img = solid(1, 1, [1.0, 0.0, 0.0]);
        let (data, content_w, content_h) = preprocess(&img);
        assert_eq!((content_w, content_h), (1, 1));
        let plane = INPUT_SIDE as usize * INPUT_SIDE as usize;
        assert_eq!(data.len(), CHANNELS * plane);
        // 红色像素：B=0,G=0,R=255。
        assert_eq!(data[0], 0.0);
        assert_eq!(data[plane], 0.0);
        assert_eq!(data[2 * plane], 255.0);
        // 补零区域。
        assert_eq!(data[1], 0.0);
    }

    #[test]
    fn resize_keeps_aspect_and_caps_long_side() {
        let img = solid(1280, 640, [0.5, 0.5, 0.5]);
        let (resized, scale) = resize_for_detection(&img, 640);
        assert_eq!((resized.width, resized.height), (640, 320));
        assert!((scale - 0.5).abs() < 1e-6);
    }

    #[test]
    fn decode_known_cell() {
        // 2x2 网格，stride 8；第一个 cell 给出 16x16 的框，位于 (0,0)。
        let cls = [1.0, 0.0, 0.0, 0.0];
        let obj = [1.0, 0.0, 0.0, 0.0];
        let mut bbox = [0.0f32; 16];
        bbox[2] = 2.0f32.ln();
        bbox[3] = 2.0f32.ln();
        let faces = decode_stride(&cls, &obj, &bbox, 2, 2, 8.0, 32.0, 32.0, 0.6);
        assert_eq!(faces.len(), 1);
        let (face, score) = faces[0];
        assert!((score - 1.0).abs() < 1e-6);
        assert!((face.x - 0.0).abs() < 1e-6);
        assert!((face.y - 0.0).abs() < 1e-6);
        assert!((face.w - 0.5).abs() < 1e-6);
        assert!((face.h - 0.5).abs() < 1e-6);
    }

    #[test]
    fn decode_respects_score_threshold() {
        let cls = [0.25];
        let obj = [0.25];
        let bbox = [0.0, 0.0, 0.0, 0.0];
        assert!(decode_stride(&cls, &obj, &bbox, 1, 1, 8.0, 32.0, 32.0, 0.6).is_empty());
    }

    #[test]
    fn decode_uses_product_of_class_and_objectness_scores() {
        let cls = [0.5];
        let obj = [0.8];
        let bbox = [0.0, 0.0, 0.0, 0.0];
        assert!(decode_stride(&cls, &obj, &bbox, 1, 1, 8.0, 32.0, 32.0, 0.6).is_empty());
    }

    #[test]
    fn nms_suppresses_overlaps() {
        let a = FaceBox {
            x: 0.0,
            y: 0.0,
            w: 0.2,
            h: 0.2,
        };
        let b = FaceBox {
            x: 0.01,
            y: 0.01,
            w: 0.2,
            h: 0.2,
        };
        let c = FaceBox {
            x: 0.7,
            y: 0.7,
            w: 0.2,
            h: 0.2,
        };
        let kept = nms(vec![(a, 0.9), (b, 0.8), (c, 0.7)], 0.3, 100);
        assert_eq!(kept.len(), 2);
        assert_eq!(kept[0], a);
        assert_eq!(kept[1], c);
    }

    #[test]
    fn portrait_needs_large_enough_face() {
        let settings = SceneSettings {
            portrait_min_face_ratio: 0.1,
            ..SceneSettings::default()
        };
        // 直接测试判定逻辑，避免加载模型。
        let info = SceneInfo {
            kind: SceneKind::Other,
            faces: vec![FaceBox {
                x: 0.0,
                y: 0.0,
                w: 0.4,
                h: 0.4,
            }],
        };
        let is_portrait = info.largest_face_ratio() >= settings.portrait_min_face_ratio;
        assert!(is_portrait);
    }

    #[test]
    fn finds_packaged_library_in_directory() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!("sepiola-ort-{nanos}"));
        std::fs::create_dir_all(&dir).expect("create dir");
        assert_eq!(find_library_in_dir(&dir), None);

        std::fs::write(dir.join("libonnxruntime.so.1.23"), b"x").expect("write library");
        std::fs::write(dir.join("libonnxruntime.so"), b"x").expect("write library");
        std::fs::write(dir.join("readme.txt"), b"x").expect("write file");
        assert_eq!(
            find_library_in_dir(&dir),
            Some(dir.join("libonnxruntime.so"))
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
