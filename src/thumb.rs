//! 缩略图磁盘缓存：`<库根>/.sepiola/thumbs/{max_side}/{hash}.jpg`。
//!
//! 文件名用 FNV-1a 64 对相对路径字节做哈希，保证跨进程稳定
//! （`DefaultHasher` 的种子每进程随机，不能用于缓存键）。

use std::io::Cursor;
use std::path::{Path, PathBuf};

use image::ImageOutputFormat;

use crate::error::{Error, Result};
use crate::library::LIBRARY_DIR;

/// 缓存目录：`<root>/.sepiola/thumbs`。
pub fn thumbs_dir(root: &Path) -> PathBuf {
    root.join(LIBRARY_DIR).join("thumbs")
}

/// 缩略图缓存路径：按尺寸分桶，文件名为相对路径的 FNV-1a 64 哈希。
pub fn thumb_path(root: &Path, rel: &Path, max_side: u32) -> PathBuf {
    thumbs_dir(root)
        .join(max_side.to_string())
        .join(format!("{:016x}.jpg", fnv1a64(rel.to_string_lossy().as_bytes())))
}

/// FNV-1a 64 位哈希（offset basis / prime 为公开常量）。
fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// 生成（或复用）`rel` 的缩略图。
///
/// 缓存文件存在且 mtime 不早于源文件时直接复用；
/// 源文件缺失或无法解码（RAW/损坏）返回 `Ok(None)`，不落盘错误缓存。
pub fn generate(root: &Path, rel: &Path, max_side: u32) -> Result<Option<PathBuf>> {
    let src = root.join(rel);
    let out = thumb_path(root, rel, max_side);
    let src_meta = match std::fs::metadata(&src) {
        Ok(meta) => meta,
        Err(_) => return Ok(None),
    };
    if let Ok(out_meta) = std::fs::metadata(&out) {
        let fresh = matches!(
            (out_meta.modified(), src_meta.modified()),
            (Ok(out_t), Ok(src_t)) if out_t >= src_t
        );
        if fresh {
            return Ok(Some(out));
        }
    }
    let decoded = crate::imageio::load_dynamic(&src).or_else(|_| {
        // RAW 等不可直接解码的源：若已一键显影则用产物。
        crate::imageio::load_dynamic(&crate::library::developed_path(root, rel))
    });
    let Ok(decoded) = decoded else {
        return Ok(None);
    };
    let rgb = decoded.to_rgb8();
    let small = image::imageops::thumbnail(&rgb, max_side, max_side);
    let mut buffer = Vec::new();
    image::DynamicImage::ImageRgb8(small)
        .write_to(&mut Cursor::new(&mut buffer), ImageOutputFormat::Jpeg(80))
        .map_err(|e| Error::Image(format!("encode thumbnail {}: {e}", rel.display())))?;
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&out, buffer)?;
    Ok(Some(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!("sepiola-thumb-{tag}-{nanos}"));
        std::fs::create_dir_all(&dir).expect("create temp root");
        dir
    }

    #[test]
    fn hash_is_stable_and_path_bucked_by_size() {
        let root = Path::new("/lib");
        let p1 = thumb_path(root, Path::new("a/b.jpg"), 128);
        let p2 = thumb_path(root, Path::new("a/b.jpg"), 128);
        assert_eq!(p1, p2);
        assert!(p1.starts_with(thumbs_dir(root).join("128")));
        assert_ne!(p1, thumb_path(root, Path::new("a/b.jpg"), 64));
        assert_ne!(p1, thumb_path(root, Path::new("a/c.jpg"), 128));
    }

    #[test]
    fn generate_creates_and_reuses_cache() {
        let root = temp_root("gen");
        let img = image::RgbImage::from_pixel(200, 120, image::Rgb([255, 0, 0]));
        img.save(root.join("a.jpg")).expect("save");

        let first = generate(&root, Path::new("a.jpg"), 64).expect("generate");
        let out = first.expect("some");
        assert!(out.is_file());
        let thumb = image::open(&out).expect("open thumb");
        assert!(thumb.width() <= 64 && thumb.height() <= 64);

        let mtime = std::fs::metadata(&out).expect("meta").modified().expect("mtime");
        let again = generate(&root, Path::new("a.jpg"), 64).expect("regenerate");
        assert_eq!(again.expect("path"), out);
        let after = std::fs::metadata(&out).expect("meta").modified().expect("mtime");
        assert_eq!(mtime, after, "缓存新鲜时不应重写");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn undecodable_source_returns_none() {
        let root = temp_root("bad");
        std::fs::write(root.join("raw.cr2"), b"not an image").expect("write");
        assert!(generate(&root, Path::new("raw.cr2"), 64).expect("generate").is_none());
        assert!(generate(&root, Path::new("missing.jpg"), 64).expect("generate").is_none());
        std::fs::remove_dir_all(&root).ok();
    }
}
