//! 影调统计：平均亮度与高光/暗部裁切比例。

use crate::imageio::{ImageF32, CHANNELS};

/// 亮度高于该值视为高光裁切。
pub const CLIP_HIGH: f32 = 0.99;
/// 亮度低于该值视为暗部裁切。
pub const CLIP_LOW: f32 = 0.01;

/// 一张图的影调统计。裁切比例是像素占比（0.0..=1.0）。
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ToneStats {
    pub mean_luma: f32,
    pub clipped_high: f32,
    pub clipped_low: f32,
}

/// 按 sRGB 系数计算单个像素的亮度。
#[inline]
pub fn pixel_luma(r: f32, g: f32, b: f32) -> f32 {
    0.2126 * r + 0.7152 * g + 0.0722 * b
}

/// 计算平均亮度与高光/暗部裁切比例。
pub fn analyze(img: &ImageF32) -> ToneStats {
    let pixels = img.pixel_count();
    if pixels == 0 || img.data.len() < pixels * CHANNELS {
        return ToneStats::default();
    }

    let mut sum = 0.0f64;
    let mut high = 0u64;
    let mut low = 0u64;
    for pixel in img.data.chunks_exact(CHANNELS) {
        let luma = pixel_luma(pixel[0], pixel[1], pixel[2]);
        sum += luma as f64;
        if luma >= CLIP_HIGH {
            high += 1;
        }
        if luma <= CLIP_LOW {
            low += 1;
        }
    }

    ToneStats {
        mean_luma: (sum / pixels as f64) as f32,
        clipped_high: high as f32 / pixels as f32,
        clipped_low: low as f32 / pixels as f32,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pure_white_is_fully_clipped_high() {
        let img = ImageF32::from_data(2, 2, vec![1.0; 2 * 2 * CHANNELS]).expect("valid data");
        let stats = analyze(&img);
        assert!((stats.clipped_high - 1.0).abs() < 1e-6);
        assert!((stats.clipped_low - 0.0).abs() < 1e-6);
        assert!((stats.mean_luma - 1.0).abs() < 1e-6);
    }

    #[test]
    fn pure_black_is_fully_clipped_low() {
        let img = ImageF32::from_data(2, 2, vec![0.0; 2 * 2 * CHANNELS]).expect("valid data");
        let stats = analyze(&img);
        assert!((stats.clipped_low - 1.0).abs() < 1e-6);
        assert!((stats.clipped_high - 0.0).abs() < 1e-6);
    }

    #[test]
    fn mid_gray_is_not_clipped() {
        let img = ImageF32::from_data(1, 1, vec![0.5, 0.5, 0.5]).expect("valid data");
        let stats = analyze(&img);
        assert!((stats.clipped_high - 0.0).abs() < 1e-6);
        assert!((stats.clipped_low - 0.0).abs() < 1e-6);
        assert!((stats.mean_luma - 0.5).abs() < 1e-6);
    }

    #[test]
    fn ratios_are_pixel_fractions() {
        // 4 个像素：两个纯白、两个中灰。
        let mut data = vec![0.5; 4 * CHANNELS];
        data[6..9].copy_from_slice(&[1.0, 1.0, 1.0]);
        data[9..12].copy_from_slice(&[1.0, 1.0, 1.0]);
        let img = ImageF32::from_data(2, 2, data).expect("valid data");
        let stats = analyze(&img);
        assert!((stats.clipped_high - 0.5).abs() < 1e-6);
    }
}
