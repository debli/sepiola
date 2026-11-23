//! 记忆卡片笔记本（M47）：卡片模型、遗忘曲线复习队列、复习记录与统计。
//!
//! 数据存于库根 `.sepiola/notebook.json`，写入使用临时文件 + rename；文件损坏时
//! 先备份再回退空表，避免静默覆盖用户数据。

pub mod ai;
pub mod schedule;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use chrono::{Local, NaiveDate, TimeZone};
use serde::{Deserialize, Serialize};

use crate::library::LIBRARY_DIR;
use schedule::Rating;

/// 每日新卡上限。
pub const DAILY_NEW_LIMIT: usize = 20;
/// 每张卡片保留的复习记录条数。
pub const MAX_HISTORY: usize = 200;
/// 默认 ease。
const DEFAULT_EASE: f32 = 2.5;

/// 一次复习记录。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ReviewLog {
    pub at: i64,
    pub rating: Rating,
    pub prev_interval_days: f32,
    pub next_interval_days: f32,
}

/// 最近一次造句批改。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PracticeRecord {
    pub at: i64,
    pub sentence: String,
    pub ok: bool,
    pub corrected: String,
    pub comment: String,
    pub issues: Vec<ai::SentenceIssue>,
}

/// 一张记忆卡片。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct NoteCard {
    pub id: u64,
    /// 正面：待记忆的词/短语/问题。
    pub front: String,
    /// 背面：答案或释义。
    pub back: String,
    pub tags: Vec<String>,
    pub created_at: i64,
    pub updated_at: i64,
    pub due_at: i64,
    pub interval_days: f32,
    pub ease: f32,
    pub repetitions: u32,
    pub lapses: u32,
    pub last_reviewed_at: Option<i64>,
    pub history: Vec<ReviewLog>,
    pub practice: Option<PracticeRecord>,
}

impl Default for NoteCard {
    fn default() -> Self {
        Self {
            id: 0,
            front: String::new(),
            back: String::new(),
            tags: Vec::new(),
            created_at: 0,
            updated_at: 0,
            due_at: 0,
            interval_days: 0.0,
            ease: DEFAULT_EASE,
            repetitions: 0,
            lapses: 0,
            last_reviewed_at: None,
            history: Vec::new(),
            practice: None,
        }
    }
}

/// 笔记本全量数据。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Notebook {
    pub next_id: u64,
    pub cards: Vec<NoteCard>,
}

impl Default for Notebook {
    fn default() -> Self {
        Self {
            next_id: 1,
            cards: Vec::new(),
        }
    }
}

/// 统计面板数据。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct NotebookStats {
    pub due: usize,
    pub new_cards: usize,
    pub reviewed_today: usize,
    pub streak: u32,
    /// 最近 30 天的复习次数，标签为 `MM-DD`，按时间升序。
    pub daily: Vec<(String, usize)>,
}

/// 笔记本文件路径。
pub fn notebook_path(root: &Path) -> PathBuf {
    root.join(LIBRARY_DIR).join("notebook.json")
}

pub fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}

impl Notebook {
    pub fn load(root: &Path) -> Self {
        let path = notebook_path(root);
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(_) => return Self::default(),
        };
        match serde_json::from_str::<Self>(&text) {
            Ok(mut notebook) => {
                notebook.normalize();
                notebook
            }
            Err(_) => {
                backup_corrupt(&path);
                Self::default()
            }
        }
    }

    pub fn save(&self, root: &Path) -> Result<(), String> {
        let path = notebook_path(root);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| format!("创建 {} 失败：{error}", parent.display()))?;
        }
        let text = serde_json::to_string_pretty(self)
            .map_err(|error| format!("序列化笔记本失败：{error}"))?;
        let parent = path.parent().unwrap_or(Path::new("."));
        let temporary = parent.join(format!(".notebook-{}.tmp", std::process::id()));
        std::fs::write(&temporary, text)
            .map_err(|error| format!("写入 {} 失败：{error}", temporary.display()))?;
        match std::fs::rename(&temporary, &path) {
            Ok(()) => Ok(()),
            Err(_error) if path.exists() => {
                std::fs::remove_file(&path)
                    .map_err(|error| format!("替换 {} 失败：{error}", path.display()))?;
                std::fs::rename(&temporary, &path)
                    .map_err(|error| format!("写入 {} 失败：{error}", path.display()))
            }
            Err(error) => {
                let _ = std::fs::remove_file(&temporary);
                Err(format!("写入 {} 失败：{error}", path.display()))
            }
        }
    }

    fn normalize(&mut self) {
        let max_id = self.cards.iter().map(|card| card.id).max().unwrap_or(0);
        self.next_id = self.next_id.max(max_id + 1).max(1);
        for card in &mut self.cards {
            if card.ease < 1.3 {
                card.ease = DEFAULT_EASE;
            }
            if card.interval_days < 0.0 {
                card.interval_days = 0.0;
            }
            if card.history.len() > MAX_HISTORY {
                let keep = card.history.len() - MAX_HISTORY;
                card.history.drain(..keep);
            }
        }
    }

    pub fn add(&mut self, front: &str, back: &str, tags: Vec<String>, now: i64) -> u64 {
        let id = self.next_id.max(1);
        self.next_id = id + 1;
        self.cards.push(NoteCard {
            id,
            front: front.trim().to_string(),
            back: back.trim().to_string(),
            tags: normalize_tags(tags),
            created_at: now,
            updated_at: now,
            due_at: now,
            ..NoteCard::default()
        });
        id
    }

    pub fn card(&self, id: u64) -> Option<&NoteCard> {
        self.cards.iter().find(|card| card.id == id)
    }

    /// 正面文本归一化后相同的卡片 id；用于加入卡片前的程序去重。
    pub fn find_duplicate(&self, front: &str) -> Option<u64> {
        let candidate = normalize_front(front);
        if candidate.is_empty() {
            return None;
        }
        self.cards
            .iter()
            .find(|card| normalize_front(&card.front) == candidate)
            .map(|card| card.id)
    }

    pub fn card_mut(&mut self, id: u64) -> Option<&mut NoteCard> {
        self.cards.iter_mut().find(|card| card.id == id)
    }

    pub fn update(&mut self, id: u64, front: &str, back: &str, tags: Vec<String>, now: i64) -> bool {
        let Some(card) = self.card_mut(id) else {
            return false;
        };
        card.front = front.trim().to_string();
        card.back = back.trim().to_string();
        card.tags = normalize_tags(tags);
        card.updated_at = now;
        true
    }

    pub fn remove(&mut self, id: u64) -> bool {
        let before = self.cards.len();
        self.cards.retain(|card| card.id != id);
        before != self.cards.len()
    }

    /// 记录一次复习；返回新的到期时间。
    pub fn review(&mut self, id: u64, rating: Rating, now: i64) -> Result<i64, String> {
        let Some(card) = self.card_mut(id) else {
            return Err("卡片不存在".to_string());
        };
        let (prev, next) = schedule::apply(card, rating, now);
        card.history.push(ReviewLog {
            at: now,
            rating,
            prev_interval_days: prev,
            next_interval_days: next,
        });
        if card.history.len() > MAX_HISTORY {
            let keep = card.history.len() - MAX_HISTORY;
            card.history.drain(..keep);
        }
        Ok(card.due_at)
    }

    pub fn record_practice(&mut self, id: u64, record: PracticeRecord) -> bool {
        let Some(card) = self.card_mut(id) else {
            return false;
        };
        card.practice = Some(record);
        true
    }

    /// 已到期的旧卡（不含新卡），按到期时间升序。
    pub fn due_cards(&self, now: i64) -> Vec<&NoteCard> {
        let mut due: Vec<&NoteCard> = self
            .cards
            .iter()
            .filter(|card| card.last_reviewed_at.is_some() && card.due_at <= now)
            .collect();
        due.sort_by_key(|card| (card.due_at, card.id));
        due
    }

    /// 未学习的新卡，按创建时间升序。
    pub fn new_cards(&self) -> Vec<&NoteCard> {
        let mut new: Vec<&NoteCard> = self
            .cards
            .iter()
            .filter(|card| card.last_reviewed_at.is_none())
            .collect();
        new.sort_by_key(|card| (card.created_at, card.id));
        new
    }

    /// 今日复习队列：到期旧卡优先，新卡按每日上限补充。
    pub fn queue(&self, now: i64) -> Vec<u64> {
        let mut ids: Vec<u64> = self.due_cards(now).iter().map(|card| card.id).collect();
        ids.extend(
            self.new_cards()
                .into_iter()
                .take(DAILY_NEW_LIMIT)
                .map(|card| card.id),
        );
        ids
    }

    pub fn due_count(&self, now: i64) -> usize {
        self.due_cards(now).len()
    }

    pub fn all_tags(&self) -> Vec<String> {
        let mut tags: BTreeSet<String> = BTreeSet::new();
        for card in &self.cards {
            for tag in &card.tags {
                tags.insert(tag.clone());
            }
        }
        tags.into_iter().collect()
    }

    /// 计算统计面板数据（按本地日期）。
    pub fn stats(&self, now: i64) -> NotebookStats {
        let today = local_date(now);
        let mut per_day: BTreeMap<NaiveDate, usize> = BTreeMap::new();
        let mut reviewed_today = 0;
        for card in &self.cards {
            for log in &card.history {
                let date = local_date(log.at);
                *per_day.entry(date).or_default() += 1;
            }
        }
        if let Some(count) = per_day.get(&today) {
            reviewed_today = *count;
        }
        let mut streak = 0;
        let mut cursor = if per_day.contains_key(&today) {
            today
        } else {
            today - chrono::Duration::days(1)
        };
        while per_day.contains_key(&cursor) {
            streak += 1;
            cursor -= chrono::Duration::days(1);
        }
        let daily = (0..30)
            .rev()
            .map(|offset| {
                let date = today - chrono::Duration::days(offset);
                (
                    date.format("%m-%d").to_string(),
                    per_day.get(&date).copied().unwrap_or(0),
                )
            })
            .collect();
        NotebookStats {
            due: self.due_count(now),
            new_cards: self.new_cards().len(),
            reviewed_today,
            streak,
            daily,
        }
    }
}

fn local_date(timestamp: i64) -> NaiveDate {
    Local
        .timestamp_opt(timestamp, 0)
        .single()
        .map(|time| time.date_naive())
        .unwrap_or_else(|| Local::now().date_naive())
}

fn normalize_tags(tags: Vec<String>) -> Vec<String> {
    let mut seen = BTreeSet::new();
    tags.into_iter()
        .map(|tag| tag.trim().to_string())
        .filter(|tag| !tag.is_empty() && seen.insert(tag.clone()))
        .collect()
}

/// 卡片正面的去重键：忽略大小写、标点与多余空白。
pub fn normalize_front(text: &str) -> String {
    text.chars()
        .filter_map(|ch| {
            if ch.is_alphanumeric() {
                Some(ch.to_ascii_lowercase())
            } else if ch.is_whitespace() {
                Some(' ')
            } else {
                None
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn backup_corrupt(path: &Path) {
    let stamp = now_unix();
    let backup = path.with_file_name(format!("notebook.corrupt-{stamp}.json"));
    let _ = std::fs::copy(path, backup);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!("sepiola-notebook-{tag}-{nanos}"));
        std::fs::create_dir_all(&dir).expect("mkdir");
        dir
    }

    #[test]
    fn add_review_and_round_trip() {
        let dir = temp_dir("round-trip");
        let mut notebook = Notebook::default();
        let id = notebook.add("take off", "起飞", vec!["phrasal".into(), " phrasal ".into()], 100);
        assert_eq!(id, 1);
        assert_eq!(notebook.cards[0].tags, ["phrasal"]);
        notebook
            .review(id, Rating::Good, 200)
            .expect("review");
        notebook.save(&dir).expect("save");
        let mut loaded = Notebook::load(&dir);
        assert_eq!(loaded.cards.len(), 1);
        assert_eq!(loaded.cards[0].last_reviewed_at, Some(200));
        assert_eq!(loaded.cards[0].history.len(), 1);
        assert_eq!(loaded.next_id, 2);
        let next = loaded.add("second", "第二", Vec::new(), 300);
        assert_eq!(next, 2, "id 不复用");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn queue_puts_due_before_new_and_limits_new_cards() {
        let mut notebook = Notebook::default();
        for index in 0..(DAILY_NEW_LIMIT + 5) {
            notebook.add(&format!("front {index}"), "back", Vec::new(), index as i64);
        }
        let first = notebook.cards[0].id;
        notebook.review(first, Rating::Again, 10).expect("review");
        let queue = notebook.queue(1000);
        assert_eq!(queue[0], first, "到期旧卡优先");
        assert_eq!(
            queue.len(),
            1 + DAILY_NEW_LIMIT,
            "新卡受每日上限约束"
        );
    }

    #[test]
    fn corrupt_file_is_backed_up_and_reset() {
        let dir = temp_dir("corrupt");
        let path = notebook_path(&dir);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(&path, b"not-json").expect("write");
        let notebook = Notebook::load(&dir);
        assert!(notebook.cards.is_empty());
        let backup = std::fs::read_dir(path.parent().expect("parent"))
            .expect("read dir")
            .filter_map(Result::ok)
            .any(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("notebook.corrupt-")
            });
        assert!(backup, "损坏文件应保留备份");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn stats_count_today_and_streak() {
        let now = now_unix();
        let mut notebook = Notebook::default();
        let id = notebook.add("a", "b", Vec::new(), now);
        notebook.review(id, Rating::Good, now).expect("review");
        let stats = notebook.stats(now);
        assert_eq!(stats.reviewed_today, 1);
        assert_eq!(stats.streak, 1);
        assert_eq!(stats.daily.len(), 30);
        assert_eq!(stats.new_cards, 0);
    }

    #[test]
    fn update_and_remove_cards() {
        let mut notebook = Notebook::default();
        let id = notebook.add("a", "b", Vec::new(), 1);
        assert!(notebook.update(id, "c", "d", vec!["x".into()], 2));
        let card = notebook.card(id).expect("card");
        assert_eq!(card.front, "c");
        assert_eq!(card.tags, ["x"]);
        assert!(notebook.remove(id));
        assert!(!notebook.remove(id));
    }

    #[test]
    fn duplicate_detection_ignores_case_punctuation_and_spacing() {
        let mut notebook = Notebook::default();
        let id = notebook.add("Take off", "remove", Vec::new(), 1);
        assert_eq!(notebook.find_duplicate(" take   OFF. "), Some(id));
        assert_eq!(notebook.find_duplicate("takeoff"), None);
        assert_eq!(notebook.find_duplicate("   "), None);
        assert_eq!(notebook.find_duplicate("Take out"), None);
    }
}
