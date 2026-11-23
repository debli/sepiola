//! 所有「调光调色」逻辑：Stage 抽象与各 Stage 实现。

use std::sync::Arc;

use crate::config::SceneConfig;
use crate::error::{Error, Result};
use crate::imageio::ImageF32;
use crate::scene::SceneInfo;

pub mod blend;
pub mod look_stage;
pub mod lut3d;
pub mod lut_stage;
pub mod stats;

/// Stage 执行时的上下文。
pub struct Context<'a> {
    pub scene: &'a SceneInfo,
    /// 全局强度系数，默认 1.0；安全检查失败时会减半。
    pub strength_scale: f32,
}

/// 一次调色处理。实现必须可跨线程共享。
pub trait Stage: Send + Sync {
    fn name(&self) -> &'static str;
    fn apply(&self, img: &mut ImageF32, ctx: &Context<'_>) -> Result<()>;
}

/// 按某个场景的配置，创建要依次执行的 Stage 列表。
pub fn build_stages(cfg: &SceneConfig) -> Result<Vec<Box<dyn Stage>>> {
    let mut stages: Vec<Box<dyn Stage>> = Vec::with_capacity(cfg.stages.len());
    for name in &cfg.stages {
        match name.as_str() {
            "look" => {
                let look_cfg = cfg.look.as_ref().ok_or_else(|| {
                    Error::Config(
                        "stage 'look' is listed but the scene has no look section".to_string(),
                    )
                })?;
                let preset =
                    look_stage::LookPreset::from_id(&look_cfg.preset).ok_or_else(|| {
                        Error::Config(format!("unknown built-in look '{}'", look_cfg.preset))
                    })?;
                stages.push(Box::new(look_stage::LookStage::new(
                    preset,
                    look_cfg.strength,
                    look_cfg.exposure_ev,
                )));
            }
            "lut" => {
                let lut_cfg = cfg.lut.as_ref().ok_or_else(|| {
                    Error::Config(format!(
                        "stage 'lut' is listed but the scene has no lut section ({name})"
                    ))
                })?;
                let lut_path = crate::paths::resolve_asset(&lut_cfg.file);
                tracing::info!(lut = %lut_path.display(), "loading lut");
                let lut = lut3d::Lut3d::from_cube_file(&lut_path)?;
                stages.push(Box::new(lut_stage::LutStage::new(
                    Arc::new(lut),
                    lut_cfg.strength,
                )));
            }
            other => {
                return Err(Error::Config(format!("unknown stage '{other}'")));
            }
        }
    }
    Ok(stages)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{LutConfig, SceneConfig};
    use crate::grade::lut3d::Lut3d;
    use crate::scene::{SceneInfo, SceneKind};
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_cube(name: &str, content: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let path = std::env::temp_dir().join(format!("sepiola-{nanos}-{name}.cube"));
        std::fs::write(&path, content).expect("write cube");
        path
    }

    /// R 与 B 互换的 2x2x2 LUT。
    const SWAP_RB: &str = "LUT_3D_SIZE 2\n0 0 0\n0 0 1\n0 1 0\n0 1 1\n1 0 0\n1 0 1\n1 1 0\n1 1 1\n";

    fn gradient() -> ImageF32 {
        ImageF32::from_data(
            4,
            2,
            vec![
                0.9, 0.2, 0.1, 0.1, 0.5, 0.9, 0.4, 0.6, 0.2, 0.7, 0.3, 0.8, 0.2, 0.8, 0.4, 0.6,
                0.1, 0.9, 0.5, 0.5, 0.5, 0.3, 0.3, 0.3,
            ],
        )
        .expect("valid data")
    }

    #[test]
    fn stage_result_matches_direct_lut_and_blend() {
        let path = temp_cube("swap", SWAP_RB);
        let cfg = SceneConfig {
            stages: vec!["lut".to_string()],
            look: None,
            lut: Some(LutConfig {
                file: path.to_string_lossy().into_owned(),
                strength: 0.5,
            }),
        };
        let stages = build_stages(&cfg).expect("build stages");

        let original = gradient();
        let scene = SceneInfo::without_faces(SceneKind::Other);
        let ctx = Context {
            scene: &scene,
            strength_scale: 1.0,
        };
        let mut via_stage = original.clone();
        for stage in &stages {
            stage.apply(&mut via_stage, &ctx).expect("apply stage");
        }

        let lut = Lut3d::from_cube_str(SWAP_RB).expect("parse lut");
        let mut direct = original.clone();
        lut.apply(&mut direct);
        blend::blend(&original, &mut direct, 0.5);

        assert_eq!(via_stage.data.len(), direct.data.len());
        for (a, b) in via_stage.data.iter().zip(direct.data.iter()) {
            assert!((a - b).abs() < 1e-6, "stage {a} vs direct {b}");
        }
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn strength_scale_multiplies_stage_strength() {
        let path = temp_cube("swap-scale", SWAP_RB);
        let cfg = SceneConfig {
            stages: vec!["lut".to_string()],
            look: None,
            lut: Some(LutConfig {
                file: path.to_string_lossy().into_owned(),
                strength: 1.0,
            }),
        };
        let stages = build_stages(&cfg).expect("build stages");
        let original = gradient();
        let scene = SceneInfo::without_faces(SceneKind::Other);
        let ctx = Context {
            scene: &scene,
            strength_scale: 0.5,
        };
        let mut img = original.clone();
        for stage in &stages {
            stage.apply(&mut img, &ctx).expect("apply stage");
        }

        let lut = Lut3d::from_cube_str(SWAP_RB).expect("parse lut");
        let mut direct = original.clone();
        lut.apply(&mut direct);
        blend::blend(&original, &mut direct, 0.5);

        for (a, b) in img.data.iter().zip(direct.data.iter()) {
            assert!((a - b).abs() < 1e-6);
        }
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn unknown_stage_is_rejected() {
        let cfg = SceneConfig {
            stages: vec!["sharpen".to_string()],
            look: None,
            lut: None,
        };
        assert!(build_stages(&cfg).is_err());
    }

    #[test]
    fn empty_stage_list_is_fine() {
        let cfg = SceneConfig::default();
        let stages = build_stages(&cfg).expect("build empty");
        assert!(stages.is_empty());
    }
}
