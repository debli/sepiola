use std::path::{Path, PathBuf};

/// 解析配置中的外部资源路径（模型、LUT 等）：绝对路径原样返回；
/// 相对路径优先取可执行文件同目录下的文件，其次回退到相对当前工作目录的路径。
pub fn resolve_asset(configured: &str) -> PathBuf {
    let path = Path::new(configured);
    if path.is_absolute() {
        return path.to_path_buf();
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let candidate = dir.join(path);
            if candidate.is_file() {
                return candidate;
            }
        }
    }
    path.to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_asset_prefers_executable_directory() {
        let exe = std::env::current_exe().expect("current exe");
        let dir = exe.parent().expect("exe dir");
        let relative = format!("sepiola-asset-test-{}.cube", std::process::id());
        let file = dir.join(&relative);
        std::fs::write(&file, b"x").expect("write asset");
        assert_eq!(resolve_asset(&relative), file);
        let _ = std::fs::remove_file(&file);
    }

    #[test]
    fn absolute_asset_is_unchanged() {
        let path = std::env::temp_dir().join("sepiola-absolute.cube");
        let text = path.to_string_lossy().into_owned();
        assert_eq!(resolve_asset(&text), path);
    }

    #[test]
    fn missing_relative_asset_falls_back_to_working_directory() {
        let relative = "sepiola-missing-asset-dir/lut.cube";
        assert_eq!(resolve_asset(relative), PathBuf::from(relative));
    }
}
