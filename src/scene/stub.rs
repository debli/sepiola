//! 假场景分类器：返回固定结果，用于在接入真实人脸检测前跑通流程。

use crate::error::Result;
use crate::imageio::ImageF32;
use crate::scene::{SceneClassifier, SceneInfo, SceneKind};

/// 固定返回指定场景的分类器。
pub struct StubClassifier {
    kind: SceneKind,
}

impl StubClassifier {
    /// 构造一个总是返回 `kind` 的分类器。
    pub fn new(kind: SceneKind) -> Self {
        Self { kind }
    }

    /// 总是返回人像。
    pub fn portrait() -> Self {
        Self::new(SceneKind::Portrait)
    }

    /// 总是返回其他（风景等）。
    pub fn other() -> Self {
        Self::new(SceneKind::Other)
    }
}

impl SceneClassifier for StubClassifier {
    fn classify(&self, _img: &ImageF32) -> Result<SceneInfo> {
        Ok(SceneInfo::without_faces(self.kind))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::imageio::ImageF32;

    #[test]
    fn stub_returns_fixed_kind() {
        let img = ImageF32::new(2, 2).expect("valid size");
        let classifier = StubClassifier::portrait();
        let info = classifier.classify(&img).expect("classify");
        assert_eq!(info.kind, SceneKind::Portrait);
        assert!(info.faces.is_empty());
    }

    #[test]
    fn stub_other_variant() {
        let img = ImageF32::new(1, 1).expect("valid size");
        let info = StubClassifier::other().classify(&img).expect("classify");
        assert_eq!(info.kind, SceneKind::Other);
    }
}
