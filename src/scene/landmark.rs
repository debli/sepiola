//! 人脸 106 关键点模型（InsightFace 2d106det）与相似变换对齐。
//!
//! 用途：为身份识别提供 digiKam 式姿态对齐——先在 192×192 普通裁剪上
//! 估计关键点，取双眼/鼻尖/嘴角 5 点，再用相似变换对齐到 ArcFace 标准模板。

use std::path::Path;
use std::sync::Mutex;

use ort::session::Session;
use ort::value::Tensor;

use crate::error::{Error, Result};
use crate::imageio::{ImageF32, CHANNELS};
use crate::paths::resolve_asset;
use crate::scene::face::ort_environment;

/// 关键点模型输入边长（2d106det 为 192×192）。
pub const SIDE: u32 = 192;
/// 关键点数量。
pub const NUM_POINTS: usize = 106;

/// ArcFace 112×112 标准 5 点模板（左眼、右眼、鼻尖、左嘴角、右嘴角）。
/// 关键点在 192 空间估计，相似变换 src=192 空间、dst=112 模板空间，直接 warp 到 112。
pub fn arc_template_5() -> [[f32; 2]; 5] {
    [
        [38.2946, 46.631],
        [73.631, 46.631],
        [56.5481, 67.9553],
        [41.7043, 86.7056],
        [73.0587, 86.7056],
    ]
}

/// 2D106 关键点估计器。线程安全。
pub struct LandmarkModel {
    session: Mutex<Session>,
}

impl LandmarkModel {
    /// 加载模型；文件不存在返回 Err（调用方回退到普通裁剪）。
    pub fn load(configured_path: &str) -> Result<Self> {
        let path = resolve_asset(configured_path);
        if !path.is_file() {
            return Err(Error::Model(format!(
                "landmark model not found: {}",
                path.display()
            )));
        }
        let environment = ort_environment()?;
        tracing::info!(model = %path.display(), "loading landmark model");
        let session = Session::builder(environment)
            .and_then(|mut builder| builder.commit_from_file(&path))
            .map_err(|e| Error::Model(format!("failed to load {}: {e}", path.display())))?;
        Ok(Self {
            session: Mutex::new(session),
        })
    }

    /// 在 192×192 裁剪上估计 106 个关键点（像素坐标，0..192）。
    pub fn landmarks(&self, crop: &ImageF32) -> Result<Vec<[f32; 2]>> {
        let plane = (SIDE * SIDE) as usize;
        let mut data = vec![0.0f32; plane * CHANNELS];
        for (idx, px) in crop.data.chunks_exact(CHANNELS).enumerate() {
            // 该导出已把 (x-127.5)/127.5 归一化烘焙进图（首节点 Sub/Div），
            // 因此必须喂原始像素值 0..255、BGR 通道序（InsightFace cv2 约定）。
            for c in 0..CHANNELS {
                data[c * plane + idx] = px[CHANNELS - 1 - c] * 255.0;
            }
        }
        let tensor = Tensor::from_array((
            [1usize, CHANNELS, SIDE as usize, SIDE as usize],
            data.into_boxed_slice(),
        ))
        .map_err(|e| Error::Model(format!("landmark input: {e}")))?;
        let mut session = self
            .session
            .lock()
            .map_err(|_| Error::Model("landmark mutex poisoned".to_string()))?;
        let outputs = session
            .run(ort::inputs!["data" => tensor])
            .map_err(|e| Error::Model(format!("landmark inference failed: {e}")))?;
        let value = &outputs[0];
        let (_, flat) = value
            .try_extract_tensor::<f32>()
            .map_err(|e| Error::Model(format!("landmark output: {e}")))?;
        if flat.len() < NUM_POINTS * 2 {
            return Err(Error::Model(format!(
                "landmark output too short: {} < {}",
                flat.len(),
                NUM_POINTS * 2
            )));
        }
        // 输出布局（实测）：(x,y) 成对交错，归一化到 [-1,1]。
        // 换算为像素 (v+1)/2*192 = v*96+96。若个别导出已是像素坐标（量级 >1.5）则原样使用。
        let raw: Vec<[f32; 2]> = (0..NUM_POINTS)
            .map(|i| [flat[i * 2], flat[i * 2 + 1]])
            .collect();
        let max_abs = raw
            .iter()
            .flat_map(|p| [p[0].abs(), p[1].abs()])
            .fold(0.0f32, f32::max);
        let points: Vec<[f32; 2]> = if max_abs <= 1.5 {
            raw.into_iter()
                .map(|p| [p[0] * 96.0 + 96.0, p[1] * 96.0 + 96.0])
                .collect()
        } else {
            raw
        };
        Ok(points)
    }
}

/// 从 106 点提取 ArcFace 5 点（模板顺序：画面左眼、画面右眼、鼻尖、画面左嘴角、画面右嘴角）。
/// 索引映射经 men_1 实测校准：轮廓 0..=32、画面左眼 33..=42、左眉 43..=51、
/// 嘴 52..=71、鼻 75..=86、画面右眼 87..=96、右眉 97..=105。
pub fn five_points(points: &[[f32; 2]]) -> Option<[[f32; 2]; 5]> {
    if points.len() < NUM_POINTS {
        return None;
    }
    let centroid = |range: std::ops::RangeInclusive<usize>| -> [f32; 2] {
        let n = range.end() - range.start() + 1;
        let mut acc = [0.0f32; 2];
        for p in &points[range] {
            acc[0] += p[0];
            acc[1] += p[1];
        }
        [acc[0] / n as f32, acc[1] / n as f32]
    };
    // 鼻尖取鼻部点集中 y 最大者（最靠下）。
    let nose = *points[75..=86]
        .iter()
        .max_by(|a, b| a[1].partial_cmp(&b[1]).unwrap_or(std::cmp::Ordering::Equal))?;
    // 嘴角：嘴部点集（52..=71）中 x 最小/最大者。
    let mouth = &points[52..=71];
    let left = *mouth
        .iter()
        .min_by(|a, b| a[0].partial_cmp(&b[0]).unwrap_or(std::cmp::Ordering::Equal))?;
    let right = *mouth
        .iter()
        .max_by(|a, b| a[0].partial_cmp(&b[0]).unwrap_or(std::cmp::Ordering::Equal))?;
    Some([
        centroid(33..=42),
        centroid(87..=96),
        nose,
        left,
        right,
    ])
}

/// 相似变换（旋转+等比缩放+平移）最小二乘拟合 src→dst（Umeyama，2D 闭式解）。
/// 返回仿射矩阵 [a b tx; c d ty]，映射 dst = M·src。
pub fn similarity_transform(src: &[[f32; 2]], dst: &[[f32; 2]]) -> Option<[f32; 6]> {
    if src.len() != dst.len() || src.len() < 2 {
        return None;
    }
    let mean = |pts: &[[f32; 2]]| -> [f32; 2] {
        let n = pts.len() as f32;
        [
            pts.iter().map(|p| p[0]).sum::<f32>() / n,
            pts.iter().map(|p| p[1]).sum::<f32>() / n,
        ]
    };
    let (ms, md) = (mean(src), mean(dst));
    // 复数法：sum((q-mean_q) * conj(p-mean_p)) 给出旋转角与缩放。
    let (mut re, mut im, mut sp) = (0.0f32, 0.0f32, 0.0f32);
    for (p, q) in src.iter().zip(dst) {
        let (px, py) = (p[0] - ms[0], p[1] - ms[1]);
        let (qx, qy) = (q[0] - md[0], q[1] - md[1]);
        re += qx * px + qy * py;
        im += qy * px - qx * py;
        sp += px * px + py * py;
    }
    if sp <= 0.0 || (re == 0.0 && im == 0.0) {
        return None;
    }
    // 目标：q ≈ s·R·p。复数和 = s·e^{iθ}·Σ|p|²。
    let norm = (re * re + im * im).sqrt();
    let scale = norm / sp;
    let (cos, sin) = (re / norm, im / norm);
    // R = [cos -sin; sin cos]（注意 y 轴向下坐标系中该式仍为刚体旋转）。
    let a = scale * cos;
    let b = -scale * sin;
    let c = scale * sin;
    let d = scale * cos;
    let tx = md[0] - (a * ms[0] + b * ms[1]);
    let ty = md[1] - (c * ms[0] + d * ms[1]);
    Some([a, b, tx, c, d, ty])
}

/// 逆变换：dst→src，用于反向采样。
fn invert(m: &[f32; 6]) -> Option<[f32; 6]> {
    let det = m[0] * m[4] - m[1] * m[3];
    if det.abs() < 1e-9 {
        return None;
    }
    let (a, b, c, d) = (m[4] / det, -m[1] / det, -m[3] / det, m[0] / det);
    Some([
        a,
        b,
        -(a * m[2] + b * m[5]),
        c,
        d,
        -(c * m[2] + d * m[5]),
    ])
}

/// 将 112×112 裁剪按相似变换对齐到 112×112 模板（双线性采样，越界取 0）。
pub fn warp_similarity(src: &ImageF32, m: &[f32; 6], side: u32) -> Result<ImageF32> {
    let inv = invert(m).ok_or_else(|| Error::Model("singular similarity matrix".to_string()))?;
    let mut out = ImageF32::new(side, side)?;
    for y in 0..side {
        for x in 0..side {
            let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
            let sx = inv[0] * fx + inv[1] * fy + inv[2];
            let sy = inv[3] * fx + inv[4] * fy + inv[5];
            let dst = (y as usize * side as usize + x as usize) * CHANNELS;
            if sx < 0.0 || sy < 0.0 || sx > src.width as f32 || sy > src.height as f32 {
                continue;
            }
            let x0 = sx.floor().max(0.0) as usize;
            let y0 = sy.floor().max(0.0) as usize;
            let x1 = (x0 + 1).min(src.width as usize - 1);
            let y1 = (y0 + 1).min(src.height as usize - 1);
            let (wx, wy) = (sx - x0 as f32, sy - y0 as f32);
            let idx = |px: usize, py: usize| (py * src.width as usize + px) * CHANNELS;
            for c in 0..CHANNELS {
                let top = src.data[idx(x0, y0) + c] * (1.0 - wx) + src.data[idx(x1, y0) + c] * wx;
                let bottom =
                    src.data[idx(x0, y1) + c] * (1.0 - wx) + src.data[idx(x1, y1) + c] * wx;
                out.data[dst + c] = top * (1.0 - wy) + bottom * wy;
            }
        }
    }
    Ok(out)
}

/// 从模型路径加载；空路径/缺失返回 None。
pub fn load_landmark_model(configured: &str) -> Option<LandmarkModel> {
    if configured.trim().is_empty() {
        return None;
    }
    let path = resolve_asset(configured);
    if !Path::new(&path).is_file() {
        tracing::warn!(model = %path.display(), "landmark model unavailable; using plain crop");
        return None;
    }
    match LandmarkModel::load(configured) {
        Ok(model) => Some(model),
        Err(error) => {
            tracing::warn!(%error, "landmark model load failed; using plain crop");
            None
        }
    }
}
