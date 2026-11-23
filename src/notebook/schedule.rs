//! 遗忘曲线调度（M47）：四档评分 → 下次间隔与 ease。

use serde::{Deserialize, Serialize};

use crate::notebook::NoteCard;

/// 复习评分。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Rating {
    Again,
    Hard,
    Good,
    Easy,
}

impl Rating {
    pub const ALL: [Self; 4] = [Self::Again, Self::Hard, Self::Good, Self::Easy];

    pub fn label(self) -> &'static str {
        match self {
            Self::Again => "忘了",
            Self::Hard => "困难",
            Self::Good => "一般",
            Self::Easy => "轻松",
        }
    }
}

/// 一次评分后的调度结果。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NextSchedule {
    pub interval_days: f32,
    pub ease: f32,
    pub repetitions: u32,
    pub due_at: i64,
}

const AGAIN_MINUTES: f32 = 10.0;
const MAX_INTERVAL_DAYS: f32 = 365.0;
const MIN_EASE: f32 = 1.3;
const MAX_EASE: f32 = 3.0;
const DEFAULT_EASE: f32 = 2.5;

fn minutes(days: f32) -> f32 {
    days * 24.0 * 60.0
}

/// 计算评分后的间隔；不修改卡片。
pub fn next(card: &NoteCard, rating: Rating, now: i64) -> NextSchedule {
    let ease = if card.ease >= MIN_EASE {
        card.ease
    } else {
        DEFAULT_EASE
    };
    let previous = card.interval_days.max(AGAIN_MINUTES / (24.0 * 60.0));
    let (interval_days, ease, repetitions) = if card.repetitions == 0 {
        match rating {
            Rating::Again => (AGAIN_MINUTES / (24.0 * 60.0), ease, 0),
            Rating::Hard | Rating::Good => (1.0, ease, 1),
            Rating::Easy => (3.0, ease, 1),
        }
    } else {
        match rating {
            Rating::Again => (
                AGAIN_MINUTES / (24.0 * 60.0),
                (ease - 0.2).max(MIN_EASE),
                0,
            ),
            Rating::Hard => (
                previous * 1.2,
                (ease - 0.15).max(MIN_EASE),
                card.repetitions + 1,
            ),
            Rating::Good => (previous * ease, ease, card.repetitions + 1),
            Rating::Easy => (
                previous * ease * 1.3,
                (ease + 0.15).min(MAX_EASE),
                card.repetitions + 1,
            ),
        }
    };
    let interval_days = interval_days.clamp(AGAIN_MINUTES / (24.0 * 60.0), MAX_INTERVAL_DAYS);
    let seconds = (interval_days * 24.0 * 60.0 * 60.0).round() as i64;
    NextSchedule {
        interval_days,
        ease,
        repetitions,
        due_at: now + seconds.max(60),
    }
}

/// 四档预览，界面按钮直接展示。
pub fn preview(card: &NoteCard, now: i64) -> [(Rating, NextSchedule); 4] {
    [
        (Rating::Again, next(card, Rating::Again, now)),
        (Rating::Hard, next(card, Rating::Hard, now)),
        (Rating::Good, next(card, Rating::Good, now)),
        (Rating::Easy, next(card, Rating::Easy, now)),
    ]
}

/// 套用评分，返回（旧间隔, 新间隔），供复习历史记录。
pub fn apply(card: &mut NoteCard, rating: Rating, now: i64) -> (f32, f32) {
    let previous = card.interval_days;
    let schedule = next(card, rating, now);
    card.interval_days = schedule.interval_days;
    card.ease = schedule.ease;
    card.repetitions = schedule.repetitions;
    card.due_at = schedule.due_at;
    card.last_reviewed_at = Some(now);
    card.updated_at = now;
    if rating == Rating::Again {
        card.lapses = card.lapses.saturating_add(1);
    }
    (previous, schedule.interval_days)
}

/// 可读的间隔文案：分钟 / 天 / 个月 / 年。
pub fn format_interval(days: f32) -> String {
    let total_minutes = minutes(days);
    if total_minutes < 60.0 {
        return format!("{} 分钟", (total_minutes.round() as i64).max(1));
    }
    if total_minutes < 24.0 * 60.0 {
        return format!("{} 小时", (total_minutes / 60.0).round() as i64);
    }
    if days < 30.0 {
        return format!("{} 天", days.round() as i64);
    }
    if days < 365.0 {
        return format!("{} 个月", (days / 30.0).round() as i64);
    }
    format!("{} 年", (days / 365.0).round() as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn card(repetitions: u32, interval_days: f32, ease: f32) -> NoteCard {
        NoteCard {
            id: 1,
            repetitions,
            interval_days,
            ease,
            ..NoteCard::default()
        }
    }

    #[test]
    fn new_card_intervals_follow_rating() {
        let now = 1_000_000;
        let again = next(&card(0, 0.0, 2.5), Rating::Again, now);
        assert!((again.interval_days * 1440.0 - 10.0).abs() < 0.01);
        assert_eq!(again.due_at, now + 600);
        assert_eq!(next(&card(0, 0.0, 2.5), Rating::Hard, now).interval_days, 1.0);
        assert_eq!(next(&card(0, 0.0, 2.5), Rating::Good, now).interval_days, 1.0);
        assert_eq!(next(&card(0, 0.0, 2.5), Rating::Easy, now).interval_days, 3.0);
    }

    #[test]
    fn old_card_multiplies_interval_and_bounds_ease() {
        let now = 0;
        let good = next(&card(2, 10.0, 2.5), Rating::Good, now);
        assert!((good.interval_days - 25.0).abs() < 0.01);
        let easy = next(&card(2, 10.0, 3.0), Rating::Easy, now);
        assert!((easy.ease - 3.0).abs() < 0.01, "ease 不应超过上限");
        let again = next(&card(4, 10.0, 1.3), Rating::Again, now);
        assert!((again.ease - MIN_EASE).abs() < 0.01);
        assert_eq!(again.repetitions, 0);
        let capped = next(&card(9, 300.0, 3.0), Rating::Easy, now);
        assert_eq!(capped.interval_days, MAX_INTERVAL_DAYS);
    }

    #[test]
    fn preview_matches_applied_schedule() {
        let now = 5000;
        let mut card = card(3, 4.0, 2.2);
        for rating in Rating::ALL {
            let expected = next(&card, rating, now);
            let (_, interval) = apply(&mut card, rating, now);
            assert!((interval - expected.interval_days).abs() < 0.001);
            assert_eq!(card.due_at, expected.due_at);
        }
    }

    #[test]
    fn formats_intervals_for_display() {
        assert_eq!(format_interval(10.0 / 1440.0), "10 分钟");
        assert_eq!(format_interval(1.0), "1 天");
        assert_eq!(format_interval(45.0), "2 个月");
        assert_eq!(format_interval(400.0), "1 年");
    }
}
