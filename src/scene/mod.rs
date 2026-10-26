//! 场景分析：类型定义与分类器接口。

use crate::error::Result;
use crate::imageio::ImageF32;

pub mod face;
pub mod landmark;
pub mod places;
pub mod stub;
pub mod tags;

/// 场景类型。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SceneKind {
    Portrait,
    Other,
}

impl SceneKind {
    /// 用于日志与 CSV 的名称。
    pub fn as_str(&self) -> &'static str {
        match self {
            SceneKind::Portrait => "portrait",
            SceneKind::Other => "other",
        }
    }
}

/// 归一化坐标，0.0..=1.0，相对整张图。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FaceBox {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl FaceBox {
    /// 归一化面积（0.0..=1.0）。
    pub fn area(&self) -> f32 {
        (self.w.max(0.0) * self.h.max(0.0)).clamp(0.0, 1.0)
    }
}

/// 单张图的场景分析结果。
#[derive(Clone, Debug, PartialEq)]
pub struct SceneInfo {
    pub kind: SceneKind,
    /// 没有人脸则为空。
    pub faces: Vec<FaceBox>,
}

impl SceneInfo {
    /// 没有检测到人脸时的场景结果。
    pub fn without_faces(kind: SceneKind) -> Self {
        Self {
            kind,
            faces: Vec::new(),
        }
    }

    /// 最大人脸占整张图的面积比例；没有人脸时为 0.0。
    pub fn largest_face_ratio(&self) -> f32 {
        self.faces.iter().map(FaceBox::area).fold(0.0, f32::max)
    }
}

/// 场景分类器。实现必须可跨线程共享。
pub trait SceneClassifier: Send + Sync {
    fn classify(&self, img: &ImageF32) -> Result<SceneInfo>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn largest_face_ratio_picks_biggest() {
        let info = SceneInfo {
            kind: SceneKind::Portrait,
            faces: vec![
                FaceBox {
                    x: 0.0,
                    y: 0.0,
                    w: 0.1,
                    h: 0.1,
                },
                FaceBox {
                    x: 0.2,
                    y: 0.2,
                    w: 0.5,
                    h: 0.4,
                },
            ],
        };
        assert!((info.largest_face_ratio() - 0.2).abs() < 1e-6);
    }

    #[test]
    fn no_faces_means_zero_ratio() {
        let info = SceneInfo::without_faces(SceneKind::Other);
        assert_eq!(info.largest_face_ratio(), 0.0);
    }

    #[test]
    fn kind_names_are_stable() {
        assert_eq!(SceneKind::Portrait.as_str(), "portrait");
        assert_eq!(SceneKind::Other.as_str(), "other");
    }
}
