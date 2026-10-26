//! 共享小工具：模型发现、中文字体、场景配置与通用选择行控件。

use std::path::Path;

use eframe::egui;
use sepiola::config::{LutConfig, SceneConfig};

/// 按 LUT 路径与强度生成场景配置；路径为空时返回默认（无 Stage）。
pub fn scene_config(path: &str, strength: f32) -> SceneConfig {
    let path = path.trim();
    if path.is_empty() {
        SceneConfig::default()
    } else {
        SceneConfig {
            stages: vec!["lut".to_string()],
            lut: Some(LutConfig {
                file: path.to_string(),
                strength,
            }),
        }
    }
}

/// 查找 ONNX 模型：从当前工作目录与可执行文件目录逐级向上，命中即返回绝对路径。
/// GUI 可能从项目子目录或 `target/debug` 启动，仅按工作目录相对路径会找不到模型。
pub fn discover_model(default: &str, hint: &str) -> String {
    let mut roots = Vec::new();
    if let Ok(cwd) = std::env::current_dir() {
        roots.push(cwd);
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            roots.push(dir.to_path_buf());
        }
    }
    for root in roots {
        for ancestor in root.ancestors() {
            let candidate = ancestor.join(default);
            if candidate.is_file() {
                return candidate.display().to_string();
            }
        }
    }
    let _ = hint;
    default.to_string()
}

/// egui 默认字体不含中文，启动时加载系统中文字体作为回退。
pub fn install_cjk_font(ctx: &egui::Context) {
    const CANDIDATES: &[&str] = &[
        "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/truetype/wqy/wqy-microhei.ttc",
        "/System/Library/Fonts/PingFang.ttc",
        "C:/Windows/Fonts/msyh.ttc",
    ];
    for path in CANDIDATES {
        let Ok(bytes) = std::fs::read(path) else {
            continue;
        };
        let mut fonts = egui::FontDefinitions::default();
        fonts
            .font_data
            .insert("cjk".to_owned(), egui::FontData::from_owned(bytes).into());
        for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
            fonts.families.entry(family).or_default().push("cjk".to_owned());
        }
        ctx.set_fonts(fonts);
        return;
    }
}

/// 路径末段文件名（无文件名时回退整个路径）。
pub fn base_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

pub fn pointing_hand(response: egui::Response) -> egui::Response {
    if response.enabled() {
        response.on_hover_cursor(egui::CursorIcon::PointingHand)
    } else {
        response
    }
}

/// 一行「标签 + 文本框 + …目录选择」，返回选中的目录。
pub fn pick_dir(ui: &mut egui::Ui, label: &str, value: &mut String) -> Option<std::path::PathBuf> {
    let mut picked = None;
    ui.horizontal(|ui| {
        ui.label(label);
        ui.text_edit_singleline(value);
        if pointing_hand(ui.button("选择…")).clicked() {
            if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                *value = dir.display().to_string();
                picked = Some(dir);
            }
        }
    });
    picked
}

/// 一行「标签 + 文本框 + …文件选择」；`exts` 非空时提供文件过滤器。
pub fn pick_row(ui: &mut egui::Ui, label: &str, value: &mut String, exts: &[&str]) {
    ui.horizontal(|ui| {
        ui.label(label);
        ui.text_edit_singleline(value);
        if pointing_hand(ui.button("…")).clicked() {
            let mut dialog = rfd::FileDialog::new();
            if !exts.is_empty() {
                dialog = dialog.add_filter(exts.join(".").to_uppercase(), exts);
            }
            if let Some(file) = dialog.pick_file() {
                *value = file.display().to_string();
            }
        }
    });
}
