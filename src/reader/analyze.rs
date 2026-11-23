//! 整屏文本分析（M46）：把可见区间拼给 AI，解析条目并回标到章节字节区间。

use std::ops::Range;

use serde::{Deserialize, Serialize};

use crate::config::ReaderAiSettings;
use crate::reader::{BookDocument, Chapter};

/// 单次分析最多发送的字符数。
pub const MAX_ANALYSIS_CHARS: usize = 2500;
/// 整章分析的总字符上限（超出部分截断，避免异常超长章节无限调用）。
pub const MAX_CHAPTER_CHARS: usize = 27000;
/// 分段分析时每段的最大字符数；按段落边界切分。
pub const MAX_CHUNK_CHARS: usize = 4500;

/// 分析条目的类型。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AnnotationKind {
    Phrase,
    #[default]
    Expression,
    Grammar,
    /// 常用句式/句型。
    Pattern,
    /// 专业术语。
    Term,
    /// 专有名词。
    ProperNoun,
}

/// 持久化用的稳定键；与 `from_key` 成对，未知键回退到「常用表达」。
impl Serialize for AnnotationKind {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.key())
    }
}

impl<'de> Deserialize<'de> for AnnotationKind {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let key = String::deserialize(deserializer)?;
        Ok(Self::from_key(&key))
    }
}

impl AnnotationKind {
    pub const ALL: [Self; 6] = [
        Self::Phrase,
        Self::Expression,
        Self::Pattern,
        Self::Term,
        Self::ProperNoun,
        Self::Grammar,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Phrase => "词组",
            Self::Expression => "常用表达",
            Self::Grammar => "语法",
            Self::Pattern => "句式",
            Self::Term => "术语",
            Self::ProperNoun => "专有名词",
        }
    }

    pub fn from_key(key: &str) -> Self {
        match key.trim().to_ascii_lowercase().as_str() {
            "phrase" | "word" | "vocabulary" => Self::Phrase,
            "grammar" | "syntax" => Self::Grammar,
            "pattern" | "sentence_pattern" | "sentence-pattern" | "structure" => Self::Pattern,
            "term" | "terminology" | "technical" => Self::Term,
            "proper_noun" | "proper-noun" | "propernoun" | "name" => Self::ProperNoun,
            _ => Self::Expression,
        }
    }

    /// 持久化键（与 `from_key` 对称）。
    pub fn key(self) -> &'static str {
        match self {
            Self::Phrase => "phrase",
            Self::Expression => "expression",
            Self::Grammar => "grammar",
            Self::Pattern => "pattern",
            Self::Term => "term",
            Self::ProperNoun => "proper_noun",
        }
    }
}

/// 一条分析结果及其在正文中的位置。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Annotation {
    pub chapter: usize,
    pub start: usize,
    pub end: usize,
    pub kind: AnnotationKind,
    pub note: String,
    pub detail: String,
}

/// AI 返回的条目（尚未定位）。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AnalysisItem {
    pub text: String,
    pub kind: AnnotationKind,
    pub note: String,
    pub detail: String,
}

/// 一次分析的完整结果。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Analysis {
    pub summary: String,
    pub items: Vec<AnalysisItem>,
    pub annotations: Vec<Annotation>,
    pub stats: AnalysisStats,
}

/// 本次分析的消耗统计（tokens 为按字符估算，非服务端精确值）。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AnalysisStats {
    /// 实际请求的段数。
    pub chunks: usize,
    /// 总耗时（秒）。
    pub elapsed_secs: f64,
    /// 估算消耗：发送 + 返回字符数 / 4。
    pub tokens: usize,
    /// 发送的正文总字符数。
    pub sent_chars: usize,
    /// 返回的回复总字符数。
    pub received_chars: usize,
}

/// 分析阶段，供界面展示进度。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AnalysisStage {
    /// 正在请求某一段。
    #[default]
    Requesting,
    /// 全部完成。
    Done,
    /// 全部分段都失败。
    Failed,
}

/// 分段分析的实时进度。
#[derive(Clone, Debug, Default)]
pub struct AnalysisProgress {
    pub chapter: usize,
    /// 当前段号（从 1 开始）；0 表示尚未开始。
    pub chunk: usize,
    /// 总段数。
    pub chunks: usize,
    pub sent_chars: usize,
    pub received_chars: usize,
    pub elapsed_secs: f64,
    /// 已收集的条目数。
    pub items: usize,
    pub stage: AnalysisStage,
}

/// 内置提示词：只输出 JSON，便于解析；解释用英文，兼顾英语学习与知识拓展。
pub const DEFAULT_PROMPT: &str =
    "你是英语学习与知识拓展助手。分析下面这段正文，提取值得学习的语言点，类型包括：\
     phrase（词组）、expression（常用表达）、grammar（语法点）、pattern（常用句式/句型）、\
     term（专业术语）、proper_noun（专有名词）。\n\
     只输出 JSON，不要输出任何解释或 Markdown 围栏，格式：\n\
     {\"summary\": \"one-sentence English summary\", \"items\": [{\"text\": \"exact span from the text\", \
     \"kind\": \"phrase|expression|grammar|pattern|term|proper_noun\", \
     \"note\": \"concise English explanation\", \
     \"detail\": \"English explanation of meaning and usage, plus grammar/register notes and one example when helpful\"}]}\n\
     要求：text 必须逐字出现在正文中，不要改写或补标点；note 与 detail 一律用英文，从英语学习、\
     知识理解与拓展的角度写；最多 16 条；没有可分析内容时返回空 items。";

/// 实际使用的提示词：设置里的自定义内容优先，留空回退内置默认值。
pub fn system_prompt(settings: &ReaderAiSettings) -> &str {
    settings.prompts.analyze().unwrap_or(DEFAULT_PROMPT)
}

/// 内置提示词：围绕单个条目继续追问（英文回答）。
pub const DEFAULT_CHAT_PROMPT: &str =
    "你是英语学习助手。围绕用户正在阅读的语言点回答问题：解释含义、用法、搭配、语法、语域、\
     语源或背景知识，必要时给出例句。一律用英文回答，只回答用户的问题，不要复述无关内容。";

/// 条目追问提示词：设置里的自定义内容优先，留空回退内置默认值。
pub fn chat_system(settings: &ReaderAiSettings) -> &str {
    settings.prompts.chat().unwrap_or(DEFAULT_CHAT_PROMPT)
}

/// 整章正文；超长章节截断到 `MAX_CHAPTER_CHARS`，并落在字符边界上。
pub fn chapter_text(chapter: &Chapter) -> String {
    if chapter.text.chars().count() <= MAX_CHAPTER_CHARS {
        return chapter.text.clone();
    }
    let end = chapter
        .text
        .char_indices()
        .nth(MAX_CHAPTER_CHARS)
        .map(|(offset, _)| offset)
        .unwrap_or(chapter.text.len());
    chapter.text[..end].to_string()
}

/// 把正文按段落边界切成不超过 `max_chars` 的片段，返回（字节区间，片段）。
/// 区间始终落在字符边界上，可直接用于 `locate`。
pub fn split_chunks(text: &str, max_chars: usize) -> Vec<(Range<usize>, String)> {
    let limit = max_chars.max(200);
    let mut chunks = Vec::new();
    let mut start = 0usize;
    while start < text.len() {
        let mut end = text[start..]
            .char_indices()
            .nth(limit)
            .map(|(offset, _)| start + offset)
            .unwrap_or(text.len());
        if end < text.len() {
            if let Some(cut) = best_cut(&text[start..end]) {
                end = start + cut;
            }
        }
        if end <= start {
            break;
        }
        chunks.push((start..end, text[start..end].to_string()));
        start = end;
    }
    chunks
}

/// 在片段后半段寻找合适的断点：优先空行，其次换行，最后句末空格。
fn best_cut(slice: &str) -> Option<usize> {
    let min = slice.len() / 2;
    let pick = |index: Option<usize>, length: usize| {
        index
            .map(|index| index + length)
            .filter(|cut| *cut >= min)
    };
    pick(slice.rfind("\r\n\r\n"), 4)
        .or_else(|| pick(slice.rfind("\n\n"), 2))
        .or_else(|| pick(slice.rfind('\n'), 1))
        .or_else(|| pick(slice.rfind(". "), 2))
}

/// 条目文本的去重键：忽略大小写、空白与标点。
fn item_key(text: &str) -> String {
    text.chars()
        .filter(|character| character.is_alphanumeric())
        .flat_map(|character| character.to_lowercase())
        .collect()
}

/// 分析整章：按段落分段请求，合并条目并按原文重新定位。
/// 每段独立解析，重复条目只保留一次；部分段落失败时保留已成功的结果。
pub fn analyze_chapter(
    settings: &ReaderAiSettings,
    document: &BookDocument,
    chapter: usize,
) -> Result<Analysis, String> {
    analyze_chapter_with_progress(settings, document, chapter, |_| {})
}

/// 同 `analyze_chapter`，并在每段开始/结束时回调实时进度。
pub fn analyze_chapter_with_progress(
    settings: &ReaderAiSettings,
    document: &BookDocument,
    chapter: usize,
    mut report: impl FnMut(AnalysisProgress),
) -> Result<Analysis, String> {
    let Some(target) = document.chapters.get(chapter) else {
        return Err("章节不存在".to_string());
    };
    let text = chapter_text(target);
    if text.trim().is_empty() {
        return Err("本章没有可分析的正文".to_string());
    }
    let system = system_prompt(settings);
    let chunks = split_chunks(&text, MAX_CHUNK_CHARS);
    crate::ai::log(format!(
        "整章分析 第 {} 章 共 {} 段（每段 ≤ {} 字）",
        chapter + 1,
        chunks.len(),
        MAX_CHUNK_CHARS
    ));
    let started = std::time::Instant::now();
    let total_chunks = chunks.len();
    let mut summaries: Vec<String> = Vec::new();
    let mut items: Vec<AnalysisItem> = Vec::new();
    let mut annotations: Vec<Annotation> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut errors: Vec<String> = Vec::new();
    let mut sent_chars = 0usize;
    let mut received_chars = 0usize;
    let progress = |chunk: usize,
                    sent: usize,
                    received: usize,
                    items: usize,
                    stage: AnalysisStage| AnalysisProgress {
        chapter,
        chunk,
        chunks: total_chunks,
        sent_chars: sent,
        received_chars: received,
        elapsed_secs: started.elapsed().as_secs_f64(),
        items,
        stage,
    };
    report(progress(0, sent_chars, received_chars, 0, AnalysisStage::Requesting));
    for (index, (range, chunk)) in chunks.into_iter().enumerate() {
        sent_chars += chunk.chars().count();
        report(progress(
            index + 1,
            sent_chars,
            received_chars,
            items.len(),
            AnalysisStage::Requesting,
        ));
        match crate::ai::ask_with_limit(settings, system, &chunk, chunk.chars().count().max(1)) {
            Ok(reply) => match parse_reply(&reply) {
                Ok((summary, chunk_items)) => {
                    received_chars += reply.chars().count();
                    if !summary.is_empty() {
                        summaries.push(summary);
                    }
                    let fresh: Vec<AnalysisItem> = chunk_items
                        .into_iter()
                        .filter(|item| seen.insert(item_key(&item.text)))
                        .collect();
                    if !fresh.is_empty() {
                        annotations.extend(locate(
                            document,
                            &[(chapter, range)],
                            &fresh,
                        ));
                        items.extend(fresh);
                    }
                }
                Err(error) => {
                    received_chars += reply.chars().count();
                    crate::ai::log(format!("第 {} 段解析失败：{error}", index + 1));
                    errors.push(error);
                }
            },
            Err(error) => {
                crate::ai::log(format!("第 {} 段请求失败：{error}", index + 1));
                errors.push(error);
            }
        }
        report(progress(
            index + 1,
            sent_chars,
            received_chars,
            items.len(),
            AnalysisStage::Requesting,
        ));
    }
    let elapsed_secs = started.elapsed().as_secs_f64();
    let stats = AnalysisStats {
        chunks: total_chunks,
        elapsed_secs,
        tokens: (sent_chars + received_chars + 3) / 4,
        sent_chars,
        received_chars,
    };
    if items.is_empty() {
        report(progress(
            total_chunks,
            sent_chars,
            received_chars,
            0,
            AnalysisStage::Failed,
        ));
        return Err(if errors.is_empty() {
            "AI 未返回可用的分析条目".to_string()
        } else {
            errors.join("；")
        });
    }
    crate::ai::log(format!(
        "整章分析完成 第 {} 章 条目={} 定位={} 用时={:.1}s 发送={} 字 返回={} 字 估算 tokens≈{}",
        chapter + 1,
        items.len(),
        annotations.len(),
        elapsed_secs,
        sent_chars,
        received_chars,
        stats.tokens
    ));
    report(progress(
        total_chunks,
        sent_chars,
        received_chars,
        items.len(),
        AnalysisStage::Done,
    ));
    Ok(Analysis {
        summary: summaries.join(" "),
        items,
        annotations,
        stats,
    })
}

/// 把可见区间拼成一段文本（章节间空行），并限制总字符数。
pub fn visible_text(document: &BookDocument, ranges: &[(usize, Range<usize>)]) -> String {
    let mut output = String::new();
    let mut remaining = MAX_ANALYSIS_CHARS;
    for (chapter, range) in ranges {
        let Some(chapter) = document.chapters.get(*chapter) else {
            continue;
        };
        let range = clamp_range(&chapter.text, range);
        if range.is_empty() {
            continue;
        }
        if remaining == 0 {
            break;
        }
        if !output.is_empty() {
            output.push_str("\n\n");
        }
        let slice: String = chapter.text[range].chars().take(remaining).collect();
        remaining = remaining.saturating_sub(slice.chars().count());
        output.push_str(slice.trim_end());
    }
    output
}

/// 容忍 ```json 围栏、前后说明与缺字段；返回 summary 与可用条目。
pub fn parse_reply(reply: &str) -> Result<(String, Vec<AnalysisItem>), String> {
    let json = crate::ai::json_object(reply)
        .ok_or_else(|| "AI 返回中没有 JSON 对象".to_string())?;
    let raw: RawAnalysis = serde_json::from_str(json)
        .map_err(|error| format!("分析结果无法解析：{error}"))?;
    let items: Vec<AnalysisItem> = raw
        .items
        .into_iter()
        .filter_map(|item| {
            let text = item.text.trim().to_string();
            if text.is_empty() {
                return None;
            }
            Some(AnalysisItem {
                text,
                kind: AnnotationKind::from_key(&item.kind),
                note: item.note.trim().to_string(),
                detail: item.detail.trim().to_string(),
            })
        })
        .collect();
    if items.is_empty() {
        return Err("AI 未返回可用的分析条目".to_string());
    }
    Ok((raw.summary.trim().to_string(), items))
}

/// 按条目文本在可见区间内定位（最长优先、跳过重叠），生成回标区间。
pub fn locate(
    document: &BookDocument,
    ranges: &[(usize, Range<usize>)],
    items: &[AnalysisItem],
) -> Vec<Annotation> {
    let mut ordered: Vec<&AnalysisItem> = items.iter().collect();
    ordered.sort_by_key(|item| std::cmp::Reverse(item.text.chars().count()));
    let mut annotations: Vec<Annotation> = Vec::new();
    for item in ordered {
        for (chapter, range) in ranges {
            let Some(chapter_text) = document.chapters.get(*chapter) else {
                continue;
            };
            let range = clamp_range(&chapter_text.text, range);
            if range.is_empty() {
                continue;
            }
            let haystack = &chapter_text.text[range.clone()];
            // 先精确匹配；模型偶尔改变大小写，再用 ASCII 小写重试。
            let found = haystack
                .find(item.text.as_str())
                .map(|offset| (offset, item.text.len()))
                .or_else(|| {
                    let needle = item.text.to_ascii_lowercase();
                    haystack
                        .to_ascii_lowercase()
                        .find(&needle)
                        .map(|offset| (offset, needle.len()))
                });
            let Some((offset, length)) = found else {
                continue;
            };
            let span_start = range.start + offset;
            let span_end = span_start + length;
            if annotations.iter().any(|annotation| {
                annotation.chapter == *chapter
                    && annotation.start < span_end
                    && span_start < annotation.end
            }) {
                continue;
            }
            annotations.push(Annotation {
                chapter: *chapter,
                start: span_start,
                end: span_end,
                kind: item.kind,
                note: item.note.clone(),
                detail: item.detail.clone(),
            });
            break;
        }
    }
    annotations.sort_by_key(|annotation| (annotation.chapter, annotation.start));
    annotations
}

fn clamp_range(text: &str, range: &Range<usize>) -> Range<usize> {
    let mut start = range.start.min(text.len());
    while start < text.len() && !text.is_char_boundary(start) {
        start += 1;
    }
    let mut end = range.end.min(text.len());
    while end > start && !text.is_char_boundary(end) {
        end -= 1;
    }
    start..end
}

#[derive(Deserialize, Default)]
struct RawAnalysis {    #[serde(default)]
    summary: String,
    #[serde(default)]
    items: Vec<RawItem>,
}

#[derive(Deserialize, Default)]
struct RawItem {
    #[serde(default)]
    text: String,
    #[serde(default)]
    kind: String,
    #[serde(default)]
    note: String,
    #[serde(default)]
    detail: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reader::Chapter;

    fn document(text: &str) -> BookDocument {
        BookDocument {
            title: None,
            author: None,
            is_pdf: false,
            chapters: vec![Chapter {
                title: "One".to_string(),
                text: text.to_string(),
                ..Chapter::default()
            }],
        }
    }

    #[test]
    fn parses_fenced_json_and_skips_bad_items() {
        let reply = "```json\n{\"summary\":\"概要\",\"items\":[\
            {\"text\":\"take off\",\"kind\":\"phrase\",\"note\":\"起飞\"},\
            {\"text\":\"  \",\"kind\":\"grammar\"},\
            {\"text\":\"would have\",\"kind\":\"odd\",\"detail\":\"虚拟语气\"}]}\n```";
        let (summary, items) = parse_reply(reply).expect("parse");
        assert_eq!(summary, "概要");
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].kind, AnnotationKind::Phrase);
        assert_eq!(items[1].kind, AnnotationKind::Expression);
    }

    #[test]
    fn annotation_kinds_cover_language_points() {
        assert_eq!(AnnotationKind::from_key("pattern"), AnnotationKind::Pattern);
        assert_eq!(AnnotationKind::from_key("Term"), AnnotationKind::Term);
        assert_eq!(
            AnnotationKind::from_key("proper_noun"),
            AnnotationKind::ProperNoun
        );
        assert_eq!(AnnotationKind::ALL.len(), 6);
        assert_eq!(AnnotationKind::Pattern.label(), "句式");
        assert_eq!(AnnotationKind::ProperNoun.label(), "专有名词");
    }

    #[test]
    fn reports_reply_without_items() {
        let error = parse_reply("{\"summary\":\"x\",\"items\":[]}").expect_err("empty");
        assert!(error.contains("未返回"));
        assert!(parse_reply("no json").is_err());
    }

    #[test]
    fn locates_longest_first_and_skips_overlap() {
        let book = document("Take off the coat, then take off the shoes. take");
        let ranges = vec![(0, 0..book.chapters[0].text.len())];
        let items = vec![
            AnalysisItem {
                text: "take".to_string(),
                kind: AnnotationKind::Phrase,
                note: String::new(),
                detail: String::new(),
            },
            AnalysisItem {
                text: "Take off".to_string(),
                kind: AnnotationKind::Phrase,
                note: String::new(),
                detail: String::new(),
            },
            AnalysisItem {
                text: "take off".to_string(),
                kind: AnnotationKind::Phrase,
                note: String::new(),
                detail: String::new(),
            },
        ];
        let annotations = locate(&book, &ranges, &items);
        assert_eq!(annotations.len(), 2);
        assert_eq!(&book.chapters[0].text[annotations[0].start..annotations[0].end], "Take off");
        assert_eq!(&book.chapters[0].text[annotations[1].start..annotations[1].end], "take off");
    }

    #[test]
    fn visible_text_respects_ranges_and_budget() {
        let text = "字".repeat(3000);
        let book = document(&text);
        let ranges = vec![(0, 0..text.len())];
        let visible = visible_text(&book, &ranges);
        assert_eq!(visible.chars().count(), MAX_ANALYSIS_CHARS);
        let ranges = vec![(0, 0..30)];
        assert_eq!(visible_text(&book, &ranges), "字".repeat(10));
        // 非字符边界会被向内收拢，不会 panic。
        let ranges = vec![(0, 10..20)];
        assert_eq!(visible_text(&book, &ranges), "字".repeat(2));
    }

    #[test]
    fn splits_at_paragraph_boundaries() {
        let paragraph = "This is a fairly long paragraph used for chunking tests. ".repeat(10);
        let text = (0..20)
            .map(|_| paragraph.clone())
            .collect::<Vec<_>>()
            .join("\n\n");
        let chunks = split_chunks(&text, 800);
        assert!(chunks.len() > 1);
        assert_eq!(chunks[0].0.start, 0);
        assert_eq!(chunks.last().unwrap().0.end, text.len());
        for pair in chunks.windows(2) {
            assert_eq!(pair[0].0.end, pair[1].0.start);
        }
        for (range, chunk) in &chunks {
            assert!(chunk.chars().count() <= 800);
            assert_eq!(&text[range.clone()], chunk);
        }
        // 非最后一段都切在段落边界上。
        for (_, chunk) in chunks.iter().take(chunks.len() - 1) {
            assert!(chunk.ends_with("\n\n"));
        }
    }

    #[test]
    fn item_keys_ignore_case_and_punctuation() {
        assert_eq!(item_key("Take off!"), item_key("take  off"));
        assert_ne!(item_key("take off"), item_key("take out"));
    }
}
