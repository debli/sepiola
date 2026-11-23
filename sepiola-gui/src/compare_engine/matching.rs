use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};

use super::images::{hash_buckets, similarity, similarity_from_distance};
use super::metadata::differences;
use super::{CompareItem, CompareMode, CompareReport, FileRecord};

pub(super) fn compare_records(
    a_records: Vec<FileRecord>,
    b_records: Vec<FileRecord>,
    mode: CompareMode,
    cancel: &AtomicBool,
    item_callback: &mut impl FnMut(CompareItem),
) -> CompareReport {
    let files_a = a_records.len();
    let files_b = b_records.len();
    let mut a_by_path: BTreeMap<_, _> = a_records
        .into_iter()
        .map(|record| (record.relative.clone(), record))
        .collect();
    let mut b_by_path: BTreeMap<_, _> = b_records
        .into_iter()
        .map(|record| (record.relative.clone(), record))
        .collect();
    let mut items = Vec::new();
    let common: Vec<_> = a_by_path
        .keys()
        .filter(|path| b_by_path.contains_key(*path))
        .cloned()
        .collect();
    for path in common {
        if cancel.load(Ordering::Relaxed) {
            return CompareReport {
                files_a,
                files_b,
                cancelled: true,
                ..Default::default()
            };
        }
        let a = a_by_path.remove(&path).expect("path came from A");
        let b = b_by_path.remove(&path).expect("path came from B");

        let size_match = a.size == b.size && a.size > 0;

        let mut details = differences(&a, &b, mode);
        let similarity = if mode == CompareMode::Images {
            a.difference_hash
                .zip(b.difference_hash)
                .map(|(left, right)| similarity(left, right))
        } else {
            None
        };

        let hash_matches = match (&a.hash, &b.hash) {
            (Some(a_hash), Some(b_hash)) if a_hash == b_hash => true,
            _ => false,
        };

        let exact_duplicate = hash_matches || (size_match && a.name == b.name);

        if mode == CompareMode::Images && !exact_duplicate && a.hash != b.hash {
            if size_match {
                details.push("大小相同但哈希不同".to_string());
            } else {
                details.push(format!(
                    "大小不同（{} vs {}）",
                    format_size(a.size),
                    format_size(b.size)
                ));
            }
        }

        if !details.is_empty() || exact_duplicate {
            let item = CompareItem {
                a: Some(a.relative),
                b: Some(b.relative),
                exact_duplicate,
                details,
                similarity,
            };
            item_callback(item.clone());
            items.push(item);
        }
    }

    let a_unmatched: Vec<_> = a_by_path.into_values().collect();
    let mut b_unmatched: Vec<_> = b_by_path.into_values().collect();

    let mut b_hashes: HashMap<&str, Vec<usize>> = HashMap::new();
    let mut b_size_name: HashMap<(u64, &str), Vec<usize>> = HashMap::new();
    for (index, record) in b_unmatched.iter().enumerate() {
        if let Some(hash) = record.hash.as_deref() {
            b_hashes.entry(hash).or_default().push(index);
        }
        b_size_name
            .entry((record.size, record.name.as_str()))
            .or_default()
            .push(index);
    }

    let mut paired_b = HashSet::new();
    let mut remaining_a = Vec::new();
    for record in a_unmatched {
        let match_index = record
            .hash
            .as_deref()
            .and_then(|hash| b_hashes.get(hash))
            .and_then(|indices| {
                indices
                    .iter()
                    .copied()
                    .find(|index| !paired_b.contains(index))
            })
            .or_else(|| {
                b_size_name
                    .get(&(record.size, record.name.as_str()))
                    .and_then(|indices| {
                        indices
                            .iter()
                            .copied()
                            .find(|index| !paired_b.contains(index))
                    })
            });

        if let Some(index) = match_index {
            paired_b.insert(index);
            let other = &b_unmatched[index];
            let item = CompareItem {
                a: Some(record.relative.clone()),
                b: Some(other.relative.clone()),
                exact_duplicate: true,
                details: vec!["大小相同，内容可能重复".to_string()],
                similarity: None,
            };
            item_callback(item.clone());
            items.push(item);
        } else {
            remaining_a.push(record);
        }
    }

    let remaining_b: Vec<_> = b_unmatched
        .drain(..)
        .enumerate()
        .filter_map(|(index, record)| (!paired_b.contains(&index)).then_some(record))
        .collect();

    if mode == CompareMode::Images {
        add_image_matches(
            &mut items,
            &remaining_a,
            &remaining_b,
            cancel,
            item_callback,
        );
        if cancel.load(Ordering::Relaxed) {
            return CompareReport {
                files_a,
                files_b,
                cancelled: true,
                ..Default::default()
            };
        }
    }

    for record in remaining_a {
        let item = CompareItem {
            a: Some(record.relative),
            b: None,
            details: vec!["仅 A 存在".to_string()],
            exact_duplicate: false,
            similarity: None,
        };
        item_callback(item.clone());
        items.push(item);
    }

    for record in remaining_b {
        let item = CompareItem {
            a: None,
            b: Some(record.relative),
            details: vec!["仅 B 存在".to_string()],
            exact_duplicate: false,
            similarity: None,
        };
        item_callback(item.clone());
        items.push(item);
    }

    items.sort_by(|left, right| left.a.cmp(&right.a).then(left.b.cmp(&right.b)));

    CompareReport {
        items,
        files_a,
        files_b,
        cancelled: false,
    }
}

fn add_image_matches(
    items: &mut Vec<CompareItem>,
    remaining_a: &[FileRecord],
    remaining_b: &[FileRecord],
    cancel: &AtomicBool,
    item_callback: &mut impl FnMut(CompareItem),
) {
    let mut buckets: HashMap<(u8, u8), Vec<usize>> = HashMap::new();
    for (index, record) in remaining_b.iter().enumerate() {
        if let Some(hash) = record.difference_hash {
            for key in hash_buckets(hash) {
                buckets.entry(key).or_default().push(index);
            }
        }
    }
    for a in remaining_a {
        let Some(hash) = a.difference_hash else {
            continue;
        };
        let candidates: HashSet<_> = hash_buckets(hash)
            .into_iter()
            .filter_map(|key| buckets.get(&key))
            .flatten()
            .copied()
            .collect();
        for index in candidates {
            if cancel.load(Ordering::Relaxed) {
                return;
            }
            let b = &remaining_b[index];
            let Some(other_hash) = b.difference_hash else {
                continue;
            };
            let distance = (hash ^ other_hash).count_ones();
            if distance <= 10 {
                let item = CompareItem {
                    a: Some(a.relative.clone()),
                    b: Some(b.relative.clone()),
                    exact_duplicate: false,
                    details: vec!["感知相似候选（仅供检查，不可用于自动删除）".to_string()],
                    similarity: Some(similarity_from_distance(distance)),
                };
                item_callback(item.clone());
                items.push(item);
            }
        }
    }
}

fn format_size(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB"];
    let mut size = bytes as f64;
    let mut unit_idx = 0;
    while size >= 1024.0 && unit_idx < UNITS.len() - 1 {
        size /= 1024.0;
        unit_idx += 1;
    }
    if unit_idx == 0 {
        format!("{} {}", bytes, UNITS[unit_idx])
    } else {
        format!("{:.1} {}", size, UNITS[unit_idx])
    }
}
