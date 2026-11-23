//! 设置页：编辑工具条依赖的外部服务配置（AI 翻译/解释）；词典设置在阅读设置弹窗中。
//!
//! 修改写入用户设置（`~/.config/sepiola/user-settings.json`），留空的字段回退到
//! `config.toml` 中的 `[reader.ai]` / `[reader.dictionary]`。

use eframe::egui;
use sepiola::config::ReaderDictionarySettings;

use crate::reader_selection::{has_scheme, open_target};
use crate::state::SepiolaApp;

/// 千分位数字，方便读 tokens。
fn thousands(value: u64) -> String {
    let digits = value.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, ch) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

/// 常用词典的跳转模板。
const DICTIONARY_PRESETS: [(&str, &str); 3] = [
    ("欧路词典", "eudic://dict/{word}"),
    (
        "有道词典",
        "https://dict.youdao.com/result?word={word}&lang=en",
    ),
    (
        "剑桥词典",
        "https://dictionary.cambridge.org/dictionary/english/{word}",
    ),
];

impl SepiolaApp {
    pub fn app_settings_ui(&mut self, ui: &mut egui::Ui) {
        egui::ScrollArea::vertical().show(ui, |ui| {
            ui.heading("设置");
            ui.label(
                egui::RichText::new("修改会立即保存到用户设置；留空的项沿用 config.toml 中的配置。")
                    .small()
                    .weak(),
            );
            ui.add_space(10.0);
            ui.label(
                egui::RichText::new("词典地址已移到阅读窗口的「阅读设置」中调整。")
                    .small()
                    .weak(),
            );
            ui.add_space(6.0);

            let mut changed = false;
            ui.group(|ui| {
                ui.set_width(ui.available_width());
                changed |= self.ai_settings_ui(ui);
            });
            ui.add_space(8.0);
            ui.group(|ui| {
                ui.set_width(ui.available_width());
                changed |= self.proxy_settings_ui(ui);
            });
            ui.add_space(8.0);
            ui.group(|ui| {
                ui.set_width(ui.available_width());
                self.ai_usage_ui(ui);
            });

            if changed {
                self.save_user_settings();
            }
        });
    }

    /// 词典设置（阅读设置弹窗内使用）。
    pub fn dictionary_settings_ui(&mut self, ui: &mut egui::Ui) -> bool {
        let mut changed = false;
        ui.heading("词典");
        ui.label("选中文字后点工具条「词典」，按下面的模板在浏览器或词典 App 中查询。");
        ui.label("{word} 会替换为选中文字（自动 URL 编码），未写占位符时追加到末尾。");
        ui.add_space(6.0);

        ui.horizontal(|ui| {
            ui.label("地址模板");
            changed |= ui
                .add(
                    egui::TextEdit::singleline(&mut self.user_settings.dictionary_url)
                        .hint_text("eudic://dict/{word}")
                        .desired_width(360.0),
                )
                .changed();
            if ui.button("清空").clicked() {
                self.user_settings.dictionary_url.clear();
                changed = true;
            }
        });

        ui.horizontal(|ui| {
            ui.label("常用：");
            for (name, template) in DICTIONARY_PRESETS {
                if ui.button(name).clicked() {
                    self.user_settings.dictionary_url = template.to_string();
                    changed = true;
                }
            }
        });

        let settings = ReaderDictionarySettings {
            url: self.user_settings.dictionary_url.clone(),
        };
        if !settings.enabled() {
            ui.label(
                egui::RichText::new("未配置：工具条中的「词典」按钮不可用")
                    .small()
                    .weak(),
            );
        } else {
            let target = settings.target("hello");
            if has_scheme(&target) {
                ui.label(
                    egui::RichText::new(format!("将打开：{target}"))
                        .small()
                        .weak(),
                );
                if ui.button("试一下").clicked() {
                    if let Err(error) = open_target(&target) {
                        self.message = format!("打开词典失败：{error}");
                    }
                }
            } else {
                ui.colored_label(
                    egui::Color32::from_rgb(200, 80, 60),
                    "地址缺少 https:// 或 eudic:// 这样的 scheme",
                );
            }
        }
        changed
    }

    /// 网络代理设置：作用于 AI HTTP 接口请求与 AI CLI 子进程。
    fn proxy_settings_ui(&mut self, ui: &mut egui::Ui) -> bool {
        let mut changed = false;
        ui.heading("网络代理");
        ui.label(
            "仅作用于 AI 接口请求与 AI CLI 子进程；留空直连，HTTPS 留空时回退 HTTP 代理。",
        );
        ui.add_space(6.0);
        egui::Grid::new("settings_proxy")
            .num_columns(2)
            .spacing([12.0, 8.0])
            .show(ui, |ui| {
                ui.label("HTTP 代理");
                changed |= ui
                    .add(
                        egui::TextEdit::singleline(&mut self.user_settings.proxy_http)
                            .hint_text("http://127.0.0.1:7890")
                            .desired_width(360.0),
                    )
                    .changed();
                ui.end_row();

                ui.label("HTTPS 代理");
                changed |= ui
                    .add(
                        egui::TextEdit::singleline(&mut self.user_settings.proxy_https)
                            .hint_text("留空使用 HTTP 代理")
                            .desired_width(360.0),
                    )
                    .changed();
                ui.end_row();
            });
        changed
    }

    /// AI 用量：读取 `ai-usage.jsonl` 汇总展示，按日志修改时间缓存。
    fn ai_usage_ui(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.heading("AI 用量");
            if ui.button("刷新").clicked() {
                self.ai_usage = None;
            }
        });
        ui.label(
            egui::RichText::new(format!(
                "日志：{}",
                sepiola::ai::usage_log_path().display()
            ))
            .small()
            .weak(),
        );
        ui.add_space(4.0);
        let summary = self.cached_usage_summary();
        if summary.calls == 0 {
            ui.label("暂无调用记录。");
            return;
        }
        ui.label(format!(
            "累计 {} 次 · 输入 {} tokens · 输出 {} tokens · 耗时 {:.0}s",
            thousands(summary.calls as u64),
            thousands(summary.prompt_tokens),
            thousands(summary.completion_tokens),
            summary.elapsed_secs
        ));
        ui.label(format!(
            "今日 {} 次 · 输入 {} tokens · 输出 {} tokens",
            thousands(summary.today_calls as u64),
            thousands(summary.today_prompt_tokens),
            thousands(summary.today_completion_tokens)
        ));
        ui.add_space(6.0);
        egui::Grid::new("settings_ai_usage")
            .num_columns(5)
            .striped(true)
            .spacing([18.0, 6.0])
            .show(ui, |ui| {
                ui.strong("模型");
                ui.strong("调用");
                ui.strong("输入");
                ui.strong("输出");
                ui.strong("耗时");
                ui.end_row();
                for row in &summary.by_model {
                    let name = if row.model.trim().is_empty() {
                        row.provider.clone()
                    } else {
                        format!("{} / {}", row.provider, row.model)
                    };
                    ui.label(name);
                    ui.label(thousands(row.calls as u64));
                    ui.label(thousands(row.prompt_tokens));
                    ui.label(thousands(row.completion_tokens));
                    ui.label(format!("{:.0}s", row.elapsed_secs));
                    ui.end_row();
                }
            });
        ui.add_space(4.0);
        ui.label(
            egui::RichText::new(
                "tokens 为接口返回的真实值；CLI 后端拿不到 token 数据，只计调用次数。",
            )
            .small()
            .weak(),
        );
    }

    fn cached_usage_summary(&mut self) -> &sepiola::ai::UsageSummary {
        let path = sepiola::ai::usage_log_path();
        let modified = std::fs::metadata(&path)
            .and_then(|metadata| metadata.modified())
            .ok();
        let fresh = matches!(
            (&self.ai_usage, modified),
            (Some((stamp, _)), Some(current)) if *stamp == current
        );
        if !fresh {
            self.ai_usage = Some((
                modified.unwrap_or(std::time::SystemTime::UNIX_EPOCH),
                sepiola::ai::usage_summary(),
            ));
        }
        &self.ai_usage.as_ref().expect("刚写入用量缓存").1
    }

    /// 立即落盘用户设置并刷新工具条依赖的服务配置。
    pub fn save_user_settings(&mut self) {
        self.sync_reader_services();
        if let Err(error) = self.user_settings.save() {
            self.message = format!("保存设置失败：{error}");
        }
    }

    fn ai_settings_ui(&mut self, ui: &mut egui::Ui) -> bool {
        use sepiola::config::AiProvider;
        let mut changed = false;
        ui.heading("AI 服务");
        ui.label(
            "翻译、解释、分析本页与笔记本出题/造句共用此后端；未填写的项回退到 \
             config.toml 的 [reader.ai]。",
        );
        ui.add_space(6.0);

        ui.horizontal(|ui| {
            ui.label("后端");
            egui::ComboBox::from_id_salt("settings_ai_provider")
                .selected_text(AiProvider::from_key(&self.user_settings.ai_provider).label())
                .show_ui(ui, |ui| {
                    for provider in AiProvider::ALL {
                        let selected =
                            self.user_settings.ai_provider == provider.key();
                        if ui
                            .selectable_label(selected, provider.label())
                            .clicked()
                            && !selected
                        {
                            self.user_settings.ai_provider = provider.key().to_string();
                            changed = true;
                        }
                    }
                });
        });
        ui.add_space(6.0);

        let provider = AiProvider::from_key(&self.user_settings.ai_provider);
        match provider {
            AiProvider::Api => {
                egui::Grid::new("settings_ai")
                    .num_columns(2)
                    .spacing([12.0, 8.0])
                    .show(ui, |ui| {
                        ui.label("接口地址");
                        changed |= ui
                            .add(
                                egui::TextEdit::singleline(&mut self.user_settings.ai_base_url)
                                    .hint_text("https://api.example.com/v1")
                                    .desired_width(360.0),
                            )
                            .changed();
                        ui.end_row();

                        ui.label("API Key");
                        changed |= ui
                            .add(
                                egui::TextEdit::singleline(&mut self.user_settings.ai_api_key)
                                    .password(true)
                                    .desired_width(360.0),
                            )
                            .changed();
                        ui.end_row();

                        ui.label("模型");
                        changed |= ui
                            .add(
                                egui::TextEdit::singleline(&mut self.user_settings.ai_model)
                                    .hint_text("gpt-4o-mini")
                                    .desired_width(360.0),
                            )
                            .changed();
                        ui.end_row();
                    });
                if ui.button("清空").clicked() {
                    self.user_settings.ai_base_url.clear();
                    self.user_settings.ai_api_key.clear();
                    self.user_settings.ai_model.clear();
                    changed = true;
                }
            }
            AiProvider::Gemini => {
                egui::Grid::new("settings_ai_gemini")
                    .num_columns(2)
                    .spacing([12.0, 8.0])
                    .show(ui, |ui| {
                        ui.label("API Key");
                        changed |= ui
                            .add(
                                egui::TextEdit::singleline(&mut self.user_settings.ai_api_key)
                                    .password(true)
                                    .hint_text("AIza…")
                                    .desired_width(360.0),
                            )
                            .changed();
                        ui.end_row();

                        ui.label("模型");
                        changed |= ui
                            .add(
                                egui::TextEdit::singleline(&mut self.user_settings.ai_model)
                                    .hint_text("gemini-2.5-flash")
                                    .desired_width(360.0),
                            )
                            .changed();
                        ui.end_row();

                        ui.label("接口地址");
                        changed |= ui
                            .add(
                                egui::TextEdit::singleline(&mut self.user_settings.ai_base_url)
                                    .hint_text("留空使用官方地址")
                                    .desired_width(360.0),
                            )
                            .changed();
                        ui.end_row();
                    });
                if ui.button("清空").clicked() {
                    self.user_settings.ai_base_url.clear();
                    self.user_settings.ai_api_key.clear();
                    self.user_settings.ai_model.clear();
                    changed = true;
                }
            }
            AiProvider::Copilot | AiProvider::Command => {
                egui::Grid::new("settings_ai_cli")
                    .num_columns(2)
                    .spacing([12.0, 8.0])
                    .show(ui, |ui| {
                        ui.label("CLI 路径");
                        changed |= ui
                            .add(
                                egui::TextEdit::singleline(&mut self.user_settings.ai_cli_path)
                                    .hint_text("copilot")
                                    .desired_width(360.0),
                            )
                            .changed();
                        ui.end_row();

                        ui.label("参数");
                        changed |= ui
                            .add(
                                egui::TextEdit::singleline(&mut self.user_settings.ai_cli_args)
                                    .hint_text("-p")
                                    .desired_width(360.0),
                            )
                            .changed();
                        ui.end_row();

                        if provider == AiProvider::Copilot {
                            ui.label("模型");
                            changed |= ui
                                .add(
                                    egui::TextEdit::singleline(
                                        &mut self.user_settings.ai_cli_model,
                                    )
                                    .hint_text("gpt-6-luna")
                                    .desired_width(360.0),
                                )
                                .changed();
                            ui.end_row();
                        }
                    });
                ui.horizontal(|ui| {
                    let probing = self.ai_probe.is_some();
                    if ui
                        .add_enabled(
                            !probing,
                            egui::Button::new(if probing { "检测中…" } else { "检测" }),
                        )
                        .clicked()
                    {
                        let path = if self.user_settings.ai_cli_path.trim().is_empty() {
                            "copilot".to_string()
                        } else {
                            self.user_settings.ai_cli_path.trim().to_string()
                        };
                        let (sender, receiver) = std::sync::mpsc::channel();
                        std::thread::spawn(move || {
                            let _ = sender.send(sepiola::ai::probe_cli(
                                &path,
                                std::time::Duration::from_secs(8),
                            ));
                        });
                        self.ai_probe = Some(receiver);
                    }
                    ui.label(
                        egui::RichText::new("执行 --version / --help 探测")
                            .small()
                            .weak(),
                    );
                });
            }
        }

        let ai = self.reader_settings().ai;
        let status = if ai.enabled() {
            match ai.provider {
                AiProvider::Api => format!("已启用：api / {}", ai.model),
                AiProvider::Gemini => format!("已启用：gemini / {}", ai.model),
                provider => format!(
                    "已启用：{} / {} {}",
                    provider.key(),
                    ai.cli_path(),
                    ai.cli_args
                ),
            }
        } else {
            match ai.provider {
                AiProvider::Api => "未启用：需要同时填写接口地址与模型".to_string(),
                AiProvider::Gemini => "未启用：需要同时填写 API Key 与模型".to_string(),
                _ => "未启用：需要填写 CLI 路径".to_string(),
            }
        };
        ui.label(egui::RichText::new(status).small().weak());

        ui.add_space(8.0);
        // 默认提示词直接填入输入框，便于在原文基础上修改。
        let merged = self.reader_settings().ai.prompts.clone();
        prefill_prompt(
            &mut self.user_settings.ai_prompt_translate,
            &merged.translate,
            sepiola::reader::assist::DEFAULT_TRANSLATE_PROMPT,
        );
        prefill_prompt(
            &mut self.user_settings.ai_prompt_explain,
            &merged.explain,
            sepiola::reader::assist::DEFAULT_EXPLAIN_PROMPT,
        );
        prefill_prompt(
            &mut self.user_settings.ai_prompt_analyze,
            &merged.analyze,
            sepiola::reader::analyze::DEFAULT_PROMPT,
        );
        prefill_prompt(
            &mut self.user_settings.ai_prompt_exercise,
            &merged.exercise,
            sepiola::notebook::ai::DEFAULT_EXERCISE_PROMPT,
        );
        prefill_prompt(
            &mut self.user_settings.ai_prompt_sentence,
            &merged.sentence,
            sepiola::notebook::ai::DEFAULT_SENTENCE_PROMPT,
        );
        prefill_prompt(
            &mut self.user_settings.ai_prompt_card,
            &merged.card,
            sepiola::notebook::ai::DEFAULT_CARD_PROMPT,
        );
        prefill_prompt(
            &mut self.user_settings.ai_prompt_chat,
            &merged.chat,
            sepiola::reader::analyze::DEFAULT_CHAT_PROMPT,
        );

        egui::CollapsingHeader::new("提示词（高级）")
            .default_open(false)
            .show(ui, |ui| {
                ui.label(
                    egui::RichText::new(
                        "已填入内置默认提示词，可直接改写。分析本页、AI 出题、造句批改与生成卡片\
                         必须保留 JSON 输出格式要求，否则结果无法解析。",
                    )
                    .small()
                    .weak(),
                );
                changed |= prompt_editor(
                    ui,
                    "翻译",
                    &mut self.user_settings.ai_prompt_translate,
                    sepiola::reader::assist::DEFAULT_TRANSLATE_PROMPT,
                );
                changed |= prompt_editor(
                    ui,
                    "AI 解释",
                    &mut self.user_settings.ai_prompt_explain,
                    sepiola::reader::assist::DEFAULT_EXPLAIN_PROMPT,
                );
                changed |= prompt_editor(
                    ui,
                    "分析本页",
                    &mut self.user_settings.ai_prompt_analyze,
                    sepiola::reader::analyze::DEFAULT_PROMPT,
                );
                changed |= prompt_editor(
                    ui,
                    "AI 出题",
                    &mut self.user_settings.ai_prompt_exercise,
                    sepiola::notebook::ai::DEFAULT_EXERCISE_PROMPT,
                );
                changed |= prompt_editor(
                    ui,
                    "造句批改",
                    &mut self.user_settings.ai_prompt_sentence,
                    sepiola::notebook::ai::DEFAULT_SENTENCE_PROMPT,
                );
                changed |= prompt_editor(
                    ui,
                    "生成卡片",
                    &mut self.user_settings.ai_prompt_card,
                    sepiola::notebook::ai::DEFAULT_CARD_PROMPT,
                );
                changed |= prompt_editor(
                    ui,
                    "条目追问",
                    &mut self.user_settings.ai_prompt_chat,
                    sepiola::reader::analyze::DEFAULT_CHAT_PROMPT,
                );
            });
        changed
    }
}

/// 留空的提示词按「config.toml 的值 → 内置默认值」回填，让输入框直接显示可编辑的完整提示词。
fn prefill_prompt(field: &mut String, configured: &str, default: &str) {
    if !field.trim().is_empty() {
        return;
    }
    let value = if configured.trim().is_empty() {
        default
    } else {
        configured.trim()
    };
    *field = value.to_string();
}

/// 单个提示词编辑器；「恢复默认」回填内置提示词。
fn prompt_editor(ui: &mut egui::Ui, title: &str, value: &mut String, default: &str) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.label(title);
        if ui
            .button("恢复默认")
            .on_hover_text("回填内置提示词")
            .clicked()
        {
            *value = default.to_string();
            changed = true;
        }
    });
    changed |= ui
        .add(
            egui::TextEdit::multiline(value)
                .desired_rows(4)
                .desired_width(ui.available_width()),
        )
        .changed();
    ui.add_space(6.0);
    changed
}
