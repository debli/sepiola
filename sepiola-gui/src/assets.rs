//! 共享小工具：模型发现、中文字体、场景配置与通用选择行控件。

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use eframe::egui;
use sepiola::config::{LookConfig, LutConfig, SceneConfig};
use sepiola::grade::look_stage::{LookPreset, LookStage};
use sepiola::grade::{Context, Stage};
use sepiola::imageio::ImageF32;
use sepiola::scene::{SceneInfo, SceneKind};

pub struct LookPreview {
    pub preset: LookPreset,
    pub portrait: egui::TextureHandle,
    pub other: egui::TextureHandle,
}

pub fn build_look_previews(ctx: &egui::Context) -> Vec<LookPreview> {
    let portrait = load_preview(include_bytes!("../../test_data/portrait/women_6.jpg"));
    let other = load_preview(include_bytes!("../../test_data/landscape/pic_1043.jpg"));

    LookPreset::ALL
        .into_iter()
        .map(|preset| LookPreview {
            preset,
            portrait: render_look_preview(ctx, &portrait, preset, SceneKind::Portrait),
            other: render_look_preview(ctx, &other, preset, SceneKind::Other),
        })
        .collect()
}

fn load_preview(bytes: &[u8]) -> ImageF32 {
    let decoded = image::load_from_memory(bytes)
        .expect("bundled look preview should decode")
        .to_rgb8();
    let image = image::imageops::resize(&decoded, 144, 96, image::imageops::FilterType::Triangle);
    let data = image
        .as_raw()
        .iter()
        .map(|value| *value as f32 / 255.0)
        .collect();
    ImageF32::from_data(image.width(), image.height(), data).expect("valid preview image")
}

fn render_look_preview(
    ctx: &egui::Context,
    source: &ImageF32,
    preset: LookPreset,
    kind: SceneKind,
) -> egui::TextureHandle {
    let mut image = source.clone();
    let scene = SceneInfo::without_faces(kind);
    let stage_context = Context {
        scene: &scene,
        strength_scale: 1.0,
    };
    LookStage::new(preset, 1.0, preset.default_exposure_ev())
        .apply(&mut image, &stage_context)
        .expect("look preview should process");
    let pixels: Vec<u8> = image
        .data
        .iter()
        .map(|value| (value.clamp(0.0, 1.0) * 255.0).round() as u8)
        .collect();
    let texture =
        egui::ColorImage::from_rgb([image.width as usize, image.height as usize], &pixels);
    ctx.load_texture(
        format!("look-preview-{}-{}", kind_name(kind), preset.id()),
        texture,
        egui::TextureOptions::LINEAR,
    )
}

fn kind_name(kind: SceneKind) -> &'static str {
    match kind {
        SceneKind::Portrait => "portrait",
        SceneKind::Other => "other",
    }
}

/// 按 LUT 路径与强度生成场景配置；路径为空时返回默认（无 Stage）。
pub fn scene_config(
    path: &str,
    strength: f32,
    preset: &str,
    look_strength: f32,
    exposure_ev: f32,
) -> SceneConfig {
    let path = path.trim();
    let preset = preset.trim();
    let mut stages = Vec::new();
    let look = if preset.is_empty() {
        None
    } else {
        stages.push("look".to_string());
        Some(LookConfig {
            preset: preset.to_string(),
            strength: look_strength,
            exposure_ev,
        })
    };
    let lut = if path.is_empty() {
        None
    } else {
        stages.push("lut".to_string());
        Some(LutConfig {
            file: path.to_string(),
            strength,
        })
    };
    SceneConfig { stages, look, lut }
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
/// 同时注册阅读页用的粗体/斜体字族（找不到对应字体时退回普通字形）。
pub fn install_cjk_font(ctx: &egui::Context) {
    ctx.set_fonts(build_fonts(None));
}

/// 重新安装字体表；`custom` 可读时额外注册 `reader-custom` 字族供正文使用。
pub fn install_reader_fonts(ctx: &egui::Context, custom: Option<&Path>) {
    ctx.set_fonts(build_fonts(custom));
}

fn build_fonts(custom: Option<&Path>) -> egui::FontDefinitions {
    const CANDIDATES: &[&str] = &[
        "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/truetype/wqy/wqy-microhei.ttc",
        "/System/Library/Fonts/PingFang.ttc",
        "C:/Windows/Fonts/msyh.ttc",
    ];
    const BOLD_CANDIDATES: &[&str] = &[
        "/usr/share/fonts/opentype/noto/NotoSansCJK-Bold.ttc",
        "/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf",
        "/usr/share/fonts/truetype/liberation/LiberationSans-Bold.ttf",
        "C:/Windows/Fonts/msyhbd.ttc",
        "C:/Windows/Fonts/arialbd.ttf",
        "/System/Library/Fonts/Supplemental/Arial Bold.ttf",
    ];
    const ITALIC_CANDIDATES: &[&str] = &[
        "/usr/share/fonts/truetype/dejavu/DejaVuSans-Oblique.ttf",
        "/usr/share/fonts/truetype/liberation/LiberationSans-Italic.ttf",
        "C:/Windows/Fonts/ariali.ttf",
        "/System/Library/Fonts/Supplemental/Arial Italic.ttf",
    ];
    const SERIF_CANDIDATES: &[&str] = &[
        "/usr/share/fonts/opentype/noto/NotoSerifCJK-Regular.ttc",
        "/usr/share/fonts/truetype/arphic/uming.ttc",
        "C:/Windows/Fonts/simsun.ttc",
        "/System/Library/Fonts/Supplemental/Songti.ttc",
    ];
    const SERIF_BOLD_CANDIDATES: &[&str] = &["/usr/share/fonts/opentype/noto/NotoSerifCJK-Bold.ttc"];
    const KAI_CANDIDATES: &[&str] = &[
        "/usr/share/fonts/truetype/arphic-gkai00mp/gkai00mp.ttf",
        "/usr/share/fonts/truetype/arphic-bkai00mp/bkai00mp.ttf",
        "C:/Windows/Fonts/simkai.ttf",
        "/System/Library/Fonts/Supplemental/Kaiti.ttc",
    ];

    let mut fonts = egui::FontDefinitions::default();
    for path in CANDIDATES {
        let Ok(bytes) = std::fs::read(path) else {
            continue;
        };
        fonts
            .font_data
            .insert("cjk".to_owned(), egui::FontData::from_owned(bytes).into());
        for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
            fonts
                .families
                .entry(family)
                .or_default()
                .push("cjk".to_owned());
        }
        break;
    }
    // 粗体/斜体字族缺少字形时按普通字体的顺序回退，避免出现方块。
    let fallback = fonts
        .families
        .get(&egui::FontFamily::Proportional)
        .cloned()
        .unwrap_or_default();
    install_face(&mut fonts, "reader-bold", "reader-bold-font", BOLD_CANDIDATES, &fallback);
    install_face(
        &mut fonts,
        "reader-italic",
        "reader-italic-font",
        ITALIC_CANDIDATES,
        &fallback,
    );
    install_face(&mut fonts, "reader-serif", "reader-serif-font", SERIF_CANDIDATES, &fallback);
    // 衬线粗体优先沿用衬线字体链，避免宋体正文里出现无衬线粗体。
    let serif_chain = fonts
        .families
        .get(&egui::FontFamily::Name("reader-serif".into()))
        .cloned()
        .unwrap_or_else(|| fallback.clone());
    install_face(
        &mut fonts,
        "reader-serif-bold",
        "reader-serif-bold-font",
        SERIF_BOLD_CANDIDATES,
        &serif_chain,
    );
    install_face(&mut fonts, "reader-kai", "reader-kai-font", KAI_CANDIDATES, &fallback);
    // 用户选择的系统字体：缺字形时回退到普通字体链（含中文字体）。
    if let Some(path) = custom.filter(|path| path.is_file()) {
        if let Ok(bytes) = std::fs::read(path) {
            fonts.font_data.insert(
                "reader-custom-font".to_owned(),
                egui::FontData::from_owned(bytes).into(),
            );
            let mut chain = vec!["reader-custom-font".to_owned()];
            chain.extend(fallback.iter().cloned());
            fonts
                .families
                .insert(egui::FontFamily::Name("reader-custom".into()), chain);
        }
    }
    fonts
}

/// 系统可用字体（显示名 + 文件路径），按名称排序去重并缓存。
/// 优先使用 `fc-list`，不可用时扫描常见字体目录。
pub fn system_fonts() -> &'static Vec<(String, PathBuf)> {
    static FONTS: OnceLock<Vec<(String, PathBuf)>> = OnceLock::new();
    FONTS.get_or_init(|| {
        let mut fonts = fc_list_fonts();
        if fonts.is_empty() {
            fonts = scanned_fonts();
        }
        fonts.sort_by(|a, b| a.0.cmp(&b.0));
        fonts.dedup_by(|a, b| a.0 == b.0);
        fonts
    })
}

fn fc_list_fonts() -> Vec<(String, PathBuf)> {
    let Ok(output) = std::process::Command::new("fc-list")
        .args(["--format", "%{family[0]}\t%{file}\n"])
        .output()
    else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| {
            let (name, file) = line.split_once('\t')?;
            let (name, file) = (name.trim(), file.trim());
            (!name.is_empty() && !file.is_empty())
                .then(|| (name.to_owned(), PathBuf::from(file)))
        })
        .collect()
}

fn scanned_fonts() -> Vec<(String, PathBuf)> {
    let mut roots = vec![
        PathBuf::from("/usr/share/fonts"),
        PathBuf::from("/usr/local/share/fonts"),
        PathBuf::from("/System/Library/Fonts"),
        PathBuf::from("/Library/Fonts"),
        PathBuf::from("C:/Windows/Fonts"),
    ];
    if let Some(home) = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
    {
        let home = PathBuf::from(home);
        roots.push(home.join(".fonts"));
        roots.push(home.join(".local/share/fonts"));
    }
    let mut fonts = Vec::new();
    for root in roots {
        scan_font_dir(&root, 0, &mut fonts);
    }
    fonts
}

fn scan_font_dir(dir: &Path, depth: usize, fonts: &mut Vec<(String, PathBuf)>) {
    const MAX_DEPTH: usize = 4;
    if depth > MAX_DEPTH {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            scan_font_dir(&path, depth + 1, fonts);
            continue;
        }
        let extension = path
            .extension()
            .and_then(|ext| ext.to_str())
            .map(str::to_ascii_lowercase);
        if !matches!(extension.as_deref(), Some("ttf" | "ttc" | "otf")) {
            continue;
        }
        let Some(name) = path.file_stem() else {
            continue;
        };
        let name = name.to_string_lossy();
        if name.starts_with('.') || name.is_empty() {
            continue;
        }
        fonts.push((name.into_owned(), path));
    }
}

/// 注册一个命名字族：首选字体 + 普通字体回退链。
fn install_face(
    fonts: &mut egui::FontDefinitions,
    family: &str,
    key: &str,
    candidates: &[&str],
    fallback: &[String],
) {
    let Some(bytes) = candidates.iter().find_map(|path| std::fs::read(path).ok()) else {
        return;
    };
    fonts
        .font_data
        .insert(key.to_owned(), egui::FontData::from_owned(bytes).into());
    let list = fonts
        .families
        .entry(egui::FontFamily::Name(family.into()))
        .or_default();
    list.push(key.to_owned());
    for name in fallback {
        if name != key {
            list.push(name.clone());
        }
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
