//! 内置 RAW 显影后端：rawloader 解码 + 双线性去马赛克 + 白平衡 + 相机矩阵。
//!
//! 作为未安装 darktable 时的兜底方案，画质（降噪、镜头校正、色调映射等）
//! 明显弱于 darktable。非 RAW 输入直接按普通图片解码后写出 16-bit TIFF。

use std::path::Path;

use rawloader::{RawImage, RawImageData};

use crate::error::{Error, Result};
use crate::imageio::ImageF32;

/// XYZ(D65) → 线性 sRGB。
const XYZ_TO_SRGB: [[f32; 3]; 3] = [
    [3.240_454_2, -1.537_138_5, -0.498_531_4],
    [-0.969_266, 1.876_010_8, 0.041_556],
    [0.055_643_4, -0.204_025_9, 1.057_225_2],
];

/// 将输入显影为 16-bit TIFF。
pub fn develop_to_tiff(input: &Path, out_path: &Path) -> Result<()> {
    let image = if crate::develop::is_raw_format(input) {
        decode_raw(input)?
    } else {
        crate::imageio::load(input)?
    };
    crate::imageio::save_tiff16(&image, out_path)
}

/// 解码 RAW 并转换为 sRGB 图像。
fn decode_raw(path: &Path) -> Result<ImageF32> {
    let raw = rawloader::decode_file(path).map_err(|e| {
        Error::Develop(format!(
            "builtin decoder failed on {}: {e}",
            path.display()
        ))
    })?;

    // crops 顺序为 上、右、下、左。
    let [top, right, bottom, left] = raw.crops;
    let width = raw
        .width
        .checked_sub(left + right)
        .filter(|w| *w > 0)
        .ok_or_else(|| Error::Develop("raw crops leave an empty width".to_string()))?;
    let height = raw
        .height
        .checked_sub(top + bottom)
        .filter(|h| *h > 0)
        .ok_or_else(|| Error::Develop("raw crops leave an empty height".to_string()))?;

    let is_float = matches!(raw.data, RawImageData::Float(_));
    let mut camera = vec![0.0f32; width * height * 3];

    if raw.cpp == 3 {
        for row in 0..height {
            for col in 0..width {
                let src = ((row + top) * raw.width + (col + left)) * 3;
                for channel in 0..3 {
                    camera[(row * width + col) * 3 + channel] =
                        normalized_value(&raw, src + channel, channel, is_float);
                }
            }
        }
    } else {
        let mut plane = vec![0.0f32; width * height];
        for row in 0..height {
            for col in 0..width {
                let src = (row + top) * raw.width + (col + left);
                let channel = raw.cfa.color_at(row + top, col + left);
                plane[row * width + col] = normalized_value(&raw, src, channel, is_float);
            }
        }
        demosaic(&raw, &plane, width, height, top, left, &mut camera);
    }

    let gains = white_balance_gains(&raw);
    let matrix = camera_to_srgb(&raw);
    let mut data = Vec::with_capacity(width * height * 3);
    for pixel in camera.chunks_exact(3) {
        let mut corrected = [0.0f32; 3];
        for channel in 0..3 {
            corrected[channel] = (pixel[channel] * gains[channel]).clamp(0.0, 1.0);
        }
        let linear = apply(&matrix, &corrected);
        for value in linear {
            data.push(encode_srgb(value.clamp(0.0, 1.0)));
        }
    }

    ImageF32::from_data(width as u32, height as u32, data)
}

/// 读取原始值并按黑/白电平归一化到 0..1。
fn normalized_value(raw: &RawImage, index: usize, channel: usize, is_float: bool) -> f32 {
    let value = match &raw.data {
        RawImageData::Integer(values) => values[index] as f32,
        RawImageData::Float(values) => values[index],
    };
    if is_float {
        return value.clamp(0.0, 1.0);
    }
    normalize_scalar(
        value,
        raw.blacklevels[channel] as f32,
        raw.whitelevels[channel] as f32,
    )
}

fn normalize_scalar(value: f32, black: f32, white: f32) -> f32 {
    if white <= black {
        return 0.0;
    }
    ((value - black) / (white - black)).clamp(0.0, 1.0)
}

/// 简易双线性去马赛克：对缺失通道取 3×3 邻域内同色像素的平均值。
///
/// 预先缓存每个像素的 CFA 颜色，避免在内层循环重复计算。
fn demosaic(
    raw: &RawImage,
    plane: &[f32],
    width: usize,
    height: usize,
    top: usize,
    left: usize,
    out: &mut [f32],
) {
    let mut colors = vec![0u8; width * height];
    for row in 0..height {
        for col in 0..width {
            colors[row * width + col] = raw.cfa.color_at(row + top, col + left) as u8;
        }
    }

    for row in 0..height {
        for col in 0..width {
            let index = row * width + col;
            let own = colors[index] as usize;
            let value = plane[index];

            let mut sum = [0.0f32; 3];
            let mut count = [0u16; 3];
            let r0 = row.saturating_sub(1);
            let r1 = (row + 1).min(height - 1);
            let c0 = col.saturating_sub(1);
            let c1 = (col + 1).min(width - 1);
            for r in r0..=r1 {
                for c in c0..=c1 {
                    if r == row && c == col {
                        continue;
                    }
                    let neighbor = r * width + c;
                    let channel = colors[neighbor] as usize;
                    if channel < 3 {
                        sum[channel] += plane[neighbor];
                        count[channel] += 1;
                    }
                }
            }

            for channel in 0..3 {
                let sampled = if own == channel {
                    value
                } else if count[channel] > 0 {
                    sum[channel] / count[channel] as f32
                } else {
                    wide_neighbor_average(&colors, plane, width, height, row, col, channel)
                };
                out[index * 3 + channel] = sampled;
            }
        }
    }
}

/// 3×3 邻域内找不到目标颜色时的 5×5 回退（X-Trans 等非 Bayer 阵列）。
fn wide_neighbor_average(
    colors: &[u8],
    plane: &[f32],
    width: usize,
    height: usize,
    row: usize,
    col: usize,
    channel: usize,
) -> f32 {
    let mut sum = 0.0;
    let mut count = 0usize;
    let r0 = row.saturating_sub(2);
    let r1 = (row + 2).min(height - 1);
    let c0 = col.saturating_sub(2);
    let c1 = (col + 2).min(width - 1);
    for r in r0..=r1 {
        for c in c0..=c1 {
            if colors[r * width + c] as usize == channel {
                sum += plane[r * width + c];
                count += 1;
            }
        }
    }
    if count > 0 {
        sum / count as f32
    } else {
        plane[row * width + col]
    }
}

/// 白平衡增益，按绿色归一化；系数缺失时视为不调整。
fn white_balance_gains(raw: &RawImage) -> [f32; 3] {
    let wb = raw.wb_coeffs;
    let green = wb[1];
    if green <= 0.0 {
        return [1.0, 1.0, 1.0];
    }
    let gains = [wb[0] / green, 1.0, wb[2] / green];
    if gains.iter().any(|g| !g.is_finite() || *g <= 0.0) {
        return [1.0, 1.0, 1.0];
    }
    gains
}

/// 相机 RGB → 线性 sRGB。
///
/// `xyz_to_cam` 的系数在不同厂商文件中量级不一（如 NEF 为 ×10000 的整数），
/// 因此使用 rawloader 已归一化的 `cam_to_xyz_normalized()`，避免矩阵整体过小。
fn camera_to_srgb(raw: &RawImage) -> [[f32; 3]; 3] {
    let cam_to_xyz = raw.cam_to_xyz_normalized();
    let mut cam_to_xyz3 = [[0.0f32; 3]; 3];
    for (row, values) in cam_to_xyz3.iter_mut().enumerate() {
        values.copy_from_slice(&cam_to_xyz[row][..3]);
    }
    let out = multiply(&XYZ_TO_SRGB, &cam_to_xyz3);
    if out.iter().flatten().any(|v| !v.is_finite()) {
        return [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
    }
    out
}

fn apply(m: &[[f32; 3]; 3], v: &[f32; 3]) -> [f32; 3] {
    let mut out = [0.0f32; 3];
    for (row, value) in out.iter_mut().enumerate() {
        *value = (0..3).map(|k| m[row][k] * v[k]).sum();
    }
    out
}

fn multiply(a: &[[f32; 3]; 3], b: &[[f32; 3]; 3]) -> [[f32; 3]; 3] {
    let mut out = [[0.0f32; 3]; 3];
    for (row, out_row) in out.iter_mut().enumerate() {
        for (col, value) in out_row.iter_mut().enumerate() {
            *value = (0..3).map(|k| a[row][k] * b[k][col]).sum();
        }
    }
    out
}

fn encode_srgb(value: f32) -> f32 {
    if value <= 0.003_130_8 {
        12.92 * value
    } else {
        1.055 * value.powf(1.0 / 2.4) - 0.055
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::imageio::{load, save_jpeg};
    use rawloader::CFA;

    #[test]
    fn normalize_applies_black_and_white_levels() {
        assert!((normalize_scalar(1024.0, 1024.0, 16383.0) - 0.0).abs() < 1e-6);
        assert!((normalize_scalar(16383.0, 1024.0, 16383.0) - 1.0).abs() < 1e-6);
        assert!(normalize_scalar(500.0, 1024.0, 16383.0) == 0.0);
        assert_eq!(normalize_scalar(100.0, 10.0, 10.0), 0.0);
    }

    #[test]
    fn srgb_encoding_hits_endpoints() {
        assert!((encode_srgb(0.0) - 0.0).abs() < 1e-6);
        assert!((encode_srgb(1.0) - 1.0).abs() < 1e-6);
        let mid = encode_srgb(0.5);
        assert!((mid - 0.735).abs() < 0.01, "mid={mid}");
    }

    #[test]
    fn demosaic_preserves_uniform_color_mosaic() {
        // 构造一个 flat RGGB 画面：R=0.8, G=0.4, B=0.2。
        let cfa = CFA::new("RGGB");
        let (width, height) = (8usize, 8usize);
        let mut plane = vec![0.0f32; width * height];
        for row in 0..height {
            for col in 0..width {
                plane[row * width + col] = match cfa.color_at(row, col) {
                    0 => 0.8,
                    1 => 0.4,
                    _ => 0.2,
                };
            }
        }

        let mut camera = vec![0.0f32; width * height * 3];
        let raw = dummy_raw(cfa, width, height);
        demosaic(&raw, &plane, width, height, 0, 0, &mut camera);

        let center = (4 * width + 4) * 3;
        assert!((camera[center] - 0.8).abs() < 0.02, "R={}", camera[center]);
        assert!(
            (camera[center + 1] - 0.4).abs() < 0.02,
            "G={}",
            camera[center + 1]
        );
        assert!(
            (camera[center + 2] - 0.2).abs() < 0.02,
            "B={}",
            camera[center + 2]
        );
    }

    fn dummy_raw(cfa: CFA, width: usize, height: usize) -> RawImage {
        RawImage {
            make: "test".to_string(),
            model: "test".to_string(),
            clean_make: "test".to_string(),
            clean_model: "test".to_string(),
            width,
            height,
            cpp: 1,
            wb_coeffs: [1.0, 1.0, 1.0, 1.0],
            whitelevels: [65535, 65535, 65535, 65535],
            blacklevels: [0, 0, 0, 0],
            xyz_to_cam: [[0.0; 3]; 4],
            cfa,
            crops: [0, 0, 0, 0],
            blackareas: Vec::new(),
            orientation: rawloader::Orientation::Normal,
            data: RawImageData::Integer(vec![0; width * height]),
        }
    }

    #[test]
    fn camera_matrix_ignores_coefficient_scale() {
        // NEF 等文件的 xyz_to_cam 是 ×10000 的整数；缩放不应影响最终矩阵。
        let matrix = [
            [7732.0, -2422.0, -789.0],
            [-8238.0, 15884.0, 2498.0],
            [-859.0, 783.0, 7330.0],
        ];
        let mut integer = dummy_raw(CFA::new("RGGB"), 2, 2);
        integer.xyz_to_cam = [matrix[0], matrix[1], matrix[2], [0.0; 3]];
        let mut scaled = dummy_raw(CFA::new("RGGB"), 2, 2);
        scaled.xyz_to_cam = [
            matrix[0].map(|v| v / 10000.0),
            matrix[1].map(|v| v / 10000.0),
            matrix[2].map(|v| v / 10000.0),
            [0.0; 3],
        ];

        let a = camera_to_srgb(&integer);
        let b = camera_to_srgb(&scaled);
        for row in 0..3 {
            for col in 0..3 {
                assert!(
                    (a[row][col] - b[row][col]).abs() < 1e-3,
                    "a={} b={}",
                    a[row][col],
                    b[row][col]
                );
            }
        }
        assert!(a[0][0] > 0.5, "matrix is unexpectedly small: {a:?}");
    }

    #[test]
    fn develops_real_raw_sample() {
        // 真实样本：raw.pixls.us 的 Nikon D70 NEF（5,488,179 字节）。
        let sample = Path::new(env!("CARGO_MANIFEST_DIR")).join("test_data/raw/nikon_d70.nef");
        if !sample.is_file() {
            eprintln!("missing sample {}, skipping", sample.display());
            return;
        }

        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!("sepiola-builtin-raw-{nanos}"));
        std::fs::create_dir_all(&dir).expect("create dir");

        let output = dir.join("nikon_d70.tif");
        let started = std::time::Instant::now();
        develop_to_tiff(&sample, &output).expect("develop real raw");
        let elapsed = started.elapsed();

        let loaded = load(&output).expect("load tiff");
        assert_eq!((loaded.width, loaded.height), (3038, 2014));

        // 均值与标准差应处于合理区间：既非全黑/全白，也不是平坦画面。
        let count = loaded.data.len() as f64;
        let mean = loaded.data.iter().map(|v| *v as f64).sum::<f64>() / count;
        let variance = loaded
            .data
            .iter()
            .map(|v| {
                let diff = *v as f64 - mean;
                diff * diff
            })
            .sum::<f64>()
            / count;
        let std = variance.sqrt();
        eprintln!(
            "real raw develop: {}x{} in {:?}, mean={mean:.3}, std={std:.3}",
            loaded.width, loaded.height, elapsed
        );
        assert!(mean > 0.02 && mean < 0.98, "mean={mean}");
        assert!(std > 0.02, "std={std}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn non_raw_input_is_converted_to_tiff() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!("sepiola-builtin-{nanos}"));
        std::fs::create_dir_all(&dir).expect("create dir");

        let input = dir.join("input.jpg");
        let mut img = ImageF32::new(9, 7).expect("valid size");
        for (i, value) in img.data.iter_mut().enumerate() {
            *value = (i % 13) as f32 / 12.0;
        }
        save_jpeg(&img, &input, 95).expect("save jpeg");

        let output = dir.join("out.tif");
        develop_to_tiff(&input, &output).expect("convert");
        let loaded = load(&output).expect("load tiff");
        assert_eq!((loaded.width, loaded.height), (9, 7));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
