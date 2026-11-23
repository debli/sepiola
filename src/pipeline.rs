//! 单张与批量流程编排。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

use rayon::prelude::*;

use crate::config::{Config, SceneSettings};
use crate::develop;
use crate::error::{Error, Result};
use crate::grade::stats::{analyze, ToneStats};
use crate::grade::{build_stages, Context, Stage};
use crate::identity::FaceIdentifier;
use crate::imageio::ImageF32;
use crate::report::{self, CsvRow};
use crate::scene::face::FaceClassifier;
use crate::scene::stub::StubClassifier;
use crate::scene::{SceneClassifier, SceneInfo, SceneKind};

/// 各场景预先构建好的 Stage 列表（LUT 只在启动时读取一次）。
pub struct SceneStages {
    portrait: Vec<Box<dyn Stage>>,
    other: Vec<Box<dyn Stage>>,
}

impl SceneStages {
    /// 按配置构建两个场景的 Stage 列表。
    pub fn build(cfg: &Config) -> Result<Self> {
        Ok(Self {
            portrait: build_stages(&cfg.portrait)?,
            other: build_stages(&cfg.other)?,
        })
    }

    /// 取某个场景要执行的 Stage。
    pub fn for_kind(&self, kind: SceneKind) -> &[Box<dyn Stage>] {
        match kind {
            SceneKind::Portrait => &self.portrait,
            SceneKind::Other => &self.other,
        }
    }
}

/// 单张完整处理的结果。
#[derive(Clone, Debug)]
pub struct SingleResult {
    pub input: PathBuf,
    /// 输出 JPEG。
    pub output: PathBuf,
    /// 对比图。
    pub comparison: PathBuf,
    /// 显影后的 TIFF。
    pub developed: PathBuf,
    pub scene: SceneKind,
    /// 识别出的人物标签（人像且启用人物识别时才有）。
    pub person: Option<String>,
    /// 实际执行的 Stage 名称（按执行顺序）。
    pub stages: Vec<&'static str>,
    pub stats_before: ToneStats,
    pub stats_after: ToneStats,
    /// 是否因高光溢出而以 `strength_scale = 0.5` 重做过。
    pub retried: bool,
}

/// 支持的通用图片扩展名；RAW 扩展名由 `develop::is_raw_format` 判断。
const IMAGE_EXTENSIONS: &[&str] = &["jpg", "jpeg", "tif", "tiff", "png", "heic", "heif"];

fn is_supported(path: &Path) -> bool {
    develop::is_raw_format(path)
        || path
            .extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| {
                IMAGE_EXTENSIONS
                    .iter()
                    .any(|known| ext.eq_ignore_ascii_case(known))
            })
            .unwrap_or(false)
}

/// 输入文件相对输入根目录、去扩展名的路径，用于镜像输出结构。
fn relative_stem(raw: &Path, input_dir: &Path) -> PathBuf {
    match raw.strip_prefix(input_dir) {
        Ok(rel) => rel.with_extension(""),
        Err(_) => raw.with_extension(""),
    }
}

/// 计算单张图的输出文件与对比图路径（镜像输入的子目录）。
/// 输出扩展名由 `ext` 决定（`jpg`/`tif`）；对比图始终为 JPEG。
/// `person` 存在时插入人物子目录：`out/<label>/<rel_stem>.<ext>`。
fn output_paths(
    raw: &Path,
    input_dir: &Path,
    output_dir: &Path,
    ext: &str,
    person: Option<&str>,
) -> (PathBuf, PathBuf) {
    let rel_stem = relative_stem(raw, input_dir);
    let base = match person {
        Some(label) => output_dir.join(label),
        None => output_dir.to_path_buf(),
    };
    let output = base.join(&rel_stem).with_extension(ext);
    let mut comparison_name = rel_stem.into_os_string();
    comparison_name.push("_compare");
    let comparison = base.join(comparison_name).with_extension("jpg");
    (output, comparison)
}

/// 按导出像素格式取输出扩展名。
fn output_ext(cfg: &Config) -> &'static str {
    if cfg.is_16bit_output() {
        "tif"
    } else {
        "jpg"
    }
}

/// 处理单张图：develop → classify → apply stages → 安全检查（必要时重做）→ 输出。
pub fn process_single(
    raw: &Path,
    cfg: &Config,
    classifier: &dyn SceneClassifier,
    stages: &SceneStages,
    backend: develop::Backend,
    identifier: Option<&FaceIdentifier>,
) -> Result<SingleResult> {
    let output_dir = Path::new(&cfg.output_dir);
    let input_dir = Path::new(&cfg.input_dir);
    std::fs::create_dir_all(output_dir)?;
    let work_dir = output_dir.join(".work");

    let rel_stem = relative_stem(raw, input_dir);
    let rel_parent = rel_stem.parent().unwrap_or_else(|| Path::new(""));
    let developed = develop::raw_to_tiff(raw, &work_dir.join(rel_parent), backend)?;
    let original = crate::imageio::load(&developed)?;

    let scene: SceneInfo = classifier.classify(&original)?;

    // 人像且启用人物识别：取最大人脸做嵌入匹配；失败只降级为无人物标签。
    let person = match (identifier, scene.kind) {
        (Some(id), SceneKind::Portrait) => scene
            .faces
            .iter()
            .max_by(|a, b| {
                a.area()
                    .partial_cmp(&b.area())
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .and_then(|face| {
                id.identify(&original, face).map(Some).unwrap_or_else(|error| {
                    tracing::warn!(file = %raw.display(), %error, "identity failed; skipping person");
                    None
                })
            }),
        _ => None,
    };

    let stage_list = stages.for_kind(scene.kind);
    let stage_names: Vec<&'static str> = stage_list.iter().map(|stage| stage.name()).collect();

    let apply_all = |strength_scale: f32| -> Result<ImageF32> {
        let mut graded = original.clone();
        let ctx = Context {
            scene: &scene,
            strength_scale,
        };
        for stage in stage_list {
            stage.apply(&mut graded, &ctx)?;
        }
        Ok(graded)
    };

    let stats_before = analyze(&original);
    let mut graded = apply_all(1.0)?;
    let mut stats_after = analyze(&graded);
    let mut retried = false;

    if !stage_list.is_empty() && stats_after.clipped_high > cfg.safety.max_clipped_high {
        tracing::warn!(
            file = %raw.display(),
            clipped_high = stats_after.clipped_high,
            "highlight clipping exceeded threshold, retrying at half strength"
        );
        graded = apply_all(0.5)?;
        stats_after = analyze(&graded);
        retried = true;
    }

    let (output, comparison) =
        output_paths(raw, input_dir, output_dir, output_ext(cfg), person.as_deref());
    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent)?;
    }

    if cfg.is_16bit_output() {
        crate::imageio::save_tiff16(&graded, &output)?;
    } else {
        crate::imageio::save_jpeg(&graded, &output, cfg.jpeg_quality)?;
        if cfg.output.keep_metadata {
            // 仅 JPEG 源有可复制的 APP1/APP2/COM 段；RAW 经显影重编码后无段可复制。
            if crate::imageio::is_jpeg_path(raw) {
                crate::imageio::jpeg_copy_metadata(raw, &output)?;
            }
        } else {
            crate::imageio::jpeg_strip_metadata(&output)?;
        }
    }
    report::save_comparison(&original, &graded, &comparison, cfg.jpeg_quality)?;

    Ok(SingleResult {
        input: raw.to_path_buf(),
        output,
        comparison,
        developed,
        scene: scene.kind,
        person,
        stages: stage_names,
        stats_before,
        stats_after,
        retried,
    })
}

/// 单张处理的结果（成功或失败）。
enum Outcome {
    Success(SingleResult),
    Failure { input: PathBuf, error: String },
    /// 用户取消，未处理该张。
    Cancelled { input: PathBuf },
}

/// 批量处理汇总。
#[derive(Clone, Debug)]
pub struct BatchSummary {
    pub total: usize,
    pub succeeded: usize,
    pub failed: usize,
    /// 因用户取消而未处理的张数。
    pub cancelled: usize,
    pub csv_path: PathBuf,
}

/// 批量处理过程中向外推送的事件（图形界面用）。
#[derive(Clone, Debug)]
pub enum BatchEvent {
    /// 扫描完成，`total` 为待处理数量。
    Discovered { total: usize },
    /// 单张处理完成（成功或失败）。
    Processed {
        completed: usize,
        total: usize,
        input: PathBuf,
        output: PathBuf,
        scene: Option<SceneKind>,
        person: Option<String>,
        ok: bool,
        error: Option<String>,
    },
    /// 因取消而提前结束，剩余张数见 `remaining`。
    Cancelled { remaining: usize },
}

/// 按配置创建场景分类器。
fn build_classifier(settings: &SceneSettings) -> Result<Box<dyn SceneClassifier>> {
    match settings.classifier.as_str() {
        "stub" => Ok(Box::new(StubClassifier::other())),
        "face" => Ok(Box::new(FaceClassifier::new(settings)?)),
        other => Err(Error::Config(format!(
            "scene.classifier must be 'face' or 'stub', got '{other}'"
        ))),
    }
}

/// 按配置创建人物识别器；禁用或模型缺失时返回 None（告警后回退）。
fn build_identifier(settings: &crate::config::IdentitySettings) -> Result<Option<Arc<FaceIdentifier>>> {
    if !settings.enabled {
        return Ok(None);
    }
    match FaceIdentifier::new(settings) {
        Ok(identifier) => Ok(Some(Arc::new(identifier))),
        Err(error) => {
            tracing::warn!(%error, "identity disabled: model unavailable");
            Ok(None)
        }
    }
}

/// 处理整个输入文件夹：rayon 并行，单张失败不影响整批。
pub fn run_batch(cfg: &Config) -> Result<BatchSummary> {
    run_batch_with(cfg, |_| {})
}

/// 与 [`run_batch`] 相同，但每完成一张就通过 `on_event` 通知调用方。
pub fn run_batch_with<F>(cfg: &Config, on_event: F) -> Result<BatchSummary>
where
    F: Fn(BatchEvent) + Sync,
{
    run_batch_cancellable(cfg, on_event, &AtomicBool::new(false))
}

/// 在 [`run_batch_with`] 基础上支持取消：`cancel` 置为 true 后，
/// 尚未开始处理的图片会跳过（记为 cancelled），已开始的会跑完。
pub fn run_batch_cancellable<F>(cfg: &Config, on_event: F, cancel: &AtomicBool) -> Result<BatchSummary>
where
    F: Fn(BatchEvent) + Sync,
{
    // 输出目录可能位于输入目录内，扫描时跳过它，避免把上一轮产物当成输入。
    let exclude = std::fs::canonicalize(&cfg.output_dir).ok();
    let inputs = collect_inputs(Path::new(&cfg.input_dir), exclude.as_deref())?;
    std::fs::create_dir_all(&cfg.output_dir)?;
    let stages = SceneStages::build(cfg)?;
    let classifier = build_classifier(&cfg.scene_settings)?;
    let backend = develop::Backend::from_settings(&cfg.develop)?;
    let identifier = build_identifier(&cfg.identity)?;

    tracing::info!(count = inputs.len(), "found input files");
    let total = inputs.len();
    on_event(BatchEvent::Discovered { total });

    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(cfg.workers)
        .build()
        .map_err(|e| Error::Config(format!("failed to build thread pool: {e}")))?;

    let completed = AtomicUsize::new(0);
    let outcomes: Vec<Outcome> = pool.install(|| {
        inputs
            .par_iter()
            .map(|raw| {
                // 取消后不再启动新任务，直接把剩余项标记为已取消。
                let outcome = if cancel.load(Ordering::Relaxed) {
                    Outcome::Cancelled { input: raw.clone() }
                } else {
                    match process_single(
                        raw,
                        cfg,
                        classifier.as_ref(),
                        &stages,
                        backend,
                        identifier.as_deref(),
                    ) {
                        Ok(result) => {
                            tracing::info!(
                                file = %result.input.display(),
                                scene = result.scene.as_str(),
                                retried = result.retried,
                                "processed"
                            );
                            Outcome::Success(result)
                        }
                        Err(error) => {
                            tracing::error!(file = %raw.display(), %error, "processing failed");
                            Outcome::Failure {
                                input: raw.clone(),
                                error: error.to_string(),
                            }
                        }
                    }
                };

                let fallback_output = || {
                    output_paths(
                        raw,
                        Path::new(&cfg.input_dir),
                        Path::new(&cfg.output_dir),
                        output_ext(cfg),
                        None,
                    )
                    .0
                };
                let done = completed.fetch_add(1, Ordering::Relaxed) + 1;
                let (ok, scene, error, output, person) = match &outcome {
                    Outcome::Success(result) => (
                        true,
                        Some(result.scene),
                        None,
                        result.output.clone(),
                        result.person.clone(),
                    ),
                    Outcome::Failure { error, .. } => (
                        false,
                        None,
                        Some(error.clone()),
                        fallback_output(),
                        None,
                    ),
                    Outcome::Cancelled { .. } => (
                        false,
                        None,
                        Some("已取消".to_string()),
                        fallback_output(),
                        None,
                    ),
                };
                on_event(BatchEvent::Processed {
                    completed: done,
                    total,
                    input: raw.clone(),
                    output,
                    scene,
                    person,
                    ok,
                    error,
                });
                outcome
            })
            .collect()
    });

    let rows: Vec<CsvRow> = outcomes
        .iter()
        .map(|outcome| match outcome {
            Outcome::Success(result) => CsvRow {
                file: result.input.to_string_lossy().into_owned(),
                scene: result.scene.as_str().to_string(),
                person: result.person.clone().unwrap_or_default(),
                stages: result.stages.join("|"),
                clipped_high_before: result.stats_before.clipped_high,
                clipped_high_after: result.stats_after.clipped_high,
                retried: result.retried,
                status: "ok".to_string(),
                error: String::new(),
            },
            Outcome::Failure { input, error } => CsvRow {
                file: input.to_string_lossy().into_owned(),
                scene: String::new(),
                person: String::new(),
                stages: String::new(),
                clipped_high_before: 0.0,
                clipped_high_after: 0.0,
                retried: false,
                status: "error".to_string(),
                error: error.clone(),
            },
            Outcome::Cancelled { input } => CsvRow {
                file: input.to_string_lossy().into_owned(),
                scene: String::new(),
                person: String::new(),
                stages: String::new(),
                clipped_high_before: 0.0,
                clipped_high_after: 0.0,
                retried: false,
                status: "cancelled".to_string(),
                error: String::new(),
            },
        })
        .collect();

    let csv_path = Path::new(&cfg.output_dir).join("log.csv");
    report::write_csv(&csv_path, &rows)?;

    let succeeded = rows.iter().filter(|row| row.status == "ok").count();
    let cancelled = rows.iter().filter(|row| row.status == "cancelled").count();
    let failed = rows.len() - succeeded - cancelled;
    if cancelled > 0 {
        on_event(BatchEvent::Cancelled { remaining: cancelled });
    }
    Ok(BatchSummary {
        total: rows.len(),
        succeeded,
        failed,
        cancelled,
        csv_path,
    })
}

/// 递归列出文件夹中可处理的图片。
///
/// `exclude` 指定的目录（通常是输出目录）会被跳过；符号链接目录不会被跟随，
/// 避免目录环导致无限递归。
pub fn collect_inputs(dir: &Path, exclude: Option<&Path>) -> Result<Vec<PathBuf>> {
    let mut inputs: Vec<PathBuf> = Vec::new();
    walk(dir, exclude, &mut inputs)?;
    inputs.sort();
    Ok(inputs)
}

fn walk(dir: &Path, exclude: Option<&Path>, inputs: &mut Vec<PathBuf>) -> Result<()> {
    let entries = std::fs::read_dir(dir)
        .map_err(|e| Error::Config(format!("failed to read {}: {e}", dir.display())))?;
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            let skipped = exclude
                .and_then(|excluded| std::fs::canonicalize(&path).ok().map(|p| p == *excluded))
                .unwrap_or(false);
            if !skipped {
                walk(&path, exclude, inputs)?;
            }
        } else if file_type.is_file() && is_supported(&path) {
            inputs.push(path);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::SafetyConfig;

    fn config_for(dir: &Path) -> Config {
        Config {
            input_dir: dir.to_string_lossy().into_owned(),
            output_dir: dir.join("out").to_string_lossy().into_owned(),
            jpeg_quality: 90,
            workers: 2,
            portrait: crate::config::SceneConfig::default(),
            other: crate::config::SceneConfig::default(),
            scene_settings: SceneSettings {
                classifier: "stub".to_string(),
                ..SceneSettings::default()
            },
            safety: SafetyConfig::default(),
            develop: crate::config::DevelopSettings::default(),
            output: crate::config::OutputSettings::default(),
            identity: crate::config::IdentitySettings {
                enabled: false,
                ..crate::config::IdentitySettings::default()
            },
            scene_tags: crate::config::SceneTagSettings::default(),
            reader: crate::config::ReaderSettings::default(),
        }
    }

    #[test]
    fn heif_images_are_supported_case_insensitively() {
        assert!(is_supported(Path::new("photo.heic")));
        assert!(is_supported(Path::new("photo.HEIF")));
    }

    #[test]
    fn collect_inputs_filters_and_sorts() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!("sepiola-collect-{nanos}"));
        std::fs::create_dir_all(&dir).expect("create dir");
        for name in ["b.dng", "a.CR2", "note.txt", "c.jpg"] {
            std::fs::write(dir.join(name), b"x").expect("write file");
        }
        let found = collect_inputs(&dir, None).expect("collect");
        let names: Vec<String> = found
            .iter()
            .map(|p| {
                p.file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default()
            })
            .collect();
        assert_eq!(names, vec!["a.CR2", "b.dng", "c.jpg"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_input_dir_is_error() {
        let missing = std::env::temp_dir().join("sepiola-no-such-dir-xyz");
        let cfg = config_for(&missing);
        assert!(run_batch(&cfg).is_err());
    }

    #[test]
    fn batch_with_no_supported_files_writes_header_only_csv() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!("sepiola-empty-{nanos}"));
        std::fs::create_dir_all(&dir).expect("create dir");
        let cfg = config_for(&dir);
        let summary = run_batch(&cfg).expect("run batch");
        assert_eq!(summary.total, 0);
        assert_eq!(summary.succeeded, 0);
        assert_eq!(summary.failed, 0);
        assert!(summary.csv_path.is_file());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cancelled_batch_skips_remaining_files() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!("sepiola-cancel-{nanos}"));
        std::fs::create_dir_all(&dir).expect("create dir");
        for name in ["a.jpg", "b.jpg", "c.jpg"] {
            std::fs::write(dir.join(name), b"x").expect("write file");
        }
        let cfg = config_for(&dir);

        // 一开始就取消：全部跳过，不产生输出文件。
        let cancel = AtomicBool::new(true);
        let events = std::sync::Mutex::new(Vec::new());
        let summary = run_batch_cancellable(
            &cfg,
            |event| events.lock().expect("lock").push(event),
            &cancel,
        )
        .expect("run batch");
        assert_eq!(summary.total, 3);
        assert_eq!(summary.cancelled, 3);
        assert_eq!(summary.succeeded, 0);
        assert_eq!(summary.failed, 0);
        assert!(!Path::new(&cfg.output_dir).join("a.jpg").exists());

        let events = events.into_inner().expect("lock");
        assert!(matches!(events.first(), Some(BatchEvent::Discovered { total: 3 })));
        assert!(matches!(
            events.last(),
            Some(BatchEvent::Cancelled { remaining: 3 })
        ));
        // CSV 记录 cancelled 状态，便于事后核对。
        let csv = std::fs::read_to_string(summary.csv_path).expect("read csv");
        assert_eq!(csv.matches("cancelled").count(), 3);

        // 未取消时不会误判为 cancelled（此处输入是无效图片，故计入 failed）。
        let summary = run_batch_cancellable(&cfg, |_| {}, &AtomicBool::new(false)).expect("run");
        assert_eq!(summary.cancelled, 0);
        assert_eq!((summary.total, summary.succeeded, summary.failed), (3, 0, 3));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn batch_emits_discovered_event_with_zero_total() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!("sepiola-events-{nanos}"));
        std::fs::create_dir_all(&dir).expect("create dir");
        let cfg = config_for(&dir);

        let events = std::sync::Mutex::new(Vec::new());
        run_batch_with(&cfg, |event| events.lock().expect("lock").push(event))
            .expect("run batch");

        let events = events.into_inner().expect("lock");
        assert!(matches!(
            events.as_slice(),
            [BatchEvent::Discovered { total: 0 }]
        ));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn collect_inputs_walks_subdirectories() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!("sepiola-walk-{nanos}"));
        std::fs::create_dir_all(dir.join("sub").join("deep")).expect("create dirs");
        for name in ["a.dng", "sub/b.CR2", "sub/deep/c.jpg", "sub/note.txt"] {
            std::fs::write(dir.join(name), b"x").expect("write file");
        }
        let found = collect_inputs(&dir, None).expect("collect");
        let rels: Vec<PathBuf> = found
            .iter()
            .map(|p| p.strip_prefix(&dir).expect("under root").to_path_buf())
            .collect();
        assert_eq!(
            rels,
            vec![
                PathBuf::from("a.dng"),
                PathBuf::from("sub/b.CR2"),
                PathBuf::from("sub/deep/c.jpg")
            ]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn collect_inputs_skips_excluded_directory() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!("sepiola-exclude-{nanos}"));
        std::fs::create_dir_all(dir.join("out").join("nested")).expect("create dirs");
        std::fs::write(dir.join("a.dng"), b"x").expect("write file");
        std::fs::write(dir.join("out/b.jpg"), b"x").expect("write file");
        std::fs::write(dir.join("out/nested/c.jpg"), b"x").expect("write file");

        let excluded = std::fs::canonicalize(dir.join("out")).expect("canonicalize");
        let found = collect_inputs(&dir, Some(&excluded)).expect("collect");
        assert_eq!(found, vec![dir.join("a.dng")]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn output_paths_mirror_subdirectories() {
        let input_root = Path::new("/photos");
        let output_root = Path::new("/out");
        assert_eq!(
            output_paths(Path::new("/photos/2024/a.dng"), input_root, output_root, "jpg", None),
            (
                PathBuf::from("/out/2024/a.jpg"),
                PathBuf::from("/out/2024/a_compare.jpg")
            )
        );
        assert_eq!(
            output_paths(Path::new("/photos/b.jpg"), input_root, output_root, "jpg", None),
            (
                PathBuf::from("/out/b.jpg"),
                PathBuf::from("/out/b_compare.jpg")
            )
        );
        // 人物标签插入子目录。
        assert_eq!(
            output_paths(
                Path::new("/photos/2024/a.dng"),
                input_root,
                output_root,
                "jpg",
                Some("alice")
            ),
            (
                PathBuf::from("/out/alice/2024/a.jpg"),
                PathBuf::from("/out/alice/2024/a_compare.jpg")
            )
        );
    }

    #[test]
    fn output_paths_uses_format_extension() {
        let input_root = Path::new("/photos");
        let output_root = Path::new("/out");
        // 16-bit 导出用 .tif，对比图仍是 .jpg。
        assert_eq!(
            output_paths(Path::new("/photos/a.dng"), input_root, output_root, "tif", None),
            (
                PathBuf::from("/out/a.tif"),
                PathBuf::from("/out/a_compare.jpg")
            )
        );
        let mut cfg = Config::default();
        assert_eq!(output_ext(&cfg), "jpg");
        cfg.output.pixel_format = "16bit".to_string();
        assert_eq!(output_ext(&cfg), "tif");
    }
}
