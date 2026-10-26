//! 对比图与 CSV 处理日志。

use std::path::Path;

use crate::error::{Error, Result};
use crate::imageio::{self, ImageF32, CHANNELS};

/// 对比图中间分隔条的宽度（像素）。
const GAP: u32 = 8;

/// 生成「原图 | 结果」左右对比图并保存为 JPEG。
pub fn save_comparison(
    original: &ImageF32,
    graded: &ImageF32,
    path: &Path,
    quality: u8,
) -> Result<()> {
    let height = original.height.max(graded.height);
    if height == 0 {
        return Err(Error::Image("cannot build comparison from empty image".to_string()));
    }
    let width = original.width + GAP + graded.width;
    let mut out = ImageF32::new(width, height)?;

    copy_into(&mut out, original, 0, 0);
    copy_into(&mut out, graded, original.width + GAP, 0);
    imageio::save_jpeg(&out, path, quality)
}

fn copy_into(target: &mut ImageF32, source: &ImageF32, offset_x: u32, offset_y: u32) {
    let copy_width = source.width.min(target.width.saturating_sub(offset_x));
    let copy_height = source.height.min(target.height.saturating_sub(offset_y));
    for y in 0..copy_height as usize {
        let src_start = y * source.width as usize * CHANNELS;
        let src_end = src_start + copy_width as usize * CHANNELS;
        let dst_start = ((offset_y as usize + y) * target.width as usize + offset_x as usize)
            * CHANNELS;
        let dst_end = dst_start + copy_width as usize * CHANNELS;
        target.data[dst_start..dst_end].copy_from_slice(&source.data[src_start..src_end]);
    }
}

/// CSV 中的一行处理记录。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CsvRow {
    pub file: String,
    pub scene: String,
    /// 人物标签；无人物时为空。
    pub person: String,
    pub stages: String,
    pub clipped_high_before: f32,
    pub clipped_high_after: f32,
    pub retried: bool,
    pub status: String,
    pub error: String,
}

/// 写出 CSV 日志（覆盖旧文件）。
pub fn write_csv(path: &Path, rows: &[CsvRow]) -> Result<()> {
    let mut text = String::from(
        "file,scene,person,stages,clipped_high_before,clipped_high_after,retried,status,error\n",
    );
    for row in rows {
        text.push_str(&escape(&row.file));
        text.push(',');
        text.push_str(&escape(&row.scene));
        text.push(',');
        text.push_str(&escape(&row.person));
        text.push(',');
        text.push_str(&escape(&row.stages));
        text.push(',');
        text.push_str(&format!("{:.4}", row.clipped_high_before));
        text.push(',');
        text.push_str(&format!("{:.4}", row.clipped_high_after));
        text.push(',');
        text.push_str(if row.retried { "true" } else { "false" });
        text.push(',');
        text.push_str(&escape(&row.status));
        text.push(',');
        text.push_str(&escape(&row.error));
        text.push('\n');
    }
    std::fs::write(path, text)?;
    Ok(())
}

fn escape(value: &str) -> String {
    if value.contains(',') || value.contains('"') || value.contains('\n') || value.contains('\r') {
        let mut escaped = String::with_capacity(value.len() + 2);
        escaped.push('"');
        for ch in value.chars() {
            if ch == '"' {
                escaped.push('"');
            }
            escaped.push(ch);
        }
        escaped.push('"');
        escaped
    } else {
        value.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_path(name: &str) -> std::path::PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        std::env::temp_dir().join(format!("sepiola-report-{nanos}-{name}"))
    }

    #[test]
    fn comparison_has_expected_dimensions() {
        let original = ImageF32::from_data(4, 3, vec![0.1; 4 * 3 * CHANNELS]).expect("valid");
        let graded = ImageF32::from_data(4, 3, vec![0.9; 4 * 3 * CHANNELS]).expect("valid");
        let path = temp_path("compare.jpg");
        save_comparison(&original, &graded, &path, 95).expect("save comparison");
        let loaded = crate::imageio::load(&path).expect("load comparison");
        assert_eq!(loaded.width, 4 + GAP + 4);
        assert_eq!(loaded.height, 3);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn different_heights_use_max_height() {
        let original = ImageF32::from_data(2, 2, vec![0.5; 2 * 2 * CHANNELS]).expect("valid");
        let graded = ImageF32::from_data(2, 4, vec![0.5; 2 * 4 * CHANNELS]).expect("valid");
        let path = temp_path("compare-height.jpg");
        save_comparison(&original, &graded, &path, 90).expect("save comparison");
        let loaded = crate::imageio::load(&path).expect("load");
        assert_eq!(loaded.height, 4);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn csv_writes_header_and_escapes_fields() {
        let path = temp_path("log.csv");
        let rows = vec![CsvRow {
            file: "a,b.jpg".to_string(),
            scene: "portrait".to_string(),
            person: "张三".to_string(),
            stages: "lut".to_string(),
            clipped_high_before: 0.01,
            clipped_high_after: 0.02,
            retried: true,
            status: "ok".to_string(),
            error: "he said \"hi\"".to_string(),
        }];
        write_csv(&path, &rows).expect("write csv");
        let text = std::fs::read_to_string(&path).expect("read csv");
        assert!(text.starts_with("file,scene,person,stages,"));
        assert!(text.contains("\"a,b.jpg\""));
        assert!(text.contains("\"he said \"\"hi\"\"\""));
        assert!(text.contains(",true,"));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn csv_with_no_rows_still_has_header() {
        let path = temp_path("empty.csv");
        write_csv(&path, &[]).expect("write csv");
        let text = std::fs::read_to_string(&path).expect("read csv");
        assert_eq!(text.lines().count(), 1);
        let _ = std::fs::remove_file(&path);
    }
}
