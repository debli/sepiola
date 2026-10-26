//! 原图与调色结果按比例混合。

use crate::imageio::{ImageF32, CHANNELS};

/// `out = original * (1 - strength) + graded * strength`，结果 clamp 到 0.0..=1.0。
///
/// `strength` 会被截断到 0.0..=1.0。两张图尺寸必须一致，否则不做任何修改。
pub fn blend(original: &ImageF32, graded: &mut ImageF32, strength: f32) {
    if original.width != graded.width
        || original.height != graded.height
        || original.data.len() != graded.data.len()
    {
        return;
    }
    let s = strength.clamp(0.0, 1.0);
    let inv = 1.0 - s;
    for (dst, src) in graded.data.chunks_exact_mut(CHANNELS).zip(original.data.chunks_exact(CHANNELS)) {
        for channel in 0..CHANNELS {
            dst[channel] = (src[channel] * inv + dst[channel] * s).clamp(0.0, 1.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> ImageF32 {
        ImageF32::from_data(2, 1, vec![0.2, 0.4, 0.6, 0.8, 0.5, 0.25]).expect("valid data")
    }

    #[test]
    fn strength_zero_returns_original() {
        let original = sample();
        let mut graded = ImageF32::from_data(
            2,
            1,
            vec![0.9, 0.9, 0.9, 0.1, 0.1, 0.1],
        )
        .expect("valid data");
        blend(&original, &mut graded, 0.0);
        assert_eq!(graded, original);
    }

    #[test]
    fn strength_one_returns_graded() {
        let original = sample();
        let graded_values = vec![0.9, 0.9, 0.9, 0.1, 0.1, 0.1];
        let mut graded =
            ImageF32::from_data(2, 1, graded_values.clone()).expect("valid data");
        blend(&original, &mut graded, 1.0);
        assert_eq!(graded.data, graded_values);
    }

    #[test]
    fn intermediate_strength_interpolates() {
        let original = ImageF32::from_data(1, 1, vec![0.0, 0.0, 0.0]).expect("valid data");
        let mut graded = ImageF32::from_data(1, 1, vec![1.0, 0.5, 0.0]).expect("valid data");
        blend(&original, &mut graded, 0.5);
        assert!((graded.data[0] - 0.5).abs() < 1e-6);
        assert!((graded.data[1] - 0.25).abs() < 1e-6);
        assert!((graded.data[2] - 0.0).abs() < 1e-6);
    }

    #[test]
    fn output_is_clamped() {
        let original = ImageF32::from_data(1, 1, vec![0.0, 0.0, 0.0]).expect("valid data");
        let mut graded = ImageF32::from_data(1, 1, vec![2.0, -1.0, 0.5]).expect("valid data");
        blend(&original, &mut graded, 1.0);
        assert_eq!(graded.data, vec![1.0, 0.0, 0.5]);
    }
}
