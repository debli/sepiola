//! 图像读写：统一使用 [`ImageF32`] 作为内部表示。

use std::path::Path;

use image::{DynamicImage, ImageBuffer, Rgb};

use crate::error::{Error, Result};

/// 每个像素的通道数（RGB）。
pub const CHANNELS: usize = 3;

/// RGB，行优先，每通道 f32，范围 0.0..=1.0，sRGB 编码（非线性）。
#[derive(Clone, Debug, PartialEq)]
pub struct ImageF32 {
    pub width: u32,
    pub height: u32,
    /// 长度恒为 `width * height * CHANNELS`。
    pub data: Vec<f32>,
}

impl ImageF32 {
    /// 创建一张全黑的图。
    pub fn new(width: u32, height: u32) -> Result<Self> {
        if width == 0 || height == 0 {
            return Err(Error::Image(format!(
                "image size must be non-zero, got {width}x{height}"
            )));
        }
        Ok(Self {
            width,
            height,
            data: vec![0.0; width as usize * height as usize * CHANNELS],
        })
    }

    /// 用已有的像素数据构造，长度必须匹配。
    pub fn from_data(width: u32, height: u32, data: Vec<f32>) -> Result<Self> {
        let expected = width as usize * height as usize * CHANNELS;
        if data.len() != expected {
            return Err(Error::Image(format!(
                "pixel buffer length {} does not match {width}x{height}x{CHANNELS} = {expected}",
                data.len()
            )));
        }
        Ok(Self {
            width,
            height,
            data,
        })
    }

    /// 像素总数。
    pub fn pixel_count(&self) -> usize {
        self.width as usize * self.height as usize
    }

    /// 是否为合法尺寸（宽高非零且缓冲区长度匹配）。
    pub fn is_valid(&self) -> bool {
        self.width > 0
            && self.height > 0
            && self.data.len() == self.width as usize * self.height as usize * CHANNELS
    }
}

/// 读取 TIFF/JPEG 等常见格式，转换为 f32 并归一化到 0.0..=1.0。
///
/// 8-bit 数据除以 255，16-bit 数据除以 65535。
pub fn load(path: &Path) -> Result<ImageF32> {
    let img = image::open(path)
        .map_err(|e| Error::Image(format!("failed to open {}: {e}", path.display())))?;
    Ok(dynamic_to_f32(img))
}

/// 按指定质量保存为 JPEG。
pub fn save_jpeg(img: &ImageF32, path: &Path, quality: u8) -> Result<()> {
    if !img.is_valid() {
        return Err(Error::Image(format!(
            "invalid image buffer {width}x{height}",
            width = img.width,
            height = img.height
        )));
    }
    let buffer = to_rgb8(img);
    let file = std::fs::File::create(path)?;
    let mut writer = std::io::BufWriter::new(file);
    let mut encoder =
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut writer, quality.clamp(1, 100));
    encoder
        .encode_image(&buffer)
        .map_err(|e| Error::Image(format!("failed to encode {}: {e}", path.display())))?;
    Ok(())
}

/// 保存为 16-bit TIFF。
pub fn save_tiff16(img: &ImageF32, path: &Path) -> Result<()> {
    if !img.is_valid() {
        return Err(Error::Image(format!(
            "invalid image buffer {width}x{height}",
            width = img.width,
            height = img.height
        )));
    }
    let mut raw: Vec<u16> = Vec::with_capacity(img.data.len());
    for value in &img.data {
        raw.push((value.clamp(0.0, 1.0) * 65535.0).round() as u16);
    }
    let buffer: ImageBuffer<Rgb<u16>, Vec<u16>> =
        ImageBuffer::from_raw(img.width, img.height, raw).ok_or_else(|| {
            Error::Image(format!(
                "failed to build 16-bit buffer {width}x{height}",
                width = img.width,
                height = img.height
            ))
        })?;
    DynamicImage::ImageRgb16(buffer)
        .save_with_format(path, image::ImageFormat::Tiff)
        .map_err(|e| Error::Image(format!("failed to save {}: {e}", path.display())))
}

/// 扫描 JPEG 字节流中的元数据段（APP1/APP2/COM），返回各段的 `[start, end)` 区间
/// （含 2 字节标记）。遇到 SOS 停止（其后为熵编码数据，不能再按段解析）。
/// 非 JPEG 或结构异常时返回空列表。
fn jpeg_meta_ranges(bytes: &[u8]) -> Vec<(usize, usize)> {
    let mut ranges = Vec::new();
    if bytes.len() < 2 || bytes[0] != 0xFF || bytes[1] != 0xD8 {
        return ranges;
    }
    let mut pos = 2;
    while pos + 3 < bytes.len() {
        if bytes[pos] != 0xFF {
            break;
        }
        let marker = bytes[pos + 1];
        // 无长度字段的独立标记：TEM 与 RSTn/SOI/EOI。
        if marker == 0x01 || (0xD0..=0xD9).contains(&marker) {
            pos += 2;
            continue;
        }
        if marker == 0xDA {
            break;
        }
        let len = ((bytes[pos + 2] as usize) << 8) | bytes[pos + 3] as usize;
        if len < 2 || pos + 2 + len > bytes.len() {
            break;
        }
        let end = pos + 2 + len;
        if matches!(marker, 0xE1 | 0xE2 | 0xFE) {
            ranges.push((pos, end));
        }
        pos = end;
    }
    ranges
}

/// 从源 JPEG 提取 APP1/APP2/COM 段并插入到目标 JPEG（SOI 之后、首个非 APP0 段之前）。
/// 源不含元数据段或目标非 JPEG 时静默跳过。
pub fn jpeg_copy_metadata(src: &Path, dst: &Path) -> Result<()> {
    let src_bytes = std::fs::read(src)?;
    let ranges = jpeg_meta_ranges(&src_bytes);
    if ranges.is_empty() {
        return Ok(());
    }
    let mut meta = Vec::new();
    for (start, end) in ranges {
        meta.extend_from_slice(&src_bytes[start..end]);
    }

    let dst_bytes = std::fs::read(dst)?;
    if dst_bytes.len() < 2 || dst_bytes[0] != 0xFF || dst_bytes[1] != 0xD8 {
        return Ok(());
    }
    // 插入点：SOI 之后；若首段是 APP0（JFIF），插到它后面。
    let mut insert = 2;
    if dst_bytes.len() >= 6 && dst_bytes[2] == 0xFF && dst_bytes[3] == 0xE0 {
        let len = ((dst_bytes[4] as usize) << 8) | dst_bytes[5] as usize;
        if len >= 2 {
            insert = (2 + 2 + len).min(dst_bytes.len());
        }
    }
    let mut out = Vec::with_capacity(dst_bytes.len() + meta.len());
    out.extend_from_slice(&dst_bytes[..insert]);
    out.extend_from_slice(&meta);
    out.extend_from_slice(&dst_bytes[insert..]);
    std::fs::write(dst, out)?;
    Ok(())
}

/// 删除 JPEG 文件中的 APP1/APP2/COM 段（EXIF/ICC/注释）。
pub fn jpeg_strip_metadata(path: &Path) -> Result<()> {
    let bytes = std::fs::read(path)?;
    let ranges = jpeg_meta_ranges(&bytes);
    if ranges.is_empty() {
        return Ok(());
    }
    let mut out = Vec::with_capacity(bytes.len());
    let mut pos = 0;
    for (start, end) in ranges {
        out.extend_from_slice(&bytes[pos..start]);
        pos = end;
    }
    out.extend_from_slice(&bytes[pos..]);
    std::fs::write(path, &out)?;
    Ok(())
}

/// 文件扩展名是否为 JPEG。
pub fn is_jpeg_path(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.eq_ignore_ascii_case("jpg") || ext.eq_ignore_ascii_case("jpeg"))
        .unwrap_or(false)
}

/// 转换为 8-bit RGB 缓冲区（用于编码）。
pub(crate) fn to_rgb8(img: &ImageF32) -> ImageBuffer<Rgb<u8>, Vec<u8>> {
    let mut buffer: ImageBuffer<Rgb<u8>, Vec<u8>> =
        ImageBuffer::new(img.width, img.height);
    for (dst, src) in buffer.pixels_mut().zip(img.data.chunks_exact(CHANNELS)) {
        dst.0 = [
            (src[0].clamp(0.0, 1.0) * 255.0).round() as u8,
            (src[1].clamp(0.0, 1.0) * 255.0).round() as u8,
            (src[2].clamp(0.0, 1.0) * 255.0).round() as u8,
        ];
    }
    buffer
}

fn dynamic_to_f32(img: DynamicImage) -> ImageF32 {
    let width = img.width();
    let height = img.height();
    let mut data = vec![0.0f32; width as usize * height as usize * CHANNELS];

    match img {
        DynamicImage::ImageRgb8(buf) => {
            for (dst, px) in data.chunks_exact_mut(CHANNELS).zip(buf.pixels()) {
                dst[0] = px.0[0] as f32 / 255.0;
                dst[1] = px.0[1] as f32 / 255.0;
                dst[2] = px.0[2] as f32 / 255.0;
            }
        }
        DynamicImage::ImageRgba8(buf) => {
            for (dst, px) in data.chunks_exact_mut(CHANNELS).zip(buf.pixels()) {
                dst[0] = px.0[0] as f32 / 255.0;
                dst[1] = px.0[1] as f32 / 255.0;
                dst[2] = px.0[2] as f32 / 255.0;
            }
        }
        DynamicImage::ImageLuma8(buf) => {
            for (dst, px) in data.chunks_exact_mut(CHANNELS).zip(buf.pixels()) {
                let v = px.0[0] as f32 / 255.0;
                dst[0] = v;
                dst[1] = v;
                dst[2] = v;
            }
        }
        DynamicImage::ImageLumaA8(buf) => {
            for (dst, px) in data.chunks_exact_mut(CHANNELS).zip(buf.pixels()) {
                let v = px.0[0] as f32 / 255.0;
                dst[0] = v;
                dst[1] = v;
                dst[2] = v;
            }
        }
        DynamicImage::ImageRgb16(buf) => {
            for (dst, px) in data.chunks_exact_mut(CHANNELS).zip(buf.pixels()) {
                dst[0] = px.0[0] as f32 / 65535.0;
                dst[1] = px.0[1] as f32 / 65535.0;
                dst[2] = px.0[2] as f32 / 65535.0;
            }
        }
        DynamicImage::ImageRgba16(buf) => {
            for (dst, px) in data.chunks_exact_mut(CHANNELS).zip(buf.pixels()) {
                dst[0] = px.0[0] as f32 / 65535.0;
                dst[1] = px.0[1] as f32 / 65535.0;
                dst[2] = px.0[2] as f32 / 65535.0;
            }
        }
        DynamicImage::ImageLuma16(buf) => {
            for (dst, px) in data.chunks_exact_mut(CHANNELS).zip(buf.pixels()) {
                let v = px.0[0] as f32 / 65535.0;
                dst[0] = v;
                dst[1] = v;
                dst[2] = v;
            }
        }
        DynamicImage::ImageLumaA16(buf) => {
            for (dst, px) in data.chunks_exact_mut(CHANNELS).zip(buf.pixels()) {
                let v = px.0[0] as f32 / 65535.0;
                dst[0] = v;
                dst[1] = v;
                dst[2] = v;
            }
        }
        DynamicImage::ImageRgb32F(buf) => {
            for (dst, px) in data.chunks_exact_mut(CHANNELS).zip(buf.pixels()) {
                dst[0] = px.0[0];
                dst[1] = px.0[1];
                dst[2] = px.0[2];
            }
        }
        DynamicImage::ImageRgba32F(buf) => {
            for (dst, px) in data.chunks_exact_mut(CHANNELS).zip(buf.pixels()) {
                dst[0] = px.0[0];
                dst[1] = px.0[1];
                dst[2] = px.0[2];
            }
        }
        other => {
            let rgb = other.to_rgb8();
            for (dst, px) in data.chunks_exact_mut(CHANNELS).zip(rgb.pixels()) {
                dst[0] = px.0[0] as f32 / 255.0;
                dst[1] = px.0[1] as f32 / 255.0;
                dst[2] = px.0[2] as f32 / 255.0;
            }
        }
    }

    ImageF32 {
        width,
        height,
        data,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_path(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        std::env::temp_dir().join(format!("sepiola-test-{nanos}-{name}"))
    }

    fn gradient(width: u32, height: u32) -> ImageF32 {
        let mut img = ImageF32::new(width, height).expect("valid size");
        for y in 0..height {
            for x in 0..width {
                let idx = (y as usize * width as usize + x as usize) * CHANNELS;
                img.data[idx] = x as f32 / (width - 1) as f32;
                img.data[idx + 1] = y as f32 / (height - 1) as f32;
                img.data[idx + 2] = 0.5;
            }
        }
        img
    }

    #[test]
    fn new_rejects_empty_size() {
        assert!(ImageF32::new(0, 4).is_err());
        assert!(ImageF32::new(4, 0).is_err());
    }

    #[test]
    fn from_data_checks_length() {
        assert!(ImageF32::from_data(2, 2, vec![0.0; 12]).is_ok());
        assert!(ImageF32::from_data(2, 2, vec![0.0; 11]).is_err());
    }

    #[test]
    fn tiff16_roundtrip_within_quantization_error() {
        let img = gradient(16, 8);
        let path = temp_path("roundtrip.tif");
        save_tiff16(&img, &path).expect("save tiff");
        let loaded = load(&path).expect("load tiff");
        assert_eq!(loaded.width, img.width);
        assert_eq!(loaded.height, img.height);
        for (a, b) in img.data.iter().zip(loaded.data.iter()) {
            let err = (a - b).abs();
            assert!(err <= 1.0 / 255.0 + 1e-6, "error {err} exceeds 1/255");
        }
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn jpeg_roundtrip_is_close() {
        let img = gradient(32, 32);
        let path = temp_path("roundtrip.jpg");
        save_jpeg(&img, &path, 95).expect("save jpeg");
        let loaded = load(&path).expect("load jpeg");
        assert_eq!(loaded.width, img.width);
        assert_eq!(loaded.height, img.height);
        for (a, b) in img.data.iter().zip(loaded.data.iter()) {
            assert!((a - b).abs() <= 0.05, "jpeg error too large: {a} vs {b}");
        }
        let _ = std::fs::remove_file(&path);
    }

    /// 构造一个最小的 APP1（Exif）段字节序列。
    fn synthetic_app1() -> Vec<u8> {
        vec![0xFF, 0xE1, 0x00, 0x08, b'E', b'x', b'i', b'f', 0x00, 0x00]
    }

    /// 把一段字节插入到 JPEG 文件 SOI 之后。
    fn inject_after_soi(path: &Path, segment: &[u8]) {
        let mut bytes = std::fs::read(path).expect("read jpeg");
        bytes.splice(2..2, segment.iter().copied());
        std::fs::write(path, bytes).expect("write jpeg");
    }

    fn contains_app1(path: &Path) -> bool {
        let bytes = std::fs::read(path).expect("read jpeg");
        !jpeg_meta_ranges(&bytes).is_empty()
    }

    #[test]
    fn jpeg_metadata_copy_and_strip() {
        let src = temp_path("meta-src.jpg");
        let dst = temp_path("meta-dst.jpg");
        let img = gradient(16, 16);
        save_jpeg(&img, &src, 90).expect("save src");
        save_jpeg(&img, &dst, 90).expect("save dst");

        inject_after_soi(&src, &synthetic_app1());
        assert!(contains_app1(&src));

        jpeg_copy_metadata(&src, &dst).expect("copy metadata");
        assert!(contains_app1(&dst));
        // 复制后仍可解码，尺寸不变。
        let loaded = load(&dst).expect("load dst after copy");
        assert_eq!((loaded.width, loaded.height), (16, 16));

        jpeg_strip_metadata(&dst).expect("strip metadata");
        assert!(!contains_app1(&dst));
        let loaded = load(&dst).expect("load dst after strip");
        assert_eq!((loaded.width, loaded.height), (16, 16));

        let _ = std::fs::remove_file(&src);
        let _ = std::fs::remove_file(&dst);
    }

    #[test]
    fn strip_on_clean_jpeg_is_noop() {
        let path = temp_path("clean.jpg");
        let img = gradient(8, 8);
        save_jpeg(&img, &path, 90).expect("save");
        let before = std::fs::read(&path).expect("read");
        jpeg_strip_metadata(&path).expect("strip");
        let after = std::fs::read(&path).expect("read");
        assert_eq!(before, after);
        let _ = std::fs::remove_file(&path);
    }
}
