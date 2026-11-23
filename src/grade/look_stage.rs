use crate::error::Result;
use crate::grade::blend;
use crate::grade::{Context, Stage};
use crate::imageio::ImageF32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LookPreset {
    Natural,
    Warm,
    Cinematic,
    Vivid,
    Matte,
    Monochrome,
}

impl LookPreset {
    pub const ALL: [Self; 6] = [
        Self::Natural,
        Self::Warm,
        Self::Cinematic,
        Self::Vivid,
        Self::Matte,
        Self::Monochrome,
    ];

    pub fn from_id(id: &str) -> Option<Self> {
        match id {
            "natural" => Some(Self::Natural),
            "warm" => Some(Self::Warm),
            "cinematic" => Some(Self::Cinematic),
            "vivid" => Some(Self::Vivid),
            "matte" => Some(Self::Matte),
            "monochrome" => Some(Self::Monochrome),
            _ => None,
        }
    }

    pub fn id(self) -> &'static str {
        match self {
            Self::Natural => "natural",
            Self::Warm => "warm",
            Self::Cinematic => "cinematic",
            Self::Vivid => "vivid",
            Self::Matte => "matte",
            Self::Monochrome => "monochrome",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Natural => "自然清透",
            Self::Warm => "暖阳人像",
            Self::Cinematic => "电影青橙",
            Self::Vivid => "鲜艳风光",
            Self::Matte => "柔和哑光",
            Self::Monochrome => "黑白经典",
        }
    }

    pub fn default_exposure_ev(self) -> f32 {
        match self {
            Self::Natural => 0.15,
            Self::Warm => 0.1,
            Self::Cinematic => 0.0,
            Self::Vivid => 0.05,
            Self::Matte => 0.1,
            Self::Monochrome => 0.0,
        }
    }

    fn parameters(self) -> LookParameters {
        match self {
            Self::Natural => LookParameters::new(1.04, 1.06, 0.015, 0.0),
            Self::Warm => LookParameters::new(1.03, 1.08, 0.075, 0.0),
            Self::Cinematic => LookParameters::new(1.12, 0.82, 0.035, 0.015),
            Self::Vivid => LookParameters::new(1.1, 1.3, 0.0, 0.0),
            Self::Matte => LookParameters::new(0.88, 0.86, 0.02, 0.055),
            Self::Monochrome => LookParameters::new(1.12, 0.0, 0.0, 0.0),
        }
    }
}

struct LookParameters {
    contrast: f32,
    saturation: f32,
    warmth: f32,
    fade: f32,
}

impl LookParameters {
    fn new(contrast: f32, saturation: f32, warmth: f32, fade: f32) -> Self {
        Self {
            contrast,
            saturation,
            warmth,
            fade,
        }
    }
}

pub struct LookStage {
    preset: LookPreset,
    strength: f32,
    exposure_ev: f32,
}

impl LookStage {
    pub fn new(preset: LookPreset, strength: f32, exposure_ev: f32) -> Self {
        Self {
            preset,
            strength: strength.clamp(0.0, 1.0),
            exposure_ev: exposure_ev.clamp(-5.0, 5.0),
        }
    }
}

impl Stage for LookStage {
    fn name(&self) -> &'static str {
        "look"
    }

    fn apply(&self, img: &mut ImageF32, ctx: &Context<'_>) -> Result<()> {
        let original = img.clone();
        let parameters = self.preset.parameters();
        let exposure = 2.0f32.powf(self.exposure_ev);

        for pixel in img.data.chunks_exact_mut(3) {
            for channel in pixel.iter_mut() {
                *channel = linear_to_srgb((srgb_to_linear(*channel) * exposure).clamp(0.0, 1.0));
            }

            let luminance = 0.2126 * pixel[0] + 0.7152 * pixel[1] + 0.0722 * pixel[2];
            for channel in pixel.iter_mut() {
                *channel = (*channel - 0.5) * parameters.contrast + 0.5;
                *channel = luminance + (*channel - luminance) * parameters.saturation;
            }

            let shadow = ((0.55 - luminance) / 0.55).clamp(0.0, 1.0);
            let highlight = ((luminance - 0.45) / 0.55).clamp(0.0, 1.0);
            pixel[0] += parameters.warmth * (highlight - shadow * 0.35);
            pixel[2] += parameters.warmth * (shadow * 0.45 - highlight);
            for channel in pixel.iter_mut() {
                *channel = parameters.fade + *channel * (1.0 - 2.0 * parameters.fade);
                *channel = channel.clamp(0.0, 1.0);
            }
        }

        let effective = (self.strength * ctx.strength_scale).clamp(0.0, 1.0);
        blend::blend(&original, img, effective);
        Ok(())
    }
}

fn srgb_to_linear(value: f32) -> f32 {
    if value <= 0.04045 {
        value / 12.92
    } else {
        ((value + 0.055) / 1.055).powf(2.4)
    }
}

fn linear_to_srgb(value: f32) -> f32 {
    if value <= 0.0031308 {
        value * 12.92
    } else {
        1.055 * value.powf(1.0 / 2.4) - 0.055
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scene::{SceneInfo, SceneKind};

    #[test]
    fn zero_strength_preserves_pixels() {
        let original = ImageF32::from_data(1, 1, vec![0.2, 0.4, 0.8]).expect("image");
        let mut graded = original.clone();
        let scene = SceneInfo::without_faces(SceneKind::Other);
        let ctx = Context {
            scene: &scene,
            strength_scale: 1.0,
        };
        LookStage::new(LookPreset::Warm, 0.0, 1.0)
            .apply(&mut graded, &ctx)
            .expect("apply look");
        assert_eq!(graded.data, original.data);
    }

    #[test]
    fn monochrome_removes_color_at_full_strength() {
        let mut image = ImageF32::from_data(1, 1, vec![0.9, 0.2, 0.1]).expect("image");
        let scene = SceneInfo::without_faces(SceneKind::Other);
        let ctx = Context {
            scene: &scene,
            strength_scale: 1.0,
        };
        LookStage::new(LookPreset::Monochrome, 1.0, 0.0)
            .apply(&mut image, &ctx)
            .expect("apply look");
        assert!((image.data[0] - image.data[1]).abs() < 1e-6);
        assert!((image.data[1] - image.data[2]).abs() < 1e-6);
    }
}
