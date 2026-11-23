use super::{CompareMode, FileRecord};
use lofty::file::TaggedFileExt;
use lofty::tag::ItemKey;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct AudioMeta {
    pub readable: bool,
    pub artist: Option<String>,
    pub recording_date: Option<String>,
    pub conductor: Option<String>,
}

pub(super) fn audio_meta(path: &std::path::Path) -> AudioMeta {
    let Ok(tagged) = lofty::read_from_path(path) else {
        return AudioMeta::default();
    };
    let value = |key| {
        tagged
            .tags()
            .iter()
            .find_map(|tag| tag.get_string(&key).map(str::to_owned))
            .filter(|value| !value.trim().is_empty())
    };
    AudioMeta {
        readable: tagged.tags().iter().any(|tag| tag.item_count() > 0),
        artist: value(ItemKey::TrackArtist),
        recording_date: value(ItemKey::RecordingDate),
        conductor: value(ItemKey::Conductor),
    }
}

pub(super) fn differences(a: &FileRecord, b: &FileRecord, mode: CompareMode) -> Vec<String> {
    let mut differences = Vec::new();
    if mode == CompareMode::Files || mode == CompareMode::Books {
        if a.name != b.name {
            differences.push(format!("名称：{} ≠ {}", a.name, b.name));
        }
        if a.size != b.size {
            differences.push(format!(
                "大小：{} ≠ {}",
                format_size(a.size),
                format_size(b.size)
            ));
        }
        if a.modified != b.modified {
            differences.push("修改时间不同".to_string());
        }
        if a.hash != b.hash && a.size == b.size {
            differences.push("内容哈希不同（大小相同）".to_string());
        }
    }
    if mode == CompareMode::Books {
        let left = a.book.as_ref().cloned().unwrap_or_default();
        let right = b.book.as_ref().cloned().unwrap_or_default();
        compare_field(&mut differences, "书名", left.title, right.title);
        compare_field(&mut differences, "作者", left.author, right.author);
        compare_field(&mut differences, "出版社", left.publisher, right.publisher);
        compare_field(&mut differences, "版次", left.edition, right.edition);
        compare_field(&mut differences, "译者", left.translator, right.translator);
    }
    if mode == CompareMode::Music {
        let left = a.audio.as_ref().cloned().unwrap_or_default();
        let right = b.audio.as_ref().cloned().unwrap_or_default();
        if left.readable != right.readable {
            differences.push(format!(
                "可提取元信息：{} ≠ {}",
                yes_no(left.readable),
                yes_no(right.readable)
            ));
        }
        compare_field(&mut differences, "作者", left.artist, right.artist);
        compare_field(
            &mut differences,
            "录音时间",
            left.recording_date,
            right.recording_date,
        );
        compare_field(&mut differences, "指挥", left.conductor, right.conductor);
    }
    differences
}

fn compare_field(differences: &mut Vec<String>, label: &str, a: Option<String>, b: Option<String>) {
    if a != b {
        differences.push(format!(
            "{label}：{} ≠ {}",
            display_value(a),
            display_value(b)
        ));
    }
}

fn display_value(value: Option<String>) -> String {
    value.unwrap_or_else(|| "—".to_string())
}

fn yes_no(value: bool) -> &'static str {
    if value {
        "可"
    } else {
        "不可"
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
