//! 结果与人物操作：删除未保留、打开输出位置、重命名/合并/删除人物。

use std::path::{Path, PathBuf};

use sepiola::config::Config;
use sepiola::identity::{self, Gallery};
use sepiola::paths::resolve_asset;

use crate::state::SepiolaApp;

impl SepiolaApp {
    pub fn gallery_path(&self) -> PathBuf {
        resolve_asset(self.gallery_file.trim())
    }

    pub fn out_dir(&self) -> PathBuf {
        Path::new(self.output_dir.trim()).to_path_buf()
    }

    /// 保存当前界面参数到 TOML（含输入/输出目录）。
    pub fn save_config_to(&mut self, path: &Path) {
        let config = match self.build_config() {
            Ok(config) => config,
            Err(error) => {
                self.message = format!("配置有误，未保存：{error}");
                return;
            }
        };
        match config.save_file(path) {
            Ok(()) => self.message = format!("参数已保存到 {}", path.display()),
            Err(error) => self.message = format!("保存失败：{error}"),
        }
    }

    /// 从 TOML 载入参数并回填界面（输入/输出目录为空时保留当前选择）。
    pub fn load_config_from(&mut self, path: &Path) {
        match Config::load_file(path) {
            Ok(config) => {
                self.apply_config(&config);
                self.message = format!("已载入参数 {}", path.display());
            }
            Err(error) => self.message = format!("载入失败：{error}"),
        }
    }

    /// 处理窗口拖入的路径：目录作为输入，文件取其父目录。
    pub fn accept_dropped_paths(&mut self, paths: &[PathBuf]) {
        let Some(first) = paths.first() else {
            return;
        };
        let dir = if first.is_dir() {
            first.clone()
        } else {
            match first.parent() {
                Some(parent) => parent.to_path_buf(),
                None => return,
            }
        };
        self.input_dir = dir.display().to_string();
        if self.output_dir.trim().is_empty() {
            self.output_dir = dir.join("out").display().to_string();
        }
        self.message = format!("输入目录已设为 {}", dir.display());
    }

    /// 删除所有未勾选保留的成功输出（含对比图）。
    pub fn delete_unkept(&mut self) {
        let mut removed = 0;
        for row in &mut self.rows {
            if row.ok && !row.kept && !row.deleted {
                let _ = std::fs::remove_file(&row.output);
                let _ = std::fs::remove_file(comparison_path(&row.output));
                row.deleted = true;
                removed += 1;
            }
        }
        self.message = format!("已删除 {removed} 张未保留的输出");
    }

    pub fn set_all_kept(&mut self, kept: bool) {
        for row in &mut self.rows {
            if !row.deleted {
                row.kept = kept;
            }
        }
    }

    /// 打开系统文件管理器定位输出目录。
    pub fn open_output_location(&mut self) {
        let dir = self.out_dir();
        if dir.as_os_str().is_empty() {
            self.message = "请先设置输出目录".to_string();
            return;
        }
        if let Err(error) = std::fs::create_dir_all(&dir) {
            self.message = format!("无法创建输出目录：{error}");
            return;
        }
        match open_in_file_manager(&dir) {
            Ok(()) => self.message = format!("已在文件管理器中打开 {}", dir.display()),
            Err(error) => self.message = format!("打开输出目录失败：{error}"),
        }
    }

    /// 把第 `index` 行的人物标签重命名为 `new`：更新图库、目录与所有行。
    pub fn rename_person(&mut self, index: usize, new: &str) {
        let Some(old) = self.rows.get(index).and_then(|row| row.person.clone()) else {
            self.message = "该行没有人物标签".to_string();
            return;
        };
        let Some(new) = identity::sanitize_label(new) else {
            self.message = "标签不能为空或包含路径分隔符".to_string();
            return;
        };
        if new == old {
            return;
        }

        // 更新图库 JSON（文件不存在或解析失败时跳过，仅改目录与行）。
        let gallery_path = self.gallery_path();
        match Gallery::load(&gallery_path) {
            Ok(mut gallery) => {
                if gallery.rename(&old, &new) {
                    if let Err(error) = gallery.save(&gallery_path) {
                        self.message = format!("图库保存失败：{error}");
                    }
                }
            }
            Err(error) => self.message = format!("图库读取失败：{error}"),
        }

        // 重命名输出目录 out/<old> → out/<new>。
        let out_dir = self.out_dir();
        let old_dir = out_dir.join(&old);
        let new_dir = out_dir.join(&new);
        if old_dir.is_dir() && !new_dir.exists() {
            if let Err(error) = std::fs::rename(&old_dir, &new_dir) {
                self.message = format!("目录重命名失败：{error}");
            }
        }

        for row in &mut self.rows {
            if row.person.as_deref() == Some(old.as_str()) {
                row.person = Some(new.clone());
                row.output = replace_label(&row.output, &old, &new);
            }
        }
        self.message = format!("人物标签 {old} → {new}");
    }

    /// 把第 `index` 行的人物并入 `target`：图库 merge + 输出目录合并 + 行同步。
    pub fn merge_person(&mut self, index: usize, target: &str) {
        let Some(src) = self.rows.get(index).and_then(|row| row.person.clone()) else {
            self.message = "该行没有人物标签".to_string();
            return;
        };
        let Some(target) = identity::sanitize_label(target) else {
            self.message = "合并目标标签无效".to_string();
            return;
        };
        if target == src {
            return;
        }

        let gallery_path = self.gallery_path();
        match Gallery::load(&gallery_path) {
            Ok(mut gallery) => {
                if !gallery.merge(&src, &target) {
                    self.message = format!("图库中不存在 {src} 或 {target}，合并已取消");
                    return;
                }
                if let Err(error) = gallery.save(&gallery_path) {
                    self.message = format!("图库保存失败：{error}");
                    return;
                }
            }
            Err(error) => {
                self.message = format!("图库读取失败：{error}");
                return;
            }
        }

        let out_dir = self.out_dir();
        let mut moved = 0usize;
        if !out_dir.as_os_str().is_empty() {
            match identity::merge_person_dirs(&out_dir, &src, &target) {
                Ok(count) => moved = count,
                Err(error) => self.message = format!("目录合并失败：{error}"),
            }
        }

        for row in &mut self.rows {
            if row.person.as_deref() == Some(src.as_str()) {
                row.person = Some(target.clone());
                row.output = replace_label(&row.output, &src, &target);
            }
        }
        if !self.message.starts_with("目录合并失败") {
            self.message = format!("已把 {src} 并入 {target}，移动 {moved} 个文件");
        }
    }

    /// 删除第 `index` 行对应的人物；`delete_dir` 时同时删除其输出子目录。
    pub fn remove_person(&mut self, index: usize, delete_dir: bool) {
        let Some(label) = self.rows.get(index).and_then(|row| row.person.clone()) else {
            self.message = "该行没有人物标签".to_string();
            return;
        };

        let gallery_path = self.gallery_path();
        match Gallery::load(&gallery_path) {
            Ok(mut gallery) => {
                if !gallery.remove(&label) {
                    self.message = format!("图库中不存在 {label}");
                    return;
                }
                if let Err(error) = gallery.save(&gallery_path) {
                    self.message = format!("图库保存失败：{error}");
                    return;
                }
            }
            Err(error) => {
                self.message = format!("图库读取失败：{error}");
                return;
            }
        }

        let mut dir_removed = false;
        if delete_dir {
            let out_dir = self.out_dir();
            if !out_dir.as_os_str().is_empty() {
                match identity::remove_person_dir(&out_dir, &label) {
                    Ok(true) => dir_removed = true,
                    Ok(false) => {}
                    Err(error) => {
                        self.message = format!("删除输出目录失败：{error}");
                        return;
                    }
                }
            }
        }

        for row in &mut self.rows {
            if row.person.as_deref() == Some(label.as_str()) {
                row.person = None;
                if dir_removed {
                    row.kept = false;
                    row.deleted = true;
                }
            }
        }
        self.message = if dir_removed {
            format!("已删除人物 {label} 及其输出目录")
        } else {
            format!("已从图库删除人物 {label}（输出文件保留）")
        };
    }
}

pub fn comparison_path(output: &Path) -> PathBuf {
    let stem = output
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_default();
    output.with_file_name(format!("{stem}_compare.jpg"))
}

/// 替换路径中第一次出现的标签目录名。
fn replace_label(path: &Path, old: &str, new: &str) -> PathBuf {
    let mut out = PathBuf::new();
    let mut replaced = false;
    for component in path.components() {
        if !replaced && component.as_os_str() == old {
            out.push(new);
            replaced = true;
        } else {
            out.push(component.as_os_str());
        }
    }
    out
}

/// 调用系统文件管理器打开目录。
pub fn open_in_file_manager(dir: &Path) -> std::io::Result<()> {
    #[cfg(target_os = "macos")]
    let mut command = std::process::Command::new("open");
    #[cfg(windows)]
    let mut command = std::process::Command::new("explorer");
    #[cfg(all(unix, not(target_os = "macos")))]
    let mut command = std::process::Command::new("xdg-open");
    command.arg(dir).spawn()?;
    Ok(())
}
