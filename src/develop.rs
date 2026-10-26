//! RAW 显影：默认调用外部 `darktable-cli`，也可切换到内置后端。

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::config::DevelopSettings;
use crate::error::{Error, Result};

/// darktable 的导出插件通过配置文件决定 TIFF 位深，CLI 的 `--bpp`
/// 在当前版本不受支持，所以预置一份 `darktablerc`。
const DARKTABLE_RC: &str = "plugins/imageio/format/tiff/bpp=16\n";

/// 需要走 RAW 解码器的扩展名。
const RAW_EXTENSIONS: &[&str] = &[
    "dng", "cr2", "cr3", "nef", "nrw", "arw", "srf", "sr2", "raf", "orf", "rw2", "pef", "srw", "3fr",
    "fff", "iiq", "kdc", "mef", "mos", "mrw", "x3f", "erf",
];

/// 是否需要 RAW 解码（而非通用图片解码）。
pub fn is_raw_format(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| RAW_EXTENSIONS.iter().any(|known| ext.eq_ignore_ascii_case(known)))
        .unwrap_or(false)
}

/// 实际使用的显影后端。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Backend {
    Darktable,
    Builtin,
}

impl Backend {
    /// 按配置选择后端；`auto` 优先 darktable，缺失时回退内置后端。
    pub fn from_settings(settings: &DevelopSettings) -> Result<Self> {
        match settings.backend.as_str() {
            "darktable" => Ok(Self::Darktable),
            "builtin" => Ok(Self::Builtin),
            "auto" => {
                if darktable_available() {
                    tracing::info!("develop backend: darktable");
                    Ok(Self::Darktable)
                } else {
                    tracing::warn!(
                        "darktable-cli not found, falling back to builtin develop backend"
                    );
                    Ok(Self::Builtin)
                }
            }
            other => Err(Error::Config(format!(
                "develop.backend must be 'auto', 'darktable' or 'builtin', got '{other}'"
            ))),
        }
    }
}

/// `darktable-cli` 是否可执行。
pub fn darktable_available() -> bool {
    Command::new("darktable-cli")
        .arg("--version")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

static INSTANCE_COUNTER: AtomicU64 = AtomicU64::new(0);

/// 将输入显影为 16-bit TIFF。
///
/// 输出文件为 `out_dir/<stem>.tif`。darktable 后端每次调用使用独立的
/// 配置目录，避免并行实例互相争用数据库。
pub fn raw_to_tiff(raw: &Path, out_dir: &Path, backend: Backend) -> Result<PathBuf> {
    if !raw.is_file() {
        return Err(Error::Develop(format!(
            "input file does not exist: {}",
            raw.display()
        )));
    }
    std::fs::create_dir_all(out_dir)?;

    let stem = raw
        .file_stem()
        .ok_or_else(|| Error::Develop(format!("input has no file stem: {}", raw.display())))?
        .to_string_lossy()
        .into_owned();
    let out_path = out_dir.join(format!("{stem}.tif"));
    if out_path.exists() {
        std::fs::remove_file(&out_path)?;
    }

    if backend == Backend::Builtin {
        crate::builtin_develop::develop_to_tiff(raw, &out_path)?;
        return Ok(out_path);
    }

    let config_dir = unique_config_dir(out_dir, &stem);
    std::fs::create_dir_all(&config_dir)?;
    std::fs::write(config_dir.join("darktablerc"), DARKTABLE_RC)?;

    let output = Command::new("darktable-cli")
        .arg(raw)
        .arg(&out_path)
        .arg("--out-ext")
        .arg("tif")
        .arg("--apply-custom-presets")
        .arg("false")
        .arg("--core")
        .arg("--configdir")
        .arg(&config_dir)
        .output()
        .map_err(|e| Error::Develop(format!("failed to run darktable-cli: {e}")));

    let _ = std::fs::remove_dir_all(&config_dir);
    let output = output?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        return Err(Error::Develop(format!(
            "darktable-cli exited with {} for {}: {stderr}{stdout}",
            output.status,
            raw.display()
        )));
    }
    if !out_path.is_file() {
        return Err(Error::Develop(format!(
            "darktable-cli reported success but did not create {}",
            out_path.display()
        )));
    }
    Ok(out_path)
}

fn unique_config_dir(out_dir: &Path, stem: &str) -> PathBuf {
    let instance = INSTANCE_COUNTER.fetch_add(1, Ordering::Relaxed);
    out_dir.join(".darktable").join(format!(
        "{}-{instance}-{stem}",
        std::process::id()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::imageio::{save_jpeg, ImageF32};

    fn darktable_available() -> bool {
        Command::new("darktable-cli")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    #[test]
    fn missing_input_reports_develop_error() {
        let path = std::env::temp_dir().join("sepiola-missing-input.dng");
        let out_dir = std::env::temp_dir().join("sepiola-missing-out");
        match raw_to_tiff(&path, &out_dir, Backend::Builtin) {
            Err(Error::Develop(message)) => {
                assert!(message.contains("does not exist"), "{message}");
            }
            other => panic!("expected Develop error, got {other:?}"),
        }
    }

    #[test]
    fn converts_image_to_16bit_tiff() {
        if !darktable_available() {
            eprintln!("darktable-cli not available, skipping");
            return;
        }

        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!("sepiola-develop-{nanos}"));
        std::fs::create_dir_all(&dir).expect("create temp dir");

        let input = dir.join("input.jpg");
        let mut img = ImageF32::new(16, 12).expect("valid size");
        for (i, value) in img.data.iter_mut().enumerate() {
            *value = (i % 17) as f32 / 16.0;
        }
        save_jpeg(&img, &input, 95).expect("save jpeg");

        let tiff = raw_to_tiff(&input, &dir, Backend::Darktable).expect("convert");
        assert!(tiff.is_file());
        let loaded = crate::imageio::load(&tiff).expect("load tiff");
        assert_eq!(loaded.width, 16);
        assert_eq!(loaded.height, 12);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn backend_selection_follows_settings() {
        let mut settings = DevelopSettings::default();
        assert_eq!(settings.backend, "auto");

        settings.backend = "builtin".to_string();
        assert_eq!(
            Backend::from_settings(&settings).expect("builtin"),
            Backend::Builtin
        );

        settings.backend = "darktable".to_string();
        assert_eq!(
            Backend::from_settings(&settings).expect("darktable"),
            Backend::Darktable
        );

        settings.backend = "auto".to_string();
        assert!(Backend::from_settings(&settings).is_ok());

        settings.backend = "unknown".to_string();
        assert!(matches!(
            Backend::from_settings(&settings),
            Err(Error::Config(_))
        ));
    }

    #[test]
    fn raw_extension_detection_is_case_insensitive() {
        assert!(is_raw_format(Path::new("a.CR2")));
        assert!(is_raw_format(Path::new("b.dng")));
        assert!(!is_raw_format(Path::new("c.jpg")));
        assert!(!is_raw_format(Path::new("noext")));
    }
}
