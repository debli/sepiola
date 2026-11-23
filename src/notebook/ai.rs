//! 笔记本 AI 能力（M47）：出题与造句批改，复用 `crate::ai` 的统一后端。

use serde::{Deserialize, Serialize};

use crate::config::ReaderAiSettings;
use crate::notebook::NoteCard;

/// 习题类型。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ExerciseKind {
    Cloze,
    Choice,
    Translation,
    Correction,
}

impl ExerciseKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Cloze => "填空",
            Self::Choice => "选择",
            Self::Translation => "翻译",
            Self::Correction => "改错",
        }
    }

    fn from_key(key: &str) -> Option<Self> {
        Some(match key.trim().to_ascii_lowercase().as_str() {
            "cloze" | "fill" | "blank" => Self::Cloze,
            "choice" | "multiple_choice" | "mcq" => Self::Choice,
            "translation" | "translate" => Self::Translation,
            "correction" | "correct" => Self::Correction,
            _ => return None,
        })
    }
}

/// 一道 AI 生成的习题。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Exercise {
    #[serde(skip)]
    pub kind: Option<ExerciseKind>,
    pub question: String,
    pub options: Vec<String>,
    pub answer: String,
    pub explanation: String,
}

/// 造句批改中的一条问题。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SentenceIssue {
    pub span: String,
    pub kind: String,
    pub explanation: String,
}

/// 造句批改结果。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SentenceCheck {
    pub ok: bool,
    pub corrected: String,
    pub issues: Vec<SentenceIssue>,
    pub comment: String,
}

/// 内置提示词：出题（需保留 JSON 格式要求）。
pub const DEFAULT_EXERCISE_PROMPT: &str =
    "你是语言学习出题助手。根据用户给出的记忆卡片生成 3 到 5 道习题，覆盖填空、选择、翻译或改错。\
     只输出 JSON，不要输出任何解释或 Markdown 围栏，格式：\
     {\"exercises\":[{\"kind\":\"cloze|choice|translation|correction\",\"question\":\"题干\",\
     \"options\":[\"选项\"],\"answer\":\"参考答案\",\"explanation\":\"讲解\"}]}。\
     选择题必须提供 2 到 4 个选项；其他类型 options 可为空数组。";

/// 内置提示词：造句批改（需保留 JSON 格式要求）。
pub const DEFAULT_SENTENCE_PROMPT: &str =
    "你是语言教师。检查用户用指定词语造的句子，指出语法、搭配、用词与自然度问题。\
     只输出 JSON，不要输出任何解释或 Markdown 围栏，格式：\
     {\"ok\":true,\"corrected\":\"修改后的句子\",\"issues\":[{\"span\":\"原文片段\",\
     \"kind\":\"grammar|usage|word-choice|naturalness\",\"explanation\":\"说明\"}],\"comment\":\"总评\"}。\
     句子正确时 ok 为 true 且 issues 为空；correction 原样返回正确句子。";

/// 出题提示词：设置里的自定义内容优先，留空回退内置默认值。
pub fn exercise_system(settings: &ReaderAiSettings) -> &str {
    settings.prompts.exercise().unwrap_or(DEFAULT_EXERCISE_PROMPT)
}

pub fn exercise_user(card: &NoteCard) -> String {
    let tags = if card.tags.is_empty() {
        String::new()
    } else {
        format!("\n标签：{}", card.tags.join("、"))
    };
    format!(
        "卡片正面：{}\n卡片背面：{}{tags}",
        card.front.trim(),
        card.back.trim()
    )
}

/// 造句批改提示词：设置里的自定义内容优先，留空回退内置默认值。
pub fn sentence_system(settings: &ReaderAiSettings) -> &str {
    settings.prompts.sentence().unwrap_or(DEFAULT_SENTENCE_PROMPT)
}

pub fn sentence_user(card: &NoteCard, sentence: &str) -> String {
    format!(
        "目标词/短语：{}\n释义：{}\n用户造句：{}",
        card.front.trim(),
        card.back.trim(),
        sentence.trim()
    )
}

/// AI 生成的记忆卡片草稿。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CardDraft {
    pub front: String,
    pub back: String,
    pub tags: Vec<String>,
}

/// 内置提示词：把分析条目转成记忆卡片（英文解释）。
pub const DEFAULT_CARD_PROMPT: &str =
    "你是英语学习卡片助手。根据候选语言点生成记忆卡片：front 是需要记忆的英文片段\
     （词组、句式、术语或专有名词），back 用英文解释其含义、用法与搭配，并给一个例句，\
     必要时补充背景知识或术语定义。\n\
     同时给出已有卡片列表；若某个候选与已有卡片的语义重复，不要生成卡片，\
     放入 skipped 并说明原因。\n\
     只输出 JSON，不要输出任何解释或 Markdown 围栏，格式：\n\
     {\"cards\":[{\"front\":\"...\",\"back\":\"...\",\"tags\":[\"phrase\"]}],\
     \"skipped\":[{\"front\":\"...\",\"reason\":\"duplicate of an existing card\"}]}\n\
     tags 使用简短分类：phrase、expression、pattern、term、proper-noun。";

/// 生成卡片提示词：设置里的自定义内容优先，留空回退内置默认值。
pub fn card_system(settings: &ReaderAiSettings) -> &str {
    settings.prompts.card().unwrap_or(DEFAULT_CARD_PROMPT)
}

/// 候选语言点 + 已有卡片正面，供去重与生成。
pub fn card_user(candidates: &[String], existing: &[String]) -> String {
    use std::fmt::Write as _;
    let mut output = String::from("候选语言点：\n");
    for (index, candidate) in candidates.iter().enumerate() {
        let _ = writeln!(output, "{}. {}", index + 1, candidate.trim());
    }
    output.push_str("\n已有卡片：\n");
    if existing.is_empty() {
        output.push_str("（无）\n");
    } else {
        for front in existing {
            let _ = writeln!(output, "- {}", front.trim());
        }
    }
    output
}

/// 解析卡片生成结果：返回（卡片，跳过的重复项说明）；过滤空正面与内部重复。
pub fn parse_cards(reply: &str) -> Result<(Vec<CardDraft>, Vec<String>), String> {
    let json = crate::ai::json_object(reply)
        .ok_or_else(|| "AI 返回中没有 JSON 对象".to_string())?;
    let raw: RawCards = serde_json::from_str(json).map_err(|error| format!("卡片无法解析：{error}"))?;
    let mut seen = std::collections::BTreeSet::new();
    let cards: Vec<CardDraft> = raw
        .cards
        .into_iter()
        .filter_map(|item| {
            let front = item.front.trim().to_string();
            let back = item.back.trim().to_string();
            if front.is_empty() || back.is_empty() {
                return None;
            }
            if !seen.insert(crate::notebook::normalize_front(&front)) {
                return None;
            }
            Some(CardDraft {
                front,
                back,
                tags: item
                    .tags
                    .into_iter()
                    .map(|tag| tag.trim().to_string())
                    .filter(|tag| !tag.is_empty())
                    .collect(),
            })
        })
        .collect();
    let skipped: Vec<String> = raw
        .skipped
        .into_iter()
        .filter_map(|item| {
            let front = item.front.trim().to_string();
            if front.is_empty() {
                return None;
            }
            let reason = item.reason.trim();
            Some(if reason.is_empty() {
                front
            } else {
                format!("{front}（{reason}）")
            })
        })
        .collect();
    if cards.is_empty() && skipped.is_empty() {
        return Err("AI 未返回可用卡片".to_string());
    }
    Ok((cards, skipped))
}

/// 解析出题结果；容忍围栏、缺字段与未知类型。
pub fn parse_exercises(reply: &str) -> Result<Vec<Exercise>, String> {
    let json = crate::ai::json_object(reply)
        .ok_or_else(|| "AI 返回中没有 JSON 对象".to_string())?;
    let raw: RawExercises =
        serde_json::from_str(json).map_err(|error| format!("习题无法解析：{error}"))?;
    let exercises: Vec<Exercise> = raw
        .exercises
        .into_iter()
        .filter_map(|item| {
            let question = item.question.trim().to_string();
            if question.is_empty() {
                return None;
            }
            let kind = ExerciseKind::from_key(&item.kind)?;
            let options = item
                .options
                .into_iter()
                .map(|option| option.trim().to_string())
                .filter(|option| !option.is_empty())
                .collect();
            Some(Exercise {
                kind: Some(kind),
                question,
                options,
                answer: item.answer.trim().to_string(),
                explanation: item.explanation.trim().to_string(),
            })
        })
        .collect();
    if exercises.is_empty() {
        return Err("AI 未返回可用的习题".to_string());
    }
    Ok(exercises)
}

/// 解析造句批改结果。
pub fn parse_sentence_check(reply: &str) -> Result<SentenceCheck, String> {
    let json = crate::ai::json_object(reply)
        .ok_or_else(|| "AI 返回中没有 JSON 对象".to_string())?;
    let raw: RawSentenceCheck =
        serde_json::from_str(json).map_err(|error| format!("批改结果无法解析：{error}"))?;
    let corrected = raw.corrected.trim().to_string();
    if corrected.is_empty() {
        return Err("AI 未返回修改后的句子".to_string());
    }
    Ok(SentenceCheck {
        ok: raw.ok,
        corrected,
        issues: raw
            .issues
            .into_iter()
            .filter_map(|issue| {
                let explanation = issue.explanation.trim().to_string();
                if explanation.is_empty() {
                    return None;
                }
                Some(SentenceIssue {
                    span: issue.span.trim().to_string(),
                    kind: issue.kind.trim().to_string(),
                    explanation,
                })
            })
            .collect(),
        comment: raw.comment.trim().to_string(),
    })
}

#[derive(Deserialize, Default)]
struct RawCards {
    #[serde(default)]
    cards: Vec<RawCard>,
    #[serde(default)]
    skipped: Vec<RawCardSkip>,
}

#[derive(Deserialize, Default)]
struct RawCard {
    #[serde(default)]
    front: String,
    #[serde(default)]
    back: String,
    #[serde(default)]
    tags: Vec<String>,
}

#[derive(Deserialize, Default)]
struct RawCardSkip {
    #[serde(default)]
    front: String,
    #[serde(default)]
    reason: String,
}

#[derive(Deserialize, Default)]
struct RawExercises {
    #[serde(default)]
    exercises: Vec<RawExercise>,
}

#[derive(Deserialize, Default)]
struct RawExercise {
    #[serde(default)]
    kind: String,
    #[serde(default)]
    question: String,
    #[serde(default)]
    options: Vec<String>,
    #[serde(default)]
    answer: String,
    #[serde(default)]
    explanation: String,
}

#[derive(Deserialize, Default)]
struct RawSentenceCheck {
    #[serde(default)]
    ok: bool,
    #[serde(default)]
    corrected: String,
    #[serde(default)]
    issues: Vec<RawIssue>,
    #[serde(default)]
    comment: String,
}

#[derive(Deserialize, Default)]
struct RawIssue {
    #[serde(default)]
    span: String,
    #[serde(default)]
    kind: String,
    #[serde(default)]
    explanation: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_exercises_and_skips_unknown_kinds() {
        let reply = "```json\n{\"exercises\":[\
            {\"kind\":\"choice\",\"question\":\"哪个是释义？\",\"options\":[\"A\",\"B\"],\"answer\":\"A\",\"explanation\":\"因为\"},\
            {\"kind\":\"weird\",\"question\":\"skip\"},\
            {\"kind\":\"cloze\",\"question\":\"I ___ home.\",\"answer\":\"went\"}]}\n```";
        let exercises = parse_exercises(reply).expect("parse");
        assert_eq!(exercises.len(), 2);
        assert_eq!(exercises[0].kind, Some(ExerciseKind::Choice));
        assert_eq!(exercises[0].options, ["A", "B"]);
        assert_eq!(exercises[1].kind, Some(ExerciseKind::Cloze));
        assert!(parse_exercises("{\"exercises\":[]}").is_err());
    }

    #[test]
    fn parses_cards_and_skips_empty_or_repeated_entries() {
        let reply = "```json\n{\"cards\":[\
            {\"front\":\"take off\",\"back\":\"to remove\",\"tags\":[\"phrase\",\"\"]},\
            {\"front\":\"TAKE OFF.\",\"back\":\"duplicate\",\"tags\":[]},\
            {\"front\":\"\",\"back\":\"empty\"}],\
            \"skipped\":[{\"front\":\"give up\",\"reason\":\"duplicate of an existing card\"},\
            {\"front\":\"\",\"reason\":\"x\"}]}\n```";
        let (cards, skipped) = parse_cards(reply).expect("parse");
        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0].front, "take off");
        assert_eq!(cards[0].tags, ["phrase"]);
        assert_eq!(skipped, ["give up（duplicate of an existing card）"]);
        assert!(parse_cards("{\"cards\":[],\"skipped\":[]}").is_err());
    }

    #[test]
    fn parses_sentence_check_with_issues() {
        let reply = "结果：{\"ok\":false,\"corrected\":\"He goes home.\",\
            \"issues\":[{\"span\":\"go\",\"kind\":\"grammar\",\"explanation\":\"第三人称单数\"},\
            {\"span\":\"\",\"kind\":\"x\",\"explanation\":\"\"}],\"comment\":\"基本正确\"}";
        let check = parse_sentence_check(reply).expect("parse");
        assert!(!check.ok);
        assert_eq!(check.corrected, "He goes home.");
        assert_eq!(check.issues.len(), 1);
        assert_eq!(check.comment, "基本正确");
        assert!(parse_sentence_check("{\"corrected\":\"\"}").is_err());
    }
}
