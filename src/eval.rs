//! 人物识别评测：按真实人物目录统计嵌入，做相似度阈值扫描。
//!
//! 评测集目录结构为 `<root>/<真实人物标签>/<图片>`（如 `test_data/identity/`）。
//! 逐图取最大人脸（与生产流程一致）做对齐嵌入，再对所有样本两两比较，给出每个
//! 阈值下的精确率/召回率、聚类数、误合并与拆分人物数，用于选择稳健的
//! `[identity] threshold`。未检出人脸或推理失败的图片不参与扫描，只在跳过清单中报告。

use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::identity::{cosine, FaceIdentifier};
use crate::imageio;
use crate::scene::face::FaceClassifier;
use crate::scene::SceneClassifier;

/// 参与阈值扫描的单人样本。
pub struct Sample {
    pub label: String,
    pub path: PathBuf,
    pub embedding: Vec<f32>,
}

/// 未能参与扫描的图片及原因（未检出人脸、多人合影、读取或推理失败）。
pub struct Skipped {
    pub path: PathBuf,
    pub reason: String,
}

pub struct Dataset {
    pub samples: Vec<Sample>,
    pub skipped: Vec<Skipped>,
    /// 检出多张人脸的样本数（按最大人脸参与扫描，真实标签可能不精确）。
    pub multi_face: usize,
}

impl Dataset {
    /// 评测集中出现过的真实人物数。
    pub fn person_count(&self) -> usize {
        let mut labels: Vec<&str> = self.samples.iter().map(|s| s.label.as_str()).collect();
        labels.sort_unstable();
        labels.dedup();
        labels.len()
    }
}

/// 读取 `<root>/<人物标签>/<图片>`，检测最大人脸并提取嵌入。
pub fn load_dataset(
    root: &Path,
    detector: &FaceClassifier,
    identifier: &FaceIdentifier,
) -> Result<Dataset> {
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(root)?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.is_dir())
        .collect();
    dirs.sort();
    if dirs.is_empty() {
        return Err(Error::Config(format!(
            "评测集目录下没有人物子目录：{}",
            root.display()
        )));
    }

    let mut samples = Vec::new();
    let mut skipped = Vec::new();
    let mut multi_face = 0usize;
    for dir in dirs {
        let label = dir
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
            .to_string();
        let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)?
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .filter(|path| path.is_file())
            .collect();
        files.sort();
        for path in files {
            match embed_image(&path, detector, identifier) {
                Ok((embedding, face_count)) => {
                    if face_count > 1 {
                        multi_face += 1;
                    }
                    samples.push(Sample {
                        label: label.clone(),
                        path,
                        embedding,
                    });
                }
                Err(reason) => skipped.push(Skipped { path, reason }),
            }
        }
    }
    Ok(Dataset {
        samples,
        skipped,
        multi_face,
    })
}

/// 单图推理：取最大人脸（与生产流程一致），返回对齐后的 L2 归一化嵌入与检出人脸数。
fn embed_image(
    path: &Path,
    detector: &FaceClassifier,
    identifier: &FaceIdentifier,
) -> std::result::Result<(Vec<f32>, usize), String> {
    let image = imageio::load(path).map_err(|error| format!("读取失败: {error}"))?;
    let info = detector
        .classify(&image)
        .map_err(|error| format!("检测失败: {error}"))?;
    let Some(face) = info.faces.iter().max_by(|a, b| {
        a.area()
            .partial_cmp(&b.area())
            .unwrap_or(std::cmp::Ordering::Equal)
    }) else {
        return Err("未检出人脸".to_string());
    };
    let embedding = identifier
        .embedding_for(&image, face)
        .map_err(|error| format!("嵌入失败: {error}"))?;
    Ok((embedding, info.faces.len()))
}

/// 单个阈值下的评测指标。
pub struct Metrics {
    pub threshold: f32,
    /// 真实同人对数 / 真实不同人对数。
    pub pairs_same: usize,
    pub pairs_diff: usize,
    pub true_positive: usize,
    pub false_positive: usize,
    pub false_negative: usize,
    pub precision: f32,
    pub recall: f32,
    pub f1: f32,
    /// 按「相似度 ≥ 阈值」连通的聚类数。
    pub clusters: usize,
    /// 簇内包含多个真实人物（误合并）的簇数。
    pub merged_persons: usize,
    /// 真实人物被拆到多个簇（拆分）的人物数。
    pub split_persons: usize,
}

/// 在给定阈值下两两比较全部样本。
///
/// 精确率/召回率按“同人对”定义；无预测正例时精确率记 1.0，无真实同人时召回率记 1.0。
pub fn evaluate(samples: &[Sample], threshold: f32) -> Metrics {
    let n = samples.len();
    let mut union = UnionFind::new(n);
    let mut metrics = Metrics {
        threshold,
        pairs_same: 0,
        pairs_diff: 0,
        true_positive: 0,
        false_positive: 0,
        false_negative: 0,
        precision: 1.0,
        recall: 1.0,
        f1: 1.0,
        clusters: n,
        merged_persons: 0,
        split_persons: 0,
    };

    for i in 0..n {
        for j in (i + 1)..n {
            let same_truth = samples[i].label == samples[j].label;
            if same_truth {
                metrics.pairs_same += 1;
            } else {
                metrics.pairs_diff += 1;
            }
            let predicted = cosine(&samples[i].embedding, &samples[j].embedding) >= threshold;
            if predicted {
                union.union(i, j);
            }
            match (same_truth, predicted) {
                (true, true) => metrics.true_positive += 1,
                (true, false) => metrics.false_negative += 1,
                (false, true) => metrics.false_positive += 1,
                (false, false) => {}
            }
        }
    }

    let predicted_positive = metrics.true_positive + metrics.false_positive;
    if predicted_positive > 0 {
        metrics.precision = metrics.true_positive as f32 / predicted_positive as f32;
    }
    let actual_positive = metrics.true_positive + metrics.false_negative;
    if actual_positive > 0 {
        metrics.recall = metrics.true_positive as f32 / actual_positive as f32;
    }
    let sum = metrics.precision + metrics.recall;
    metrics.f1 = if sum > 0.0 {
        2.0 * metrics.precision * metrics.recall / sum
    } else {
        0.0
    };

    let labels: Vec<String> = samples.iter().map(|sample| sample.label.clone()).collect();
    metrics.clusters = union.cluster_count();
    metrics.merged_persons = union.merged_clusters(&labels);
    metrics.split_persons = union.split_labels(&labels);
    metrics
}

/// 从 `start` 到 `end`（含端点）按 `step` 扫描阈值。
pub fn sweep(samples: &[Sample], start: f32, end: f32, step: f32) -> Vec<Metrics> {
    let steps = if step <= 0.0 {
        1
    } else {
        ((end - start) / step).round().max(0.0) as usize
    };
    (0..=steps)
        .map(|index| evaluate(samples, start + index as f32 * step))
        .collect()
}

/// F1 最高的结果；并列时取阈值更大者（更保守，更少误合并）。
pub fn best_by_f1(rows: &[Metrics]) -> Option<&Metrics> {
    rows.iter()
        .max_by(|a, b| {
            a.f1
                .partial_cmp(&b.f1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(
                    a.threshold
                        .partial_cmp(&b.threshold)
                        .unwrap_or(std::cmp::Ordering::Equal),
                )
        })
}

/// 并查集：统计预测聚类数、误合并簇数与被拆分的人物数。
struct UnionFind {
    parent: Vec<usize>,
    rank: Vec<usize>,
}

impl UnionFind {
    fn new(n: usize) -> Self {
        Self {
            parent: (0..n).collect(),
            rank: vec![0; n],
        }
    }

    fn find(&mut self, mut x: usize) -> usize {
        while self.parent[x] != x {
            self.parent[x] = self.parent[self.parent[x]];
            x = self.parent[x];
        }
        x
    }

    fn union(&mut self, a: usize, b: usize) {
        let (ra, rb) = (self.find(a), self.find(b));
        if ra == rb {
            return;
        }
        match self.rank[ra].cmp(&self.rank[rb]) {
            std::cmp::Ordering::Less => self.parent[ra] = rb,
            std::cmp::Ordering::Greater => self.parent[rb] = ra,
            std::cmp::Ordering::Equal => {
                self.parent[rb] = ra;
                self.rank[ra] += 1;
            }
        }
    }

    /// 每个簇内的真实标签集合，按根聚合。
    fn label_sets(&mut self, labels: &[String]) -> Vec<Vec<String>> {
        let mut groups: Vec<(usize, Vec<String>)> = Vec::new();
        for (index, label) in labels.iter().enumerate() {
            let root = self.find(index);
            match groups.iter_mut().find(|(r, _)| *r == root) {
                Some((_, set)) => set.push(label.clone()),
                None => groups.push((root, vec![label.clone()])),
            }
        }
        groups.into_iter().map(|(_, set)| set).collect()
    }

    fn cluster_count(&mut self) -> usize {
        (0..self.parent.len())
            .map(|index| self.find(index))
            .collect::<std::collections::BTreeSet<_>>()
            .len()
    }

    /// 簇内出现 >1 个真实人物的簇数（误合并）。
    fn merged_clusters(&mut self, labels: &[String]) -> usize {
        self.label_sets(labels)
            .iter()
            .filter(|set| set.iter().collect::<std::collections::BTreeSet<_>>().len() > 1)
            .count()
    }

    /// 真实人物被拆到多个簇的人数（拆分）。
    fn split_labels(&mut self, labels: &[String]) -> usize {
        let sets = self.label_sets(labels);
        let mut names: Vec<String> = labels.to_vec();
        names.sort_unstable();
        names.dedup();
        names
            .iter()
            .filter(|name| {
                sets.iter()
                    .filter(|set| set.iter().any(|label| label == *name))
                    .count()
                    > 1
            })
            .count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(label: &str, embedding: [f32; 3]) -> Sample {
        Sample {
            label: label.to_string(),
            path: PathBuf::from(format!("{label}.jpg")),
            embedding: embedding.to_vec(),
        }
    }

    /// A 两人（0.8 余弦）+ B：阈值 0.7 时完全正确。
    fn dataset() -> Vec<Sample> {
        vec![
            sample("A", [1.0, 0.0, 0.0]),
            sample("A", [0.8, 0.6, 0.0]),
            sample("B", [0.0, 1.0, 0.0]),
        ]
    }

    #[test]
    fn clean_threshold_separates_everyone() {
        let metrics = evaluate(&dataset(), 0.7);
        assert_eq!(metrics.pairs_same, 1);
        assert_eq!(metrics.pairs_diff, 2);
        assert_eq!(metrics.true_positive, 1);
        assert_eq!(metrics.false_positive, 0);
        assert_eq!(metrics.false_negative, 0);
        assert_eq!(metrics.precision, 1.0);
        assert_eq!(metrics.recall, 1.0);
        assert_eq!(metrics.clusters, 2);
        assert_eq!(metrics.merged_persons, 0);
        assert_eq!(metrics.split_persons, 0);
    }

    #[test]
    fn low_threshold_merges_distinct_persons() {
        let metrics = evaluate(&dataset(), 0.5);
        // (A2,B) 余弦 0.6 ≥ 0.5，被误并入同一簇。
        assert_eq!(metrics.true_positive, 1);
        assert_eq!(metrics.false_positive, 1);
        assert_eq!(metrics.false_negative, 0);
        assert_eq!(metrics.precision, 0.5);
        assert_eq!(metrics.recall, 1.0);
        assert_eq!(metrics.clusters, 1);
        assert_eq!(metrics.merged_persons, 1);
        assert_eq!(metrics.split_persons, 0);
    }

    #[test]
    fn high_threshold_splits_same_person() {
        let metrics = evaluate(&dataset(), 0.9);
        assert_eq!(metrics.true_positive, 0);
        assert_eq!(metrics.false_positive, 0);
        assert_eq!(metrics.false_negative, 1);
        assert_eq!(metrics.precision, 1.0);
        assert_eq!(metrics.recall, 0.0);
        assert_eq!(metrics.clusters, 3);
        assert_eq!(metrics.merged_persons, 0);
        assert_eq!(metrics.split_persons, 1);
    }

    #[test]
    fn sweep_covers_endpoints_and_picks_best_f1() {
        let rows = sweep(&dataset(), 0.5, 0.7, 0.1);
        assert_eq!(rows.len(), 3);
        assert!((rows[0].threshold - 0.5).abs() < 1e-4);
        assert!((rows[2].threshold - 0.7).abs() < 1e-4);
        let best = best_by_f1(&rows).expect("non-empty");
        assert!((best.threshold - 0.7).abs() < 1e-4);
        assert!((best.f1 - 1.0).abs() < 1e-4);
    }

    #[test]
    fn person_count_dedups_labels() {
        let multi = Dataset {
            samples: dataset(),
            skipped: Vec::new(),
            multi_face: 0,
        };
        assert_eq!(multi.person_count(), 2);
    }
}
