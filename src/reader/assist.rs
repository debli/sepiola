//! 阅读助手（M45/M46）：把选中文字送到配置的 AI 后端做翻译或解释。

use crate::ai;
use crate::config::ReaderAiSettings;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AssistKind {
    Translate,
    Explain,
}

/// 内置提示词：翻译。
pub const DEFAULT_TRANSLATE_PROMPT: &str =
    "你是翻译助手。把用户选中【选中内容】翻译成中文；若原文已经是中文，则翻译成英文。\
     用户消息里可能附带选中内容所在的段落，只用来判断语境与词义，不要翻译或复述它。\
     只输出译文，保留原有段落结构。";

/// 内置提示词：解释（英文输出，面向英语学习与知识拓展）。
pub const DEFAULT_EXPLAIN_PROMPT: &str =
    "你是英语学习与知识拓展助手。用户消息给出【选中内容】以及它所在的段落（【所在段落】）。\
     结合段落判断指代、词义与语境，但解释对象只限于【选中内容】，不要整段翻译或复述。\
     用英文回答：1) 它在当前语境中的含义；2) 关键词语、搭配与用法；3) 语法或句式要点；\
     4) 必要的背景知识或延伸，并给出一个例句。\
     若选中内容不是英文，先用英文给出对应表达再解释。";

impl AssistKind {
    pub fn title(self) -> &'static str {
        match self {
            Self::Translate => "翻译",
            Self::Explain => "AI 解释",
        }
    }

    pub fn default_prompt(self) -> &'static str {
        match self {
            Self::Translate => DEFAULT_TRANSLATE_PROMPT,
            Self::Explain => DEFAULT_EXPLAIN_PROMPT,
        }
    }
}

/// 实际使用的提示词：设置里的自定义内容优先，留空回退内置默认值。
pub fn prompt<'a>(settings: &'a ReaderAiSettings, kind: AssistKind) -> &'a str {
    let custom = match kind {
        AssistKind::Translate => settings.prompts.translate(),
        AssistKind::Explain => settings.prompts.explain(),
    };
    custom.unwrap_or_else(|| kind.default_prompt())
}

/// 按当前配置的后端请求；未配置、网络或接口错误均返回可读信息。
pub fn ask(settings: &ReaderAiSettings, kind: AssistKind, text: &str) -> Result<String, String> {
    ask_with_context(settings, kind, text, None)
}

/// 附带上下文（选中内容所在段落）的翻译 / 解释。
pub fn ask_with_context(
    settings: &ReaderAiSettings,
    kind: AssistKind,
    text: &str,
    context: Option<&str>,
) -> Result<String, String> {
    ai::ask(settings, prompt(settings, kind), &user_content(text, context))
}

/// 同 `ask_with_context`，额外返回耗时与 token 用量。
pub fn ask_with_usage(
    settings: &ReaderAiSettings,
    kind: AssistKind,
    text: &str,
    context: Option<&str>,
) -> Result<(String, ai::AiUsage), String> {
    ai::ask_with_usage(settings, prompt(settings, kind), &user_content(text, context))
}

/// 用户消息：选中的文字，加上它所在的段落（供模型判断语境）。
fn user_content(text: &str, context: Option<&str>) -> String {
    match context.map(str::trim).filter(|context| !context.is_empty()) {
        Some(context) => format!(
            "【选中内容】\n{}\n\n【所在段落，仅供理解语境】\n{context}",
            text.trim()
        ),
        None => text.to_string(),
    }
}

/// 选区所在的整段文字：向两侧扩展到段落边界（空行）或章节首尾。
/// 段落太长时以选区为中心截取 `max_chars` 个字符；段落与选区相同则返回 `None`。
pub fn paragraph_context(text: &str, start: usize, end: usize, max_chars: usize) -> Option<String> {
    let start = start.min(text.len());
    let end = end.min(text.len()).max(start);
    if !text.is_char_boundary(start) || !text.is_char_boundary(end) || max_chars == 0 {
        return None;
    }
    let selected = text[start..end].trim();
    if selected.is_empty() {
        return None;
    }
    // 段落边界：空行（两个换行，允许中间夹空白）或正文首尾。
    let paragraph_start = text[..start]
        .rfind("\n\n")
        .map(|index| index + 2)
        .unwrap_or(0);
    let paragraph_end = text[end..]
        .find("\n\n")
        .map(|index| end + index)
        .unwrap_or(text.len());
    let paragraph = &text[paragraph_start..paragraph_end];
    let leading = paragraph.len() - paragraph.trim_start().len();
    let body = paragraph.trim();
    if body.is_empty() || body == selected {
        return None;
    }
    if body.chars().count() <= max_chars {
        return Some(body.to_string());
    }
    // 选区在段落中的字符位置，截取时尽量让选区居中。
    let relative = start.saturating_sub(paragraph_start + leading).min(body.len());
    let head = body[..relative].chars().count();
    let skip = head.saturating_sub(max_chars / 2);
    Some(body.chars().skip(skip).take(max_chars).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requires_configuration() {
        let settings = ReaderAiSettings::default();
        assert!(ask(&settings, AssistKind::Translate, "hello").is_err());
        assert!(!settings.enabled());
        let settings = ReaderAiSettings {
            base_url: "https://example.com/v1".to_string(),
            model: "m".to_string(),
            ..Default::default()
        };
        assert!(settings.enabled());
    }

    #[test]
    fn rejects_empty_selection_before_network() {
        let settings = ReaderAiSettings {
            base_url: "https://example.com/v1".to_string(),
            api_key: String::new(),
            model: "m".to_string(),
            ..Default::default()
        };
        let error = ask(&settings, AssistKind::Explain, "   ").expect_err("empty");
        assert!(error.contains("没有可处理"), "{error}");
    }

    #[test]
    fn prompts_are_distinct() {
        assert_ne!(
            AssistKind::Translate.default_prompt(),
            AssistKind::Explain.default_prompt()
        );
        assert_eq!(AssistKind::Translate.title(), "翻译");
    }

    #[test]
    fn custom_prompt_overrides_default() {
        let mut settings = ReaderAiSettings::default();
        assert_eq!(
            prompt(&settings, AssistKind::Explain),
            DEFAULT_EXPLAIN_PROMPT
        );
        settings.prompts.explain = "  用英文解释。 ".to_string();
        assert_eq!(prompt(&settings, AssistKind::Explain), "用英文解释。");
        // 其他功能不受影响。
        assert_eq!(
            prompt(&settings, AssistKind::Translate),
            DEFAULT_TRANSLATE_PROMPT
        );
    }

    #[test]
    fn paragraph_context_expands_to_blank_lines() {
        let text = "First paragraph.\n\nHe said it was over, as it were, for good.\n\nThird one.";
        let start = text.find("as it were").expect("phrase");
        let context =
            paragraph_context(text, start, start + "as it were".len(), 600).expect("context");
        assert_eq!(context, "He said it was over, as it were, for good.");
    }

    #[test]
    fn paragraph_context_skips_standalone_paragraph() {
        // 选区就是整段：没有额外上下文可带。
        let text = "Only one paragraph here.";
        assert_eq!(paragraph_context(text, 0, text.len(), 600), None);
        // 选区只是段中的几个词：整段都要带上。
        assert_eq!(
            paragraph_context(text, 5, 8, 600).as_deref(),
            Some("Only one paragraph here.")
        );
    }

    #[test]
    fn paragraph_context_keeps_selection_when_truncating() {
        let long = format!("{}TARGET{}", "a".repeat(500), "b".repeat(500));
        let start = 500;
        let context = paragraph_context(&long, start, start + 6, 100).expect("context");
        assert_eq!(context.chars().count(), 100);
        assert!(context.contains("TARGET"), "{context}");
    }

    #[test]
    fn user_content_includes_context() {
        assert_eq!(user_content("word", None), "word");
        let with = user_content("word", Some("  the whole sentence  "));
        assert!(with.starts_with("【选中内容】\nword"), "{with}");
        assert!(with.contains("the whole sentence"), "{with}");
        assert!(!with.contains("  the whole sentence  "), "{with}");
    }
}
