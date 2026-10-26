//! 人物身份识别：ArcFace 嵌入 + 本地图库（gallery）持久化。
//!
//! 模型：`arcfaceresnet100-11-int8.onnx`（ONNX Models，ArcFace R100 INT8）。
//! 输入 1×3×112×112 BGR、原始像素值 0..255（int8 量化版已将归一化烘焙进
//! 量化参数，再做减均值除方差反而会让嵌入塌缩，实测不同人余弦可达 0.87）；
//! 取首个输出 `fc1`（512 维嵌入）并做 L2 归一化，用余弦相似度匹配图库。

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use ort::session::Session;
use ort::value::Tensor;
use serde::{Deserialize, Serialize};

use crate::config::IdentitySettings;
use crate::error::{Error, Result};
use crate::imageio::{ImageF32, CHANNELS};
use crate::scene::face::ort_environment;
use crate::scene::FaceBox;

/// ArcFace 要求的输入边长。
const EMBED_SIDE: u32 = 112;
/// 嵌入维度。
pub const EMBED_DIM: usize = 512;

/// 人脸嵌入与身份匹配。线程安全，批量处理时以 `Arc` 共享。
pub struct FaceIdentifier {
    session: Mutex<Session>,
    threshold: f32,
    update_gallery: bool,
    max_extra: usize,
    update_min_sim: f32,
    landmark: Option<crate::scene::landmark::LandmarkModel>,
    gallery: Mutex<Gallery>,
    gallery_path: PathBuf,
}

impl FaceIdentifier {
    /// 加载 ArcFace 模型与图库。模型不存在时返回 Err（调用方决定回退）。
    pub fn new(settings: &IdentitySettings) -> Result<Self> {
        let model = crate::paths::resolve_asset(&settings.model);
        if !model.is_file() {
            return Err(Error::Model(format!(
                "identity model not found: {}",
                model.display()
            )));
        }
        let environment = ort_environment()?;
        tracing::info!(model = %model.display(), "loading face identifier model");
        let session = Session::builder(environment)
            .and_then(|mut builder| builder.commit_from_file(&model))
            .map_err(|e| Error::Model(format!("failed to load {}: {e}", model.display())))?;
        let gallery_path = crate::paths::resolve_asset(&settings.gallery);
        let gallery = Gallery::load(&gallery_path).unwrap_or_else(|error| {
            tracing::warn!(%error, "gallery unreadable; starting empty");
            Gallery::default()
        });
        Ok(Self {
            session: Mutex::new(session),
            threshold: settings.threshold,
            update_gallery: settings.update_gallery,
            max_extra: settings.max_extra,
            update_min_sim: settings.update_min_sim,
            landmark: crate::scene::landmark::load_landmark_model(&settings.landmark_model),
            gallery: Mutex::new(gallery),
            gallery_path,
        })
    }

    /// 从整图裁剪最大人脸（外扩 20% 边距、方形化），嵌入并与图库匹配。
    /// 新人物会写入图库并持久化。
    pub fn identify(&self, img: &ImageF32, face: &FaceBox) -> Result<String> {
        let crop = self.aligned_face(img, face);
        let embedding = self.embed(&crop)?;
        let mut gallery = self
            .gallery
            .lock()
            .map_err(|_| Error::Model("identity gallery mutex poisoned".to_string()))?;
        if let Some((index, score)) = gallery.best_match(&embedding) {
            if score >= self.threshold {
                let label = gallery.entries[index].label.clone();
                // Immich 式增量更新：与主嵌入差异大的新样本追加进图库。
                if self.update_gallery
                    && gallery.absorb(index, embedding, self.update_min_sim, self.max_extra)
                {
                    gallery.save(&self.gallery_path)?;
                    tracing::debug!(label = %label, "gallery absorbed new sample");
                }
                tracing::debug!(label = %label, score, "matched existing person");
                return Ok(label);
            }
        }
        let label = gallery.add_person(embedding);
        gallery.save(&self.gallery_path)?;
        tracing::info!(label = %label, path = %self.gallery_path.display(), "registered new person");
        Ok(label)
    }

    /// 人脸裁剪；关键点模型可用时做 digiKam 式 5 点相似变换对齐
    /// （192 空间估计关键点 → 对齐到 ArcFace 112 模板），任一步失败回退普通方形裁剪。
    fn aligned_face(&self, img: &ImageF32, face: &FaceBox) -> ImageF32 {
        let crop = crop_face(img, face);
        let Some(lm) = &self.landmark else {
            return crop;
        };
        let crop192 = resize_exact(&crop, crate::scene::landmark::SIDE);
        let aligned = (|| -> Result<ImageF32> {
            let points = lm.landmarks(&crop192)?;
            let five = crate::scene::landmark::five_points(&points).ok_or_else(|| {
                Error::Model("landmark five-point extraction failed".to_string())
            })?;
            let m = crate::scene::landmark::similarity_transform(
                &five,
                &crate::scene::landmark::arc_template_5(),
            )
            .ok_or_else(|| Error::Model("similarity transform fit failed".to_string()))?;
            crate::scene::landmark::warp_similarity(&crop192, &m, EMBED_SIDE)
        })();
        match aligned {
            Ok(face_img) => face_img,
            Err(error) => {
                tracing::debug!(%error, "landmark alignment failed; using plain crop");
                crop
            }
        }
    }

    /// 计算对齐后的嵌入，不读写图库（供评测工具复用同一条对齐链路）。
    pub fn embedding_for(&self, img: &ImageF32, face: &FaceBox) -> Result<Vec<f32>> {
        let crop = self.aligned_face(img, face);
        self.embed(&crop)
    }

    /// 112×112 人脸图（RGB 0..1）→ 512 维 L2 归一化嵌入。
    fn embed(&self, face_112: &ImageF32) -> Result<Vec<f32>> {
        let resized = resize_exact(face_112, EMBED_SIDE);
        let plane = (EMBED_SIDE * EMBED_SIDE) as usize;
        let mut data = vec![0.0f32; plane * CHANNELS];
        for (idx, px) in resized.data.chunks_exact(CHANNELS).enumerate() {
            let (x, y) = (idx % EMBED_SIDE as usize, idx / EMBED_SIDE as usize);
            let dst = y * EMBED_SIDE as usize + x;
            // 模型期望 BGR、原始像素值 0..255（量化参数已含归一化）。
            for c in 0..CHANNELS {
                data[c * plane + dst] = px[CHANNELS - 1 - c] * 255.0;
            }
        }
        let tensor = Tensor::from_array((
            [1usize, CHANNELS, EMBED_SIDE as usize, EMBED_SIDE as usize],
            data.into_boxed_slice(),
        ))
        .map_err(|e| Error::Model(format!("failed to build embedding input: {e}")))?;

        let mut session = self
            .session
            .lock()
            .map_err(|_| Error::Model("face identifier mutex poisoned".to_string()))?;
        let outputs = session
            .run(ort::inputs![tensor])
            .map_err(|e| Error::Model(format!("face embedding inference failed: {e}")))?;
        let value = &outputs[0];
        let (_, flat) = value
            .try_extract_tensor::<f32>()
            .map_err(|e| Error::Model(format!("embedding output: {e}")))?;
        if flat.len() < EMBED_DIM {
            return Err(Error::Model(format!(
                "embedding output too short: {} < {EMBED_DIM}",
                flat.len()
            )));
        }
        let mut embedding: Vec<f32> = flat[..EMBED_DIM].to_vec();
        l2_normalize(&mut embedding);
        Ok(embedding)
    }
}

/// 人脸框外扩 20% 边距并方形化，从原图裁剪。
pub fn crop_face(img: &ImageF32, face: &FaceBox) -> ImageF32 {
    let cx = (face.x + face.w / 2.0).clamp(0.0, 1.0);
    let cy = (face.y + face.h / 2.0).clamp(0.0, 1.0);
    let side = (face.w.max(face.h) * 1.4).clamp(0.01, 1.0);
    let half = side / 2.0;
    let x0 = (cx - half).clamp(0.0, 1.0);
    let y0 = (cy - half).clamp(0.0, 1.0);
    let x1 = (cx + half).clamp(0.0, 1.0);
    let y1 = (cy + half).clamp(0.0, 1.0);

    let px0 = (x0 * img.width as f32).floor().max(0.0) as u32;
    let py0 = (y0 * img.height as f32).floor().max(0.0) as u32;
    let px1 = (x1 * img.width as f32).ceil().min(img.width as f32) as u32;
    let py1 = (y1 * img.height as f32).ceil().min(img.height as f32) as u32;
    let w = px1.saturating_sub(px0).max(1);
    let h = py1.saturating_sub(py0).max(1);

    let mut out = ImageF32::new(w, h).expect("crop is non-empty");
    for y in 0..h {
        for x in 0..w {
            let src = ((py0 + y) as usize * img.width as usize + (px0 + x) as usize) * CHANNELS;
            let dst = (y as usize * w as usize + x as usize) * CHANNELS;
            out.data[dst..dst + CHANNELS].copy_from_slice(&img.data[src..src + CHANNELS]);
        }
    }
    out
}

/// 双线性缩放到精确的 `side × side`。
pub fn resize_exact(img: &ImageF32, side: u32) -> ImageF32 {
    if img.width == side && img.height == side {
        return img.clone();
    }
    let mut out = vec![0.0f32; side as usize * side as usize * CHANNELS];
    let x_ratio = img.width as f32 / side as f32;
    let y_ratio = img.height as f32 / side as f32;
    for y in 0..side {
        let src_y = (y as f32 + 0.5) * y_ratio - 0.5;
        let y0 = src_y.floor().max(0.0) as u32;
        let y1 = (y0 + 1).min(img.height - 1);
        let wy = (src_y - y0 as f32).clamp(0.0, 1.0);
        for x in 0..side {
            let src_x = (x as f32 + 0.5) * x_ratio - 0.5;
            let x0 = src_x.floor().max(0.0) as u32;
            let x1 = (x0 + 1).min(img.width - 1);
            let wx = (src_x - x0 as f32).clamp(0.0, 1.0);
            let idx = |px: u32, py: u32| (py as usize * img.width as usize + px as usize) * CHANNELS;
            let p00 = idx(x0, y0);
            let p10 = idx(x1, y0);
            let p01 = idx(x0, y1);
            let p11 = idx(x1, y1);
            let dst = (y as usize * side as usize + x as usize) * CHANNELS;
            for c in 0..CHANNELS {
                let top = img.data[p00 + c] * (1.0 - wx) + img.data[p10 + c] * wx;
                let bottom = img.data[p01 + c] * (1.0 - wx) + img.data[p11 + c] * wx;
                out[dst + c] = top * (1.0 - wy) + bottom * wy;
            }
        }
    }
    ImageF32 {
        width: side,
        height: side,
        data: out,
    }
}

fn l2_normalize(v: &mut [f32]) {
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for x in v.iter_mut() {
            *x /= norm;
        }
    }
}

/// 余弦相似度（输入均应为 L2 归一化向量）。
pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

/// 已识别人物：标签 + 主嵌入 + 额外嵌入（Immich 式多样本，旧 JSON 无 extra 字段时自动为空）。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GalleryEntry {
    pub label: String,
    pub embedding: Vec<f32>,
    #[serde(default)]
    pub extra: Vec<Vec<f32>>,
}

impl GalleryEntry {
    pub fn new(label: impl Into<String>, embedding: Vec<f32>) -> Self {
        Self {
            label: label.into(),
            embedding,
            extra: Vec::new(),
        }
    }

    /// 主嵌入与全部额外嵌入。
    pub fn embeddings(&self) -> impl Iterator<Item = &Vec<f32>> {
        std::iter::once(&self.embedding).chain(self.extra.iter())
    }
}

/// 人物图库，JSON 持久化。
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Gallery {
    pub entries: Vec<GalleryEntry>,
}

impl Gallery {
    /// 从 JSON 加载；文件不存在时返回空图库。
    pub fn load(path: &Path) -> Result<Self> {
        if !path.is_file() {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(path)?;
        let gallery: Gallery =
            serde_json::from_str(&text).map_err(|e| Error::Config(format!("bad gallery json: {e}")))?;
        Ok(gallery)
    }

    /// 写出 JSON（pretty，便于人工查看）。
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        let text = serde_json::to_string_pretty(self)
            .map_err(|e| Error::Config(format!("failed to serialize gallery: {e}")))?;
        std::fs::write(path, text)?;
        Ok(())
    }

    /// 最优余弦相似度（对主嵌入与全部额外嵌入取最大）≥ threshold 时复用标签。
    pub fn find(&self, embedding: &[f32], threshold: f32) -> Option<(String, f32)> {
        let (index, score) = self.best_match(embedding)?;
        if score < threshold {
            return None;
        }
        Some((self.entries[index].label.clone(), score))
    }

    /// 返回 (条目下标, 最高相似度)。
    pub fn best_match(&self, embedding: &[f32]) -> Option<(usize, f32)> {
        self.entries
            .iter()
            .enumerate()
            .map(|(i, entry)| {
                let score = entry
                    .embeddings()
                    .map(|e| cosine(embedding, e))
                    .fold(f32::MIN, f32::max);
                (score, i)
            })
            .max_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal))
            .map(|(score, index)| (index, score))
    }

    /// 匹配成功后的增量更新：与主嵌入差异大且未超上限时追加额外嵌入。
    /// 返回是否发生了追加。
    pub fn absorb(&mut self, index: usize, embedding: Vec<f32>, min_sim: f32, max_extra: usize) -> bool {
        let Some(entry) = self.entries.get_mut(index) else {
            return false;
        };
        if entry.extra.len() >= max_extra {
            return false;
        }
        let primary_sim = cosine(&embedding, &entry.embedding);
        if primary_sim >= min_sim {
            return false;
        }
        entry.extra.push(embedding);
        true
    }

    /// 把 src 人物并入 dst（嵌入合并，src 条目删除）。成功返回 true。
    pub fn merge(&mut self, src: &str, dst: &str) -> bool {
        if src == dst {
            return false;
        }
        let (Some(src_index), Some(dst_index)) = (
            self.entries.iter().position(|e| e.label == src),
            self.entries.iter().position(|e| e.label == dst),
        ) else {
            return false;
        };
        let moved = self.entries.remove(src_index);
        // remove 后 dst 下标可能前移。
        let dst_index = dst_index - if src_index < dst_index { 1 } else { 0 };
        let entry = &mut self.entries[dst_index];
        entry.extra.push(moved.embedding);
        entry.extra.extend(moved.extra);
        true
    }

    /// 删除人物条目。成功返回 true。
    pub fn remove(&mut self, label: &str) -> bool {
        let before = self.entries.len();
        self.entries.retain(|e| e.label != label);
        self.entries.len() != before
    }

    /// 新增 `person_N` 并返回标签。
    pub fn add_person(&mut self, embedding: Vec<f32>) -> String {
        let mut n = 1;
        loop {
            let label = format!("person_{n}");
            if !self.entries.iter().any(|e| e.label == label) {
                self.entries.push(GalleryEntry::new(&label, embedding));
                return label;
            }
            n += 1;
        }
    }

    /// 重命名标签；成功返回 true。新标签与已有条目冲突时不改动。
    pub fn rename(&mut self, old: &str, new: &str) -> bool {
        if old == new || new.is_empty() || self.entries.iter().any(|e| e.label == new) {
            return false;
        }
        match self.entries.iter_mut().find(|e| e.label == old) {
            Some(entry) => {
                entry.label = new.to_string();
                true
            }
            None => false,
        }
    }
}

/// 把 `out_dir/src` 子目录中所有文件移入 `out_dir/dst`；同名冲突加 `__{src}` 后缀。
/// 返回移动的文件数；src 目录不存在返回 Ok(0)。
pub fn merge_person_dirs(out_dir: &Path, src: &str, dst: &str) -> Result<usize> {
    let src_dir = out_dir.join(src);
    let dst_dir = out_dir.join(dst);
    if !src_dir.is_dir() {
        return Ok(0);
    }
    std::fs::create_dir_all(&dst_dir)?;
    let mut moved = 0usize;
    let mut entries = std::fs::read_dir(&src_dir)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .collect::<Vec<_>>();
    entries.sort();
    for path in entries {
        if !path.is_file() {
            continue;
        }
        let name = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
        let mut target = dst_dir.join(&name);
        if target.exists() {
            let stem = Path::new(&name)
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy();
            let ext = Path::new(&name)
                .extension()
                .map(|e| format!(".{}", e.to_string_lossy()))
                .unwrap_or_default();
            target = dst_dir.join(format!("{stem}__{src}{ext}"));
        }
        std::fs::rename(&path, &target)?;
        moved += 1;
    }
    let _ = std::fs::remove_dir(&src_dir);
    Ok(moved)
}

/// 删除人物输出子目录；目录不存在返回 Ok(false)。
pub fn remove_person_dir(out_dir: &Path, label: &str) -> Result<bool> {
    let dir = out_dir.join(label);
    if !dir.is_dir() {
        return Ok(false);
    }
    std::fs::remove_dir_all(&dir)?;
    Ok(true)
}

/// 清洗人物标签：去掉路径分隔符等非法字符，空则回退 None。
pub fn sanitize_label(raw: &str) -> Option<String> {
    let cleaned: String = raw
        .trim()
        .chars()
        .filter(|c| !c.is_control() && !matches!(c, '/' | '\\' | ':'))
        .collect();
    if cleaned.is_empty() || cleaned == "." || cleaned == ".." {
        None
    } else {
        Some(cleaned)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit(v: &[f32]) -> Vec<f32> {
        let mut v = v.to_vec();
        l2_normalize(&mut v);
        v
    }

    #[test]
    fn cosine_of_same_direction_is_one() {
        let a = unit(&[1.0, 2.0, 3.0]);
        let b = unit(&[2.0, 4.0, 6.0]);
        assert!((cosine(&a, &b) - 1.0).abs() < 1e-5);
    }

    #[test]
    fn gallery_find_respects_threshold() {
        let mut gallery = Gallery::default();
        let emb = unit(&[1.0, 0.0, 0.0]);
        gallery.entries.push(GalleryEntry::new("alice", emb.clone()));
        assert_eq!(gallery.find(&emb, 0.35).map(|(l, _)| l), Some("alice".to_string()));
        let other = unit(&[0.0, 1.0, 0.0]);
        assert_eq!(gallery.find(&other, 0.35), None);
        // 相似度 √2/2 ≈ 0.707，阈值 0.4 命中、0.8 不命中。
        let mid = unit(&[1.0, 1.0, 0.0]);
        assert_eq!(gallery.find(&mid, 0.4).map(|(l, _)| l), Some("alice".to_string()));
        assert_eq!(gallery.find(&mid, 0.8), None);
    }

    #[test]
    fn gallery_add_and_rename() {
        let mut gallery = Gallery::default();
        assert_eq!(gallery.add_person(vec![0.1]), "person_1");
        assert_eq!(gallery.add_person(vec![0.2]), "person_2");
        assert!(gallery.rename("person_1", "alice"));
        assert_eq!(gallery.entries[0].label, "alice");
        // 冲突与不存在都失败。
        assert!(!gallery.rename("person_2", "alice"));
        assert!(!gallery.rename("ghost", "bob"));
        assert!(!gallery.rename("alice", "alice"));
    }

    #[test]
    fn gallery_json_roundtrip() {
        let mut gallery = Gallery::default();
        gallery.add_person(vec![0.3, 0.4]);
        let path = std::env::temp_dir().join(format!(
            "sepiola-gallery-{}-{}.json",
            std::process::id(),
            gallery.entries.len()
        ));
        gallery.save(&path).expect("save");
        let loaded = Gallery::load(&path).expect("load");
        assert_eq!(loaded.entries.len(), 1);
        assert_eq!(loaded.entries[0].label, "person_1");
        assert_eq!(loaded.entries[0].embedding, vec![0.3, 0.4]);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn gallery_extra_compat_and_find() {
        // 旧版 JSON（无 extra 字段）可正常加载。
        let json = r#"{"entries":[{"label":"a","embedding":[1.0,0.0]}]}"#;
        let gallery: Gallery = serde_json::from_str(json).expect("legacy json");
        assert!(gallery.entries[0].extra.is_empty());
        // find 对 extra 也生效。
        let mut gallery = Gallery::default();
        gallery.entries.push(GalleryEntry {
            label: "a".to_string(),
            embedding: unit(&[1.0, 0.0, 0.0]),
            extra: vec![unit(&[0.0, 1.0, 0.0])],
        });
        assert_eq!(
            gallery.find(&unit(&[0.0, 1.0, 0.0]), 0.9).map(|(l, _)| l),
            Some("a".to_string())
        );
    }

    #[test]
    fn gallery_absorb_rules() {
        let mut gallery = Gallery::default();
        gallery.entries.push(GalleryEntry::new("a", unit(&[1.0, 0.0, 0.0])));
        // 与主嵌入相似（cos=1 ≥ min_sim）不追加。
        assert!(!gallery.absorb(0, unit(&[1.0, 0.0, 0.0]), 0.5, 4));
        // 差异大的样本追加。
        assert!(gallery.absorb(0, unit(&[0.0, 1.0, 0.0]), 0.5, 4));
        assert_eq!(gallery.entries[0].extra.len(), 1);
        assert_eq!(
            gallery.find(&unit(&[0.0, 1.0, 0.0]), 0.9).map(|(l, _)| l),
            Some("a".to_string())
        );
        // 达到上限不再追加。
        assert!(!gallery.absorb(0, unit(&[0.0, 0.0, 1.0]), 0.5, 1));
    }

    #[test]
    fn gallery_merge_and_remove() {
        let mut gallery = Gallery::default();
        gallery.entries.push(GalleryEntry::new("a", unit(&[1.0, 0.0, 0.0])));
        gallery.entries.push(GalleryEntry::new("b", unit(&[0.0, 1.0, 0.0])));
        assert!(gallery.merge("b", "a"));
        assert_eq!(gallery.entries.len(), 1);
        assert_eq!(gallery.entries[0].extra.len(), 1);
        assert_eq!(
            gallery.find(&unit(&[0.0, 1.0, 0.0]), 0.9).map(|(l, _)| l),
            Some("a".to_string())
        );
        assert!(!gallery.merge("a", "a"));
        assert!(!gallery.merge("ghost", "a"));
        // dst 不存在时不改动。
        assert!(!gallery.merge("a", "ghost"));
        assert!(gallery.remove("a"));
        assert!(gallery.entries.is_empty());
        assert!(!gallery.remove("a"));
        // src 在 dst 之前：remove 后 dst 下标前移仍需正确并入。
        gallery.entries.push(GalleryEntry::new("a", unit(&[1.0, 0.0, 0.0])));
        gallery.entries.push(GalleryEntry::new("b", unit(&[0.0, 1.0, 0.0])));
        assert!(gallery.merge("a", "b"));
        assert_eq!(gallery.entries.len(), 1);
        assert_eq!(gallery.entries[0].label, "b");
        assert_eq!(
            gallery.find(&unit(&[1.0, 0.0, 0.0]), 0.9).map(|(l, _)| l),
            Some("b".to_string())
        );
    }

    #[test]
    fn merge_person_dirs_moves_and_suffixes_conflicts() {
        let base = std::env::temp_dir().join(format!("sepiola-mpd-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("src")).expect("src dir");
        std::fs::create_dir_all(base.join("dst")).expect("dst dir");
        std::fs::write(base.join("src/a.jpg"), b"1").expect("write");
        std::fs::write(base.join("src/b.jpg"), b"2").expect("write");
        std::fs::write(base.join("dst/a.jpg"), b"0").expect("write");
        assert_eq!(merge_person_dirs(&base, "src", "dst").expect("merge"), 2);
        assert!(base.join("dst/a.jpg").is_file());
        assert!(base.join("dst/a__src.jpg").is_file());
        assert!(base.join("dst/b.jpg").is_file());
        assert!(!base.join("src").exists());
        // src 目录不存在时返回 0。
        assert_eq!(merge_person_dirs(&base, "src", "dst").expect("noop"), 0);
        assert!(remove_person_dir(&base, "dst").expect("remove"));
        assert!(!remove_person_dir(&base, "dst").expect("second remove"));
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn crop_face_expands_and_clamps() {
        let img = ImageF32::new(100, 100).expect("image");
        let face = FaceBox {
            x: 0.45,
            y: 0.45,
            w: 0.1,
            h: 0.1,
        };
        let crop = crop_face(&img, &face);
        // 0.1 * 1.4 = 0.14 → 约 14px 见方。
        assert!((12..=16).contains(&crop.width), "got {}", crop.width);
        assert_eq!(crop.width, crop.height);

        // 贴边人脸裁剪不越界。
        let edge = FaceBox {
            x: 0.95,
            y: 0.0,
            w: 0.1,
            h: 0.1,
        };
        let crop = crop_face(&img, &edge);
        assert!(crop.x_within_bounds(100));
    }

    #[test]
    fn resize_exact_to_112() {
        let img = ImageF32::new(64, 32).expect("image");
        let out = resize_exact(&img, 112);
        assert_eq!((out.width, out.height), (112, 112));
    }

    #[test]
    fn label_sanitizer_rejects_path_parts() {
        assert_eq!(sanitize_label(" 张三 ").as_deref(), Some("张三"));
        assert_eq!(sanitize_label("a/b").as_deref(), Some("ab"));
        assert_eq!(sanitize_label("  "), None);
        assert_eq!(sanitize_label(".."), None);
    }

    impl ImageF32 {
        fn x_within_bounds(&self, max: u32) -> bool {
            self.width >= 1 && self.height >= 1 && self.width <= max && self.height <= max
        }
    }

    /// 真实模型回归测试：不同人嵌入余弦应低于阈值，亮度扰动后仍高于阈值。
    /// 模型缺失时跳过。
    #[test]
    fn embedding_discriminates_identities() {
        use crate::imageio;
        let model = crate::paths::resolve_asset("models/arcfaceresnet100-11-int8.onnx");
        if !model.is_file() {
            return;
        }
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("test_data")
            .join("portrait");
        let load_face = |name: &str| -> Result<Vec<f32>> {
            let path = dir.join(name);
            if !path.is_file() {
                return Err(Error::Model(format!("missing {name}")));
            }
            let img = imageio::load(&path)?;
            let settings = IdentitySettings {
                enabled: true,
                model: model.to_string_lossy().into_owned(),
                ..Default::default()
            };
            let id = FaceIdentifier::new(&settings)?;
            let face = FaceBox { x: 0.3, y: 0.2, w: 0.4, h: 0.4 };
            id.embed(&crop_face(&img, &face))
        };
        let (a, b, c) = match (
            load_face("men_1.jpg"),
            load_face("men_2.jpg"),
            load_face("women_1.jpg"),
        ) {
            (Ok(a), Ok(b), Ok(c)) => (a, b, c),
            _ => return,
        };
        // 不同人之间余弦应远低于阈值 0.35。
        assert!(cosine(&a, &b) < 0.35, "men_1 vs men_2 = {}", cosine(&a, &b));
        assert!(cosine(&a, &c) < 0.35, "men_1 vs women_1 = {}", cosine(&a, &c));
        // 同一人亮度扰动 10% 后仍应稳定匹配。
        let img = imageio::load(&dir.join("men_1.jpg")).expect("load");
        let mut bright = img.clone();
        for v in bright.data.iter_mut() {
            *v = (*v * 0.9).min(1.0);
        }
        let settings = IdentitySettings {
            enabled: true,
            model: model.to_string_lossy().into_owned(),
            ..Default::default()
        };
        let id = FaceIdentifier::new(&settings).expect("identifier");
        let face = FaceBox { x: 0.3, y: 0.2, w: 0.4, h: 0.4 };
        let emb = id.embed(&crop_face(&bright, &face)).expect("embed");
        assert!(cosine(&a, &emb) > 0.5, "same person bright = {}", cosine(&a, &emb));
    }

    /// 对齐效果验证：同一张脸、两种不同构图（检测框 vs 居中收缩 20% 框）。
    /// 普通裁剪会因缩放差异导致余弦偏低；关键点对齐后应显著归一化提升。
    /// 框必须来自真实检测器（居中于脸），否则关键点估计不可靠。
    /// 模型（ArcFace + 2d106det）或测试图缺失时跳过。
    #[test]
    fn alignment_normalizes_framing() {
        use crate::config::SceneSettings;
        use crate::imageio;
        use crate::scene::face::FaceClassifier;
        use crate::scene::SceneClassifier;
        let arc = crate::paths::resolve_asset("models/arcfaceresnet100-11-int8.onnx");
        let lm = crate::paths::resolve_asset("models/2d106det.onnx");
        if !arc.is_file() || !lm.is_file() {
            return;
        }
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("test_data")
            .join("portrait");
        let settings = IdentitySettings {
            enabled: true,
            model: arc.to_string_lossy().into_owned(),
            landmark_model: lm.to_string_lossy().into_owned(),
            ..Default::default()
        };
        let id = FaceIdentifier::new(&settings).expect("identifier");
        let classifier = FaceClassifier::new(&SceneSettings::default()).expect("classifier");
        // 正面（women_1）与偏转（men_1）各测一张。
        for name in ["women_1.jpg", "men_1.jpg"] {
            let path = dir.join(name);
            if !path.is_file() {
                continue;
            }
            let img = imageio::load(&path).expect("load");
            let info = classifier.classify(&img).expect("detect");
            let detected = info
                .faces
                .iter()
                .max_by(|a, b| a.area().partial_cmp(&b.area()).unwrap())
                .cloned()
                .expect("a face should be detected");
            // 居中收缩 20% 的框，模拟不同构图。
            let mut shrunk = detected;
            shrunk.w *= 0.8;
            shrunk.h *= 0.8;
            shrunk.x += detected.w * 0.1;
            shrunk.y += detected.h * 0.1;
            let plain = cosine(
                &id.embed(&crop_face(&img, &detected)).expect("embed"),
                &id.embed(&crop_face(&img, &shrunk)).expect("embed"),
            );
            let aligned = cosine(
                &id.embed(&id.aligned_face(&img, &detected)).expect("embed"),
                &id.embed(&id.aligned_face(&img, &shrunk)).expect("embed"),
            );
            eprintln!("{name} framing cosine: plain={plain:.3} aligned={aligned:.3}");
            assert!(
                aligned > plain,
                "{name}: alignment should raise cross-framing cosine: plain={plain:.3} aligned={aligned:.3}"
            );
        }
    }
}
