//! LUT Stage：套用 LUT 并按强度与调色前结果混合。

use std::sync::Arc;

use crate::error::Result;
use crate::grade::blend;
use crate::grade::lut3d::Lut3d;
use crate::grade::{Context, Stage};
use crate::imageio::ImageF32;

/// 对一个 LUT 的封装：内部 clone 原图后应用，再按强度混合。
pub struct LutStage {
    lut: Arc<Lut3d>,
    strength: f32,
}

impl LutStage {
    /// `strength` 会被截断到 0.0..=1.0。
    pub fn new(lut: Arc<Lut3d>, strength: f32) -> Self {
        Self {
            lut,
            strength: strength.clamp(0.0, 1.0),
        }
    }

    /// 该 Stage 配置的基准强度。
    pub fn strength(&self) -> f32 {
        self.strength
    }
}

impl Stage for LutStage {
    fn name(&self) -> &'static str {
        "lut"
    }

    fn apply(&self, img: &mut ImageF32, ctx: &Context<'_>) -> Result<()> {
        let original = img.clone();
        self.lut.apply(img);
        let effective = (self.strength * ctx.strength_scale).clamp(0.0, 1.0);
        blend::blend(&original, img, effective);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scene::{SceneInfo, SceneKind};

    fn identity_lut() -> Arc<Lut3d> {
        let mut text = String::from("LUT_3D_SIZE 2\n");
        for b in 0..2 {
            for g in 0..2 {
                for r in 0..2 {
                    text.push_str(&format!("{r} {g} {b}\n"));
                }
            }
        }
        Arc::new(Lut3d::from_cube_str(&text).expect("parse identity"))
    }

    fn image() -> ImageF32 {
        ImageF32::from_data(2, 2, vec![0.1, 0.2, 0.3, 0.4, 0.5, 0.6, 0.7, 0.8, 0.9, 0.2, 0.4, 0.6])
            .expect("valid data")
    }

    #[test]
    fn identity_lut_leaves_image_unchanged() {
        let original = image();
        let mut img = original.clone();
        let scene = SceneInfo::without_faces(SceneKind::Other);
        let ctx = Context {
            scene: &scene,
            strength_scale: 1.0,
        };
        let stage = LutStage::new(identity_lut(), 1.0);
        stage.apply(&mut img, &ctx).expect("apply");
        for (a, b) in original.data.iter().zip(img.data.iter()) {
            assert!((a - b).abs() < 1e-6);
        }
    }

    #[test]
    fn strength_zero_ignores_lut() {
        // 交换 R/B 的 LUT，强度为 0 时应保持原样。
        let mut text = String::from("LUT_3D_SIZE 2\n");
        for b in 0..2 {
            for g in 0..2 {
                for r in 0..2 {
                    text.push_str(&format!("{b} {g} {r}\n"));
                }
            }
        }
        let lut = Arc::new(Lut3d::from_cube_str(&text).expect("parse"));
        let original = image();
        let mut img = original.clone();
        let scene = SceneInfo::without_faces(SceneKind::Other);
        let ctx = Context {
            scene: &scene,
            strength_scale: 1.0,
        };
        LutStage::new(lut, 0.0).apply(&mut img, &ctx).expect("apply");
        assert_eq!(img.data, original.data);
    }

    #[test]
    fn name_is_lut() {
        let stage = LutStage::new(identity_lut(), 1.0);
        assert_eq!(stage.name(), "lut");
    }
}
