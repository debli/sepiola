//! 3D LUT（`.cube`）解析与三线性插值应用。
//!
//! LUT 作用在 sRGB 编码值上，不走线性空间。

use std::path::Path;

use crate::error::{Error, Result};
use crate::imageio::{ImageF32, CHANNELS};

/// 一个 3D LUT 表。
///
/// `table` 的索引方式为 `r + g * size + b * size * size`（R 变化最快）。
#[derive(Clone, Debug)]
pub struct Lut3d {
    size: usize,
    domain_min: [f32; 3],
    domain_max: [f32; 3],
    table: Vec<[f32; 3]>,
}

impl Lut3d {
    /// 从 `.cube` 文件读取。
    pub fn from_cube_file(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| Error::Lut(format!("failed to read {}: {e}", path.display())))?;
        Self::from_cube_str(&text)
    }

    /// 从 `.cube` 文本解析，便于测试。
    pub fn from_cube_str(text: &str) -> Result<Self> {
        let mut size: Option<usize> = None;
        let mut domain_min = [0.0f32; 3];
        let mut domain_max = [1.0f32; 3];
        let mut table: Vec<[f32; 3]> = Vec::new();
        let mut saw_domain = false;

        for (line_no, raw_line) in text.lines().enumerate() {
            let line = raw_line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let mut parts = line.split_whitespace();
            let Some(keyword) = parts.next() else {
                continue;
            };

            match keyword {
                "TITLE" => continue,
                "LUT_3D_SIZE" => {
                    let value = parts.next().ok_or_else(|| {
                        Error::Lut(format!("line {}: LUT_3D_SIZE missing value", line_no + 1))
                    })?;
                    let parsed: usize = value.parse().map_err(|_| {
                        Error::Lut(format!(
                            "line {}: invalid LUT_3D_SIZE '{value}'",
                            line_no + 1
                        ))
                    })?;
                    if parsed < 2 {
                        return Err(Error::Lut(format!(
                            "line {}: LUT_3D_SIZE must be >= 2, got {parsed}",
                            line_no + 1
                        )));
                    }
                    size = Some(parsed);
                }
                "LUT_1D_SIZE" => {
                    return Err(Error::Lut(
                        "1D LUT is not supported, expected a 3D LUT".to_string(),
                    ));
                }
                "DOMAIN_MIN" | "DOMAIN_MAX" => {
                    let mut values = [0.0f32; 3];
                    for (i, value) in values.iter_mut().enumerate() {
                        let raw = parts.next().ok_or_else(|| {
                            Error::Lut(format!(
                                "line {}: {keyword} needs 3 values, missing #{}",
                                line_no + 1,
                                i + 1
                            ))
                        })?;
                        *value = raw.parse().map_err(|_| {
                            Error::Lut(format!(
                                "line {}: invalid number '{raw}' in {keyword}",
                                line_no + 1
                            ))
                        })?;
                    }
                    if keyword == "DOMAIN_MIN" {
                        domain_min = values;
                    } else {
                        domain_max = values;
                    }
                    saw_domain = true;
                }
                _ => {
                    // 数据行没有关键字，第一个 token 本身就是 R 值。
                    let mut values = [0.0f32; 3];
                    values[0] = keyword.parse().map_err(|_| {
                        Error::Lut(format!(
                            "line {}: invalid number '{keyword}' in data row",
                            line_no + 1
                        ))
                    })?;
                    for (i, value) in values.iter_mut().enumerate().skip(1) {
                        let raw = parts.next().ok_or_else(|| {
                            Error::Lut(format!(
                                "line {}: data row needs 3 values, missing #{}",
                                line_no + 1,
                                i + 1
                            ))
                        })?;
                        *value = raw.parse().map_err(|_| {
                            Error::Lut(format!(
                                "line {}: invalid number '{raw}' in data row",
                                line_no + 1
                            ))
                        })?;
                    }
                    table.push(values);
                }
            }
        }

        let size = size.ok_or_else(|| Error::Lut("missing LUT_3D_SIZE".to_string()))?;
        let expected = size * size * size;
        if table.len() != expected {
            return Err(Error::Lut(format!(
                "expected {expected} entries for size {size}, got {}",
                table.len()
            )));
        }
        for channel in 0..3 {
            if !(domain_max[channel] > domain_min[channel]) {
                return Err(Error::Lut(format!(
                    "invalid DOMAIN range on channel {channel}: min {} max {}",
                    domain_min[channel], domain_max[channel]
                )));
            }
        }
        if saw_domain {
            tracing::debug!("LUT domain: min {:?} max {:?}", domain_min, domain_max);
        }

        Ok(Self {
            size,
            domain_min,
            domain_max,
            table,
        })
    }

    /// LUT 边长（每个通道的采样点数）。
    pub fn size(&self) -> usize {
        self.size
    }

    /// 对图像原地应用 LUT（三线性插值）。
    pub fn apply(&self, img: &mut ImageF32) {
        for pixel in img.data.chunks_exact_mut(CHANNELS) {
            let out = self.sample([pixel[0], pixel[1], pixel[2]]);
            pixel[0] = out[0];
            pixel[1] = out[1];
            pixel[2] = out[2];
        }
    }

    /// 对单个 sRGB 颜色做三线性插值。
    pub fn sample(&self, color: [f32; 3]) -> [f32; 3] {
        let last = (self.size - 1) as f32;
        let mut coord = [0.0f32; 3];
        for channel in 0..3 {
            let span = self.domain_max[channel] - self.domain_min[channel];
            let normalized = (color[channel] - self.domain_min[channel]) / span;
            coord[channel] = (normalized * last).clamp(0.0, last);
        }

        let mut base = [0usize; 3];
        let mut frac = [0.0f32; 3];
        let mut upper = [0usize; 3];
        for channel in 0..3 {
            let floor = coord[channel].floor();
            base[channel] = floor as usize;
            upper[channel] = (base[channel] + 1).min(self.size - 1);
            frac[channel] = coord[channel] - floor;
        }

        let mut result = [0.0f32; 3];
        for (dr, wr) in [(0, 1.0 - frac[0]), (1, frac[0])] {
            let r = if dr == 0 { base[0] } else { upper[0] };
            for (dg, wg) in [(0, 1.0 - frac[1]), (1, frac[1])] {
                let g = if dg == 0 { base[1] } else { upper[1] };
                for (db, wb) in [(0, 1.0 - frac[2]), (1, frac[2])] {
                    let b = if db == 0 { base[2] } else { upper[2] };
                    let weight = wr * wg * wb;
                    let value = self.table[self.index(r, g, b)];
                    result[0] += value[0] * weight;
                    result[1] += value[1] * weight;
                    result[2] += value[2] * weight;
                }
            }
        }
        result
    }

    #[inline]
    fn index(&self, r: usize, g: usize, b: usize) -> usize {
        r + g * self.size + b * self.size * self.size
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 生成一个 size=2 的 identity LUT 文本。
    fn identity_cube() -> String {
        let mut text = String::from("TITLE \"identity\"\nLUT_3D_SIZE 2\n");
        // R 变化最快：b, g, r
        for b in 0..2 {
            for g in 0..2 {
                for r in 0..2 {
                    text.push_str(&format!("{r} {g} {b}\n"));
                }
            }
        }
        text
    }

    fn gradient(width: u32, height: u32) -> ImageF32 {
        let mut img = ImageF32::new(width, height).expect("valid size");
        for y in 0..height {
            for x in 0..width {
                let idx = (y as usize * width as usize + x as usize) * CHANNELS;
                img.data[idx] = x as f32 / (width - 1) as f32;
                img.data[idx + 1] = y as f32 / (height - 1) as f32;
                img.data[idx + 2] = 0.25;
            }
        }
        img
    }

    #[test]
    fn identity_lut_keeps_image_unchanged() {
        let lut = Lut3d::from_cube_str(&identity_cube()).expect("parse cube");
        let original = gradient(9, 7);
        let mut img = original.clone();
        lut.apply(&mut img);
        for (a, b) in original.data.iter().zip(img.data.iter()) {
            assert!((a - b).abs() < 1e-6, "identity LUT changed {a} to {b}");
        }
    }

    #[test]
    fn known_lut_matches_manual_result() {
        // 交换 R 与 B 的两个端点（size=2），中点插值。
        let mut text = String::from("LUT_3D_SIZE 2\n");
        for b in 0..2 {
            for g in 0..2 {
                for r in 0..2 {
                    text.push_str(&format!("{b} {g} {r}\n"));
                }
            }
        }
        let lut = Lut3d::from_cube_str(&text).expect("parse cube");
        let out = lut.sample([0.25, 0.5, 0.75]);
        assert!((out[0] - 0.75).abs() < 1e-6, "R = {}", out[0]);
        assert!((out[1] - 0.5).abs() < 1e-6, "G = {}", out[1]);
        assert!((out[2] - 0.25).abs() < 1e-6, "B = {}", out[2]);
    }

    #[test]
    fn domain_is_honoured() {
        let text = "LUT_3D_SIZE 2\nDOMAIN_MIN 0 0 0\nDOMAIN_MAX 2 2 2\n0 0 0\n1 0 0\n0 1 0\n1 1 0\n0 0 1\n1 0 1\n0 1 1\n1 1 1\n";
        let lut = Lut3d::from_cube_str(text).expect("parse cube");
        // 输入范围 0..2，0.5 对应表内位置的 0.25，identity 表输出 0.25。
        let out = lut.sample([0.5, 0.5, 0.5]);
        assert!((out[0] - 0.25).abs() < 1e-6);
        assert!((out[1] - 0.25).abs() < 1e-6);
        assert!((out[2] - 0.25).abs() < 1e-6);
    }

    #[test]
    fn missing_size_is_error() {
        assert!(Lut3d::from_cube_str("0 0 0\n").is_err());
    }

    #[test]
    fn wrong_entry_count_is_error() {
        let text = "LUT_3D_SIZE 2\n0 0 0\n";
        assert!(Lut3d::from_cube_str(text).is_err());
    }
}
