//! 记忆卡片笔记本页签（M47）：卡片管理、遗忘曲线复习、统计、AI 出题与造句批改。

use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, TryRecvError};

use eframe::egui;
use sepiola::config::ReaderAiSettings;
use sepiola::notebook::ai::{self, Exercise, ExerciseKind, SentenceCheck};
use sepiola::notebook::schedule::{self, Rating};
use sepiola::notebook::{now_unix, Notebook, NotebookStats, PracticeRecord};

use crate::assets::pointing_hand;
use crate::state::SepiolaApp;

/// 复习会话。
#[derive(Clone, Debug, Default)]
pub struct ReviewSession {
    pub queue: Vec<u64>,
    pub index: usize,
    pub flipped: bool,
}

impl ReviewSession {
    pub fn current(&self) -> Option<u64> {
        self.queue.get(self.index).copied()
    }
}

/// AI 出题状态。
#[derive(Default)]
pub struct ExerciseState {
    pub open: bool,
    pub loading: bool,
    pub error: Option<String>,
    pub items: Vec<Exercise>,
    pub receiver: Option<Receiver<Result<String, String>>>,
    pub selected: Vec<Option<usize>>,
    pub revealed: Vec<bool>,
}

/// 造句练习状态。
#[derive(Default)]
pub struct SentenceState {
    pub open: bool,
    pub card: Option<u64>,
    pub input: String,
    pub loading: bool,
    pub error: Option<String>,
    pub result: Option<SentenceCheck>,
    pub receiver: Option<Receiver<Result<String, String>>>,
}

/// 笔记本页签的界面状态与数据。
pub struct NotebookState {
    pub loaded_root: Option<PathBuf>,
    pub notebook: Notebook,
    pub ai: ReaderAiSettings,
    pub selected: Option<u64>,
    pub creating: bool,
    pub search: String,
    pub tag_filter: Option<String>,
    pub draft_front: String,
    pub draft_back: String,
    pub draft_tags: String,
    /// 新建卡片表单的 AI 生成状态：后台线程根据正面草稿生成卡片内容。
    pub draft_ai_loading: bool,
    pub draft_ai_error: Option<String>,
    pub draft_ai_receiver: Option<Receiver<Result<(Vec<ai::CardDraft>, Vec<String>), String>>>,
    pub message: String,
    pub review: Option<ReviewSession>,
    pub exercises: ExerciseState,
    pub sentence: SentenceState,
    pub dismissed_reminder: bool,
    /// 卡片背面 Markdown 渲染缓存（跨帧复用）。
    pub markdown_cache: egui_commonmark::CommonMarkCache,
    stats: Option<NotebookStats>,
    stats_at: i64,
}

impl Default for NotebookState {
    fn default() -> Self {
        Self {
            loaded_root: None,
            notebook: Notebook::default(),
            ai: ReaderAiSettings::default(),
            selected: None,
            creating: false,
            search: String::new(),
            tag_filter: None,
            draft_front: String::new(),
            draft_back: String::new(),
            draft_tags: String::new(),
            draft_ai_loading: false,
            draft_ai_error: None,
            draft_ai_receiver: None,
            message: String::new(),
            review: None,
            exercises: ExerciseState::default(),
            sentence: SentenceState::default(),
            dismissed_reminder: false,
            markdown_cache: egui_commonmark::CommonMarkCache::default(),
            stats: None,
            stats_at: 0,
        }
    }
}

impl NotebookState {
    /// 切换书库时重新载入；同一书库只刷新 AI 配置，保留界面状态。
    pub fn reload(&mut self, root: &Path, ai_settings: ReaderAiSettings) {
        self.ai = ai_settings;
        if self.loaded_root.as_deref() == Some(root) {
            return;
        }
        self.loaded_root = Some(root.to_path_buf());
        self.notebook = Notebook::load(root);
        self.selected = None;
        self.creating = false;
        self.search.clear();
        self.tag_filter = None;
        self.review = None;
        self.exercises = ExerciseState::default();
        self.sentence = SentenceState::default();
        self.draft_ai_loading = false;
        self.draft_ai_error = None;
        self.draft_ai_receiver = None;
        self.dismissed_reminder = false;
        self.message.clear();
        self.stats = None;
        self.stats_at = 0;
    }

    pub fn due_count(&self, now: i64) -> usize {
        self.notebook.due_count(now)
    }

    /// 统计较贵，按秒缓存；数据变化时调用 `invalidate_stats`。
    fn stats(&mut self, now: i64) -> NotebookStats {
        if self.stats.is_none() || self.stats_at != now {
            self.stats = Some(self.notebook.stats(now));
            self.stats_at = now;
        }
        self.stats.clone().unwrap_or_default()
    }

    pub(crate) fn invalidate_stats(&mut self) {
        self.stats = None;
    }

    fn select(&mut self, id: u64) {
        self.selected = Some(id);
        self.creating = false;
        if let Some(card) = self.notebook.card(id) {
            self.draft_front = card.front.clone();
            self.draft_back = card.back.clone();
            self.draft_tags = card.tags.join(", ");
        }
        self.exercises = ExerciseState::default();
        self.sentence = SentenceState::default();
    }

    fn start_create(&mut self) {
        self.creating = true;
        self.selected = None;
        self.draft_front.clear();
        self.draft_back.clear();
        self.draft_tags.clear();
        self.draft_ai_loading = false;
        self.draft_ai_error = None;
        self.draft_ai_receiver = None;
    }

    fn draft_tags(&self) -> Vec<String> {
        self.draft_tags
            .split([',', '，', ';', '；'])
            .map(str::trim)
            .filter(|tag| !tag.is_empty())
            .map(str::to_string)
            .collect()
    }
}

/// 列表行快照，避免渲染时长期借用笔记本。
struct CardRow {
    id: u64,
    front: String,
    due_at: i64,
    new_card: bool,
}

impl SepiolaApp {
    fn notebook_root(&self) -> Option<PathBuf> {
        self.notebook.loaded_root.clone()
    }

    pub fn notebook_save(&mut self) {
        let Some(root) = self.notebook_root() else {
            return;
        };
        if let Err(error) = self.notebook.notebook.save(&root) {
            self.notebook.message = format!("保存笔记本失败：{error}");
        }
    }

    /// 每分钟的界面轮询：笔记本 AI 请求与设置页 CLI 探测。
    pub fn poll_notebook(&mut self) {
        self.notebook.poll_exercises();
        self.notebook.poll_sentence();
        self.notebook.poll_draft_ai();
        if let Some(receiver) = self.ai_probe.as_ref() {
            let result = match receiver.try_recv() {
                Ok(result) => Some(result),
                Err(TryRecvError::Empty) => None,
                Err(TryRecvError::Disconnected) => {
                    Some(Err("探测线程已退出".to_string()))
                }
            };
            if let Some(result) = result {
                self.ai_probe = None;
                self.message = match result {
                    Ok(text) => format!("CLI 检测成功：{text}"),
                    Err(error) => format!("CLI 检测失败：{error}"),
                };
            }
        }
    }

    pub fn notebook_ui(&mut self, ui: &mut egui::Ui) {
        if self.notebook.loaded_root.is_none() {
            ui.heading("笔记本");
            ui.add_space(8.0);
            ui.weak("请先在「库」页选择库目录，笔记本按库保存在 .sepiola/notebook.json。");
            return;
        }
        let now = now_unix();
        ui.horizontal(|ui| {
            ui.heading("笔记本");
            ui.label(format!(
                "{} 张卡片 · {} 待复习",
                self.notebook.notebook.cards.len(),
                self.notebook.due_count(now)
            ));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if pointing_hand(ui.button("新建卡片")).clicked() {
                    self.notebook.start_create();
                }
                if pointer_hand_enabled(ui, !self.notebook.notebook.cards.is_empty(), "开始复习")
                    .clicked()
                {
                    self.start_review(now);
                }
            });
        });
        if !self.notebook.message.is_empty() {
            ui.colored_label(egui::Color32::from_rgb(190, 120, 60), &self.notebook.message);
        }
        let stats = self.notebook.stats(now);
        notebook_stats_ui(ui, &stats);
        ui.separator();

        if self.notebook.review.is_some() {
            self.review_ui(ui, now);
            self.exercise_window(ui.ctx());
            self.sentence_window(ui.ctx());
            return;
        }

        let height = ui.available_height().max(120.0);
        ui.horizontal_top(|ui| {
            ui.allocate_ui_with_layout(
                egui::vec2(300.0, height),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    self.card_list_ui(ui, now);
                },
            );
            ui.separator();
            ui.vertical(|ui| {
                self.card_detail_ui(ui, now);
            });
        });

        self.exercise_window(ui.ctx());
        self.sentence_window(ui.ctx());
    }

    fn start_review(&mut self, now: i64) {
        let queue = self.notebook.notebook.queue(now);
        if queue.is_empty() {
            self.notebook.message = "当前没有到期或新卡片".to_string();
            return;
        }
        self.notebook.review = Some(ReviewSession {
            queue,
            index: 0,
            flipped: false,
        });
        self.notebook.message.clear();
    }

    fn card_list_ui(&mut self, ui: &mut egui::Ui, now: i64) {
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.notebook.search)
                    .hint_text("搜索正面/背面/标签")
                    .desired_width(180.0),
            );
        });
        let tags = self.notebook.notebook.all_tags();
        ui.horizontal(|ui| {
            ui.label("标签");
            egui::ComboBox::from_id_salt("notebook_tag_filter")
                .selected_text(
                    self.notebook
                        .tag_filter
                        .clone()
                        .unwrap_or_else(|| "全部".to_string()),
                )
                .show_ui(ui, |ui| {
                    if ui
                        .selectable_label(self.notebook.tag_filter.is_none(), "全部")
                        .clicked()
                    {
                        self.notebook.tag_filter = None;
                    }
                    for tag in &tags {
                        let selected = self.notebook.tag_filter.as_deref() == Some(tag.as_str());
                        if ui.selectable_label(selected, tag).clicked() {
                            self.notebook.tag_filter = Some(tag.clone());
                        }
                    }
                });
        });
        let query = self.notebook.search.trim().to_lowercase();
        let tag_filter = self.notebook.tag_filter.clone();
        let mut rows: Vec<CardRow> = self
            .notebook
            .notebook
            .cards
            .iter()
            .filter(|card| {
                let matches_query = query.is_empty()
                    || card.front.to_lowercase().contains(&query)
                    || card.back.to_lowercase().contains(&query)
                    || card
                        .tags
                        .iter()
                        .any(|tag| tag.to_lowercase().contains(&query));
                let matches_tag = tag_filter
                    .as_ref()
                    .map_or(true, |tag| card.tags.contains(tag));
                matches_query && matches_tag
            })
            .map(|card| CardRow {
                id: card.id,
                front: truncate(&card.front, 26),
                due_at: card.due_at,
                new_card: card.last_reviewed_at.is_none(),
            })
            .collect();
        rows.sort_by_key(|row| (row.new_card, row.due_at, row.id));
        ui.separator();
        let mut select: Option<u64> = None;
        egui::ScrollArea::vertical()
            .id_salt("notebook_card_list")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                if rows.is_empty() {
                    ui.weak("还没有卡片，点右上角「新建卡片」开始。");
                }
                for row in &rows {
                    let selected = self.notebook.selected == Some(row.id) && !self.notebook.creating;
                    let status = if row.new_card {
                        "新卡".to_string()
                    } else if row.due_at <= now {
                        "待复习".to_string()
                    } else {
                        format!(
                            "{} 后",
                            schedule::format_interval(
                                (row.due_at - now) as f32 / (24.0 * 60.0 * 60.0)
                            )
                        )
                    };
                    let response = ui.selectable_label(
                        selected,
                        format!("{}\n{}", row.front, status),
                    );
                    if pointing_hand(response).clicked() {
                        select = Some(row.id);
                    }
                }
            });
        if let Some(id) = select {
            self.notebook.select(id);
        }
    }

    fn card_detail_ui(&mut self, ui: &mut egui::Ui, now: i64) {
        if !self.notebook.creating && self.notebook.selected.is_none() {
            ui.add_space(20.0);
            ui.weak("从左侧选择一张卡片，或新建卡片。");
            return;
        }
        let editing_id = if self.notebook.creating {
            None
        } else {
            self.notebook.selected
        };
        let card = editing_id.and_then(|id| self.notebook.notebook.card(id).cloned());
        ui.horizontal(|ui| {
            ui.heading(if self.notebook.creating {
                "新建卡片"
            } else {
                "编辑卡片"
            });
            if let Some(card) = &card {
                ui.weak(format!("#{}", card.id));
            }
        });
        egui::ScrollArea::vertical()
            .id_salt("notebook_detail_scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.label("正面（待记忆的词/短语/问题）");
                ui.add(
                    egui::TextEdit::multiline(&mut self.notebook.draft_front)
                        .desired_rows(2)
                        .desired_width(f32::INFINITY),
                );
                ui.label("背面（答案/释义）");
                ui.add(
                    egui::TextEdit::multiline(&mut self.notebook.draft_back)
                        .desired_rows(3)
                        .desired_width(f32::INFINITY),
                );
                ui.horizontal(|ui| {
                    ui.label("标签");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.notebook.draft_tags)
                            .hint_text("逗号分隔")
                            .desired_width(320.0),
                    );
                });
                ui.add_space(6.0);
                let mut save = false;
                let mut remove = false;
                let mut start_review = false;
                let mut exercises = false;
                let mut sentence = false;
                let mut ai_generate = false;
                ui.horizontal(|ui| {
                    if self.notebook.creating {
                        let enabled = self.notebook.ai.enabled()
                            && !self.notebook.draft_front.trim().is_empty()
                            && !self.notebook.draft_ai_loading;
                        if pointing_hand(ui.add_enabled(
                            enabled,
                            egui::Button::new("AI 生成"),
                        ))
                        .clicked()
                        {
                            ai_generate = true;
                        }
                        if self.notebook.draft_ai_loading {
                            ui.spinner();
                            ui.weak("正在生成卡片内容…");
                        }
                    }
                    if pointing_hand(ui.button("保存")).clicked() {
                        save = true;
                    }
                    if !self.notebook.creating && pointing_hand(ui.button("删除")).clicked() {
                        remove = true;
                    }
                    if !self.notebook.creating && pointing_hand(ui.button("复习这张")).clicked() {
                        start_review = true;
                    }
                    if !self.notebook.creating {
                        let enabled = self.notebook.ai.enabled();
                        if pointing_hand(ui.add_enabled(enabled, egui::Button::new("AI 出题")))
                            .clicked()
                        {
                            exercises = true;
                        }
                        if pointing_hand(ui.add_enabled(enabled, egui::Button::new("造句练习")))
                            .clicked()
                        {
                            sentence = true;
                        }
                    }
                });
                if !self.notebook.ai.enabled() {
                    ui.label(
                        egui::RichText::new("AI 未启用：「设置 → AI 服务」配置后可用。")
                            .small()
                            .weak(),
                    );
                }
                if let Some(error) = &self.notebook.draft_ai_error {
                    ui.colored_label(egui::Color32::from_rgb(200, 70, 70), error.as_str());
                }
                if save {
                    self.save_card(editing_id, now);
                }
                if remove {
                    if let Some(id) = editing_id {
                        if self.notebook.notebook.remove(id) {
                            self.notebook.selected = None;
                            self.notebook.creating = false;
                            self.notebook.message = "已删除卡片".to_string();
                            self.notebook.invalidate_stats();
                            self.notebook_save();
                        }
                    }
                }
                if start_review {
                    if let Some(id) = editing_id {
                        self.notebook.review = Some(ReviewSession {
                            queue: vec![id],
                            index: 0,
                            flipped: false,
                        });
                    }
                }
                if exercises {
                    self.start_exercises();
                }
                if sentence {
                    self.start_sentence();
                }
                if ai_generate {
                    self.start_draft_ai();
                }

                if let Some(card) = &card {
                    ui.separator();
                    ui.strong("复习历史");
                    if card.history.is_empty() {
                        ui.weak("还没有复习记录");
                    } else {
                        egui::Grid::new(("notebook_history", card.id))
                            .num_columns(3)
                            .spacing([18.0, 4.0])
                            .show(ui, |ui| {
                                ui.weak("时间");
                                ui.weak("评分");
                                ui.weak("下次间隔");
                                ui.end_row();
                                for log in card.history.iter().rev().take(20) {
                                    ui.label(relative_time(log.at, now));
                                    ui.label(log.rating.label());
                                    ui.label(schedule::format_interval(log.next_interval_days));
                                    ui.end_row();
                                }
                            });
                    }
                    ui.separator();
                    ui.strong("最近一次造句");
                    match &card.practice {
                        Some(practice) => {
                            ui.label(format!("{}：{}", relative_time(practice.at, now), practice.sentence));
                            ui.label(
                                egui::RichText::new(&practice.corrected)
                                    .color(if practice.ok {
                                        egui::Color32::from_rgb(60, 150, 90)
                                    } else {
                                        egui::Color32::from_rgb(200, 120, 60)
                                    }),
                            );
                            if !practice.comment.is_empty() {
                                ui.label(egui::RichText::new(&practice.comment).small().weak());
                            }
                        }
                        None => {
                            ui.weak("还没有造句记录");
                        }
                    }
                }
            });
    }

    fn save_card(&mut self, editing_id: Option<u64>, now: i64) {
        let front = self.notebook.draft_front.trim().to_string();
        if front.is_empty() {
            self.notebook.message = "正面不能为空".to_string();
            return;
        }
        let back = self.notebook.draft_back.trim().to_string();
        let tags = self.notebook.draft_tags();
        if let Some(id) = editing_id {
            self.notebook.notebook.update(id, &front, &back, tags, now);
            self.notebook.message = "已保存".to_string();
        } else {
            let id = self.notebook.notebook.add(&front, &back, tags, now);
            self.notebook.selected = Some(id);
            self.notebook.creating = false;
            self.notebook.message = "已新建卡片".to_string();
        }
        self.notebook.invalidate_stats();
        self.notebook_save();
    }

    fn review_ui(&mut self, ui: &mut egui::Ui, now: i64) {
        let Some(session) = self.notebook.review.clone() else {
            return;
        };
        let mut rating: Option<Rating> = None;
        let mut advance = false;
        let mut finish = false;
        let card = session
            .current()
            .and_then(|id| self.notebook.notebook.card(id).cloned());
        ui.horizontal(|ui| {
            if pointing_hand(ui.button("← 返回列表")).clicked() {
                finish = true;
            }
            ui.label(format!(
                "{}/{}",
                (session.index + 1).min(session.queue.len()),
                session.queue.len()
            ));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if pointing_hand(ui.button("跳过")).clicked() {
                    advance = true;
                }
            });
        });
        if finish {
            self.notebook.review = None;
            return;
        }
        ui.separator();
        let Some(card) = card else {
            ui.add_space(30.0);
            ui.heading("本轮复习完成");
            let next_due = self
                .notebook
                .notebook
                .cards
                .iter()
                .filter(|card| card.last_reviewed_at.is_some() && card.due_at > now)
                .map(|card| card.due_at)
                .min();
            match next_due {
                Some(next) => ui.label(format!(
                    "下一次到期：{} 后",
                    schedule::format_interval((next - now) as f32 / 86400.0)
                )),
                None => ui.weak("暂无已安排的复习"),
            };
            if pointing_hand(ui.button("返回列表")).clicked() {
                self.notebook.review = None;
            }
            return;
        };
        egui::ScrollArea::vertical()
            .id_salt("notebook_review_scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.add_space(24.0);
                let front = ui.add(
                    egui::Label::new(egui::RichText::new(&card.front).size(28.0).strong())
                        .sense(egui::Sense::click()),
                );
                if pointing_hand(front).clicked() && !session.flipped {
                    advance_flip(&mut self.notebook);
                }
                if !card.tags.is_empty() {
                    ui.label(egui::RichText::new(card.tags.join(" · ")).small().weak());
                }
                ui.add_space(24.0);
                if session.flipped {
                    ui.separator();
                    ui.add_space(12.0);
                    // 背面通常是 AI 生成的 Markdown，按格式渲染。
                    ui.scope(|ui| {
                        ui.style_mut().text_styles.insert(
                            egui::TextStyle::Body,
                            egui::FontId::proportional(22.0),
                        );
                        egui_commonmark::CommonMarkViewer::new().show(
                            ui,
                            &mut self.notebook.markdown_cache,
                            &card.back,
                        );
                    });
                    ui.add_space(24.0);
                    ui.horizontal_wrapped(|ui| {
                        for (candidate, next) in schedule::preview(&card, now) {
                            let label = format!(
                                "{} · {}",
                                candidate.label(),
                                schedule::format_interval(next.interval_days)
                            );
                            if pointing_hand(ui.add_sized(
                                [120.0, 38.0],
                                egui::Button::new(label),
                            ))
                            .clicked()
                            {
                                rating = Some(candidate);
                            }
                        }
                    });
                } else if pointing_hand(ui.add_sized(
                    [160.0, 40.0],
                    egui::Button::new("显示答案（空格）"),
                ))
                .clicked()
                {
                    advance_flip(&mut self.notebook);
                }
                if ui.input(|input| input.key_pressed(egui::Key::Space)) {
                    advance_flip(&mut self.notebook);
                }
            });
        if let Some(rating) = rating {
            if let Err(error) = self.notebook.notebook.review(card.id, rating, now) {
                self.notebook.message = error;
            } else {
                self.notebook.invalidate_stats();
                self.notebook_save();
                if let Some(session) = &mut self.notebook.review {
                    session.index += 1;
                    session.flipped = false;
                }
            }
        } else if advance {
            if let Some(session) = &mut self.notebook.review {
                session.index += 1;
                session.flipped = false;
            }
        }
    }

    pub fn start_exercises(&mut self) {
        let Some(id) = self.notebook.selected else {
            self.notebook.message = "请先选择一张卡片".to_string();
            return;
        };
        let Some(card) = self.notebook.notebook.card(id).cloned() else {
            return;
        };
        if !self.notebook.ai.enabled() {
            self.notebook.message = sepiola::ai::MISSING_HINT.to_string();
            return;
        }
        let settings = self.notebook.ai.clone();
        let system = ai::exercise_system(&settings).to_string();
        let (sender, receiver) = channel();
        std::thread::spawn(move || {
            let _ = sender.send(sepiola::ai::ask(
                &settings,
                &system,
                &ai::exercise_user(&card),
            ));
        });
        self.notebook.exercises = ExerciseState {
            open: true,
            loading: true,
            receiver: Some(receiver),
            ..ExerciseState::default()
        };
    }

    pub fn start_sentence(&mut self) {
        let Some(id) = self.notebook.selected else {
            self.notebook.message = "请先选择一张卡片".to_string();
            return;
        };
        if !self.notebook.ai.enabled() {
            self.notebook.message = sepiola::ai::MISSING_HINT.to_string();
            return;
        }
        self.notebook.sentence = SentenceState {
            open: true,
            card: Some(id),
            ..SentenceState::default()
        };
    }

    /// 新建卡片表单的 AI 生成：以正面草稿为候选，复用阅读页工具条的
    /// 「生成卡片」流程（程序去重 → AI 语义去重并生成），结果回填表单。
    fn start_draft_ai(&mut self) {
        let text = self.notebook.draft_front.trim().to_string();
        if text.is_empty() {
            self.notebook.message = "请先填写正面内容".to_string();
            return;
        }
        if !self.notebook.ai.enabled() {
            self.notebook.message = sepiola::ai::MISSING_HINT.to_string();
            return;
        }
        if self.notebook.notebook.find_duplicate(&text).is_some() {
            self.notebook.message = "该条目已在笔记本中".to_string();
            return;
        }
        let settings = self.notebook.ai.clone();
        let existing: Vec<String> = self
            .notebook
            .notebook
            .cards
            .iter()
            .map(|card| card.front.clone())
            .collect();
        let system = ai::card_system(&settings).to_string();
        let user = ai::card_user(std::slice::from_ref(&text), &existing);
        let (sender, receiver) = channel();
        std::thread::spawn(move || {
            let result = sepiola::ai::ask(&settings, &system, &user)
                .and_then(|reply| ai::parse_cards(&reply));
            let _ = sender.send(result);
        });
        self.notebook.draft_ai_loading = true;
        self.notebook.draft_ai_error = None;
        self.notebook.draft_ai_receiver = Some(receiver);
    }

    fn submit_sentence(&mut self) {
        let Some(id) = self.notebook.sentence.card else {
            return;
        };
        let Some(card) = self.notebook.notebook.card(id).cloned() else {
            return;
        };
        let input = self.notebook.sentence.input.trim().to_string();
        if input.is_empty() {
            self.notebook.sentence.error = Some("请先写一句造句".to_string());
            return;
        }
        if !self.notebook.ai.enabled() {
            self.notebook.sentence.error = Some(sepiola::ai::MISSING_HINT.to_string());
            return;
        }
        let settings = self.notebook.ai.clone();
        let system = ai::sentence_system(&settings).to_string();
        let user = ai::sentence_user(&card, &input);
        let (sender, receiver) = channel();
        std::thread::spawn(move || {
            let _ = sender.send(sepiola::ai::ask(&settings, &system, &user));
        });
        self.notebook.sentence.loading = true;
        self.notebook.sentence.error = None;
        self.notebook.sentence.result = None;
        self.notebook.sentence.receiver = Some(receiver);
    }

    fn exercise_window(&mut self, ctx: &egui::Context) {
        if !self.notebook.exercises.open {
            return;
        }
        let loading = self.notebook.exercises.loading;
        let error = self.notebook.exercises.error.clone();
        let items = self.notebook.exercises.items.clone();
        let mut open = true;
        let mut close = false;
        let mut retry = false;
        let mut selected = self.notebook.exercises.selected.clone();
        let mut revealed = self.notebook.exercises.revealed.clone();
        egui::Window::new("AI 出题")
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .default_width(480.0)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    if ui.add_enabled(!loading, egui::Button::new("重新出题")).clicked() {
                        retry = true;
                    }
                    if loading {
                        ui.spinner();
                        ui.label("正在出题…");
                    }
                });
                if let Some(error) = &error {
                    ui.colored_label(egui::Color32::from_rgb(200, 70, 70), error.as_str());
                }
                egui::ScrollArea::vertical()
                    .id_salt("notebook_exercise_scroll")
                    .max_height(460.0)
                    .show(ui, |ui| {
                        for (index, exercise) in items.iter().enumerate() {
                            let kind = exercise.kind.unwrap_or(ExerciseKind::Cloze);
                            ui.horizontal_wrapped(|ui| {
                                ui.colored_label(
                                    egui::Color32::from_rgb(64, 126, 214),
                                    format!("[{}]", kind.label()),
                                );
                                ui.label(egui::RichText::new(&exercise.question).strong());
                            });
                            if kind == ExerciseKind::Choice && exercise.options.len() > 1 {
                                for (option_index, option) in exercise.options.iter().enumerate() {
                                    let value = &mut selected[index];
                                    ui.radio_value(value, Some(option_index), option.as_str());
                                }
                            }
                            let is_revealed = revealed.get(index).copied().unwrap_or(false);
                            ui.horizontal(|ui| {
                                if ui
                                    .button(if is_revealed { "隐藏答案" } else { "显示答案" })
                                    .clicked()
                                {
                                    revealed[index] = !is_revealed;
                                }
                                if kind == ExerciseKind::Choice && is_revealed {
                                    let correct = selected[index]
                                        .and_then(|option| exercise.options.get(option))
                                        .is_some_and(|option| option.trim() == exercise.answer.trim());
                                    ui.colored_label(
                                        if correct {
                                            egui::Color32::from_rgb(60, 150, 90)
                                        } else {
                                            egui::Color32::from_rgb(200, 90, 70)
                                        },
                                        if correct { "正确" } else { "再想想" },
                                    );
                                }
                            });
                            if revealed.get(index).copied().unwrap_or(false) {
                                ui.label(format!("答案：{}", exercise.answer));
                                if !exercise.explanation.is_empty() {
                                    ui.label(
                                        egui::RichText::new(&exercise.explanation).small().weak(),
                                    );
                                }
                            }
                            ui.separator();
                        }
                        if !loading && items.is_empty() && error.is_none() {
                            ui.weak("还没有习题");
                        }
                    });
                ui.horizontal(|ui| {
                    if ui.button("关闭").clicked() {
                        close = true;
                    }
                });
            });
        self.notebook.exercises.selected = selected;
        self.notebook.exercises.revealed = revealed;
        if retry {
            self.start_exercises();
        }
        if !open || close {
            self.notebook.exercises.open = false;
        }
    }

    fn sentence_window(&mut self, ctx: &egui::Context) {
        if !self.notebook.sentence.open {
            return;
        }
        let loading = self.notebook.sentence.loading;
        let error = self.notebook.sentence.error.clone();
        let result = self.notebook.sentence.result.clone();
        let mut open = true;
        let mut close = false;
        let mut submit = false;
        let mut copy = None;
        egui::Window::new("造句练习")
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .default_width(480.0)
            .show(ctx, |ui| {
                if let Some(id) = self.notebook.sentence.card {
                    if let Some(card) = self.notebook.notebook.card(id) {
                        ui.label(format!(
                            "用「{}」造一句（释义：{}）",
                            card.front, card.back
                        ));
                    }
                }
                ui.add(
                    egui::TextEdit::multiline(&mut self.notebook.sentence.input)
                        .desired_rows(3)
                        .desired_width(f32::INFINITY)
                        .hint_text("在这里写句子…"),
                );
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(!loading, egui::Button::new("检查"))
                        .clicked()
                    {
                        submit = true;
                    }
                    if loading {
                        ui.spinner();
                        ui.label("AI 正在批改…");
                    }
                });
                if let Some(error) = &error {
                    ui.colored_label(egui::Color32::from_rgb(200, 70, 70), error.as_str());
                }
                if let Some(result) = &result {
                    ui.separator();
                    ui.horizontal(|ui| {
                        ui.colored_label(
                            if result.ok {
                                egui::Color32::from_rgb(60, 150, 90)
                            } else {
                                egui::Color32::from_rgb(200, 120, 60)
                            },
                            if result.ok { "表达正确" } else { "建议修改" },
                        );
                        if ui.button("复制修改稿").clicked() {
                            copy = Some(result.corrected.clone());
                        }
                    });
                    ui.label(egui::RichText::new(&result.corrected).size(17.0));
                    for issue in &result.issues {
                        ui.horizontal_wrapped(|ui| {
                            if !issue.span.is_empty() {
                                ui.monospace(&issue.span);
                            }
                            if !issue.kind.is_empty() {
                                ui.weak(&issue.kind);
                            }
                            ui.label(&issue.explanation);
                        });
                    }
                    if !result.comment.is_empty() {
                        ui.label(egui::RichText::new(&result.comment).small().weak());
                    }
                }
                ui.separator();
                if ui.button("关闭").clicked() {
                    close = true;
                }
            });
        if let Some(text) = copy {
            ctx.copy_text(text);
        }
        if submit {
            self.submit_sentence();
        }
        if !open || close {
            self.notebook.sentence.open = false;
        }
    }
}

impl NotebookState {
    fn poll_exercises(&mut self) {
        let result = match self.exercises.receiver.as_ref() {
            Some(receiver) => match receiver.try_recv() {
                Ok(result) => Some(result),
                Err(TryRecvError::Empty) => None,
                Err(TryRecvError::Disconnected) => Some(Err("出题线程已退出".to_string())),
            },
            None => None,
        };
        let Some(result) = result else { return };
        self.exercises.receiver = None;
        self.exercises.loading = false;
        match result {
            Ok(reply) => match ai::parse_exercises(&reply) {
                Ok(items) => {
                    self.exercises.selected = vec![None; items.len()];
                    self.exercises.revealed = vec![false; items.len()];
                    self.exercises.items = items;
                    self.exercises.error = None;
                }
                Err(error) => self.exercises.error = Some(error),
            },
            Err(error) => self.exercises.error = Some(error),
        }
    }

    fn poll_draft_ai(&mut self) {
        let result = match self.draft_ai_receiver.as_ref() {
            Some(receiver) => match receiver.try_recv() {
                Ok(result) => Some(result),
                Err(TryRecvError::Empty) => None,
                Err(TryRecvError::Disconnected) => {
                    Some(Err("卡片生成线程已退出".to_string()))
                }
            },
            None => None,
        };
        let Some(result) = result else { return };
        self.draft_ai_receiver = None;
        self.draft_ai_loading = false;
        match result {
            Ok((cards, skipped)) => {
                // 表单一次只放一张卡片：取 AI 返回的第一张回填草稿。
                if let Some(draft) = cards.into_iter().next() {
                    self.draft_front = draft.front;
                    self.draft_back = draft.back;
                    self.draft_tags = draft.tags.join(", ");
                    self.draft_ai_error = None;
                } else if let Some(reason) = skipped.into_iter().next() {
                    self.draft_ai_error = Some(format!("AI 判定与已有卡片重复：{reason}"));
                } else {
                    self.draft_ai_error = Some("AI 未返回可用卡片".to_string());
                }
            }
            Err(error) => self.draft_ai_error = Some(error),
        }
    }

    fn poll_sentence(&mut self) {
        let result = match self.sentence.receiver.as_ref() {
            Some(receiver) => match receiver.try_recv() {
                Ok(result) => Some(result),
                Err(TryRecvError::Empty) => None,
                Err(TryRecvError::Disconnected) => Some(Err("批改线程已退出".to_string())),
            },
            None => None,
        };
        let Some(result) = result else { return };
        self.sentence.receiver = None;
        self.sentence.loading = false;
        match result {
            Ok(reply) => match ai::parse_sentence_check(&reply) {
                Ok(check) => {
                    if let (Some(id), Some(root)) = (self.sentence.card, self.loaded_root.clone()) {
                        let record = PracticeRecord {
                            at: now_unix(),
                            sentence: self.sentence.input.trim().to_string(),
                            ok: check.ok,
                            corrected: check.corrected.clone(),
                            comment: check.comment.clone(),
                            issues: check.issues.clone(),
                        };
                        if self.notebook.record_practice(id, record) {
                            self.invalidate_stats();
                            let _ = self.notebook.save(&root);
                        }
                    }
                    self.sentence.result = Some(check);
                    self.sentence.error = None;
                }
                Err(error) => self.sentence.error = Some(error),
            },
            Err(error) => self.sentence.error = Some(error),
        }
    }
}

fn pointer_hand_enabled(ui: &mut egui::Ui, enabled: bool, text: &str) -> egui::Response {
    pointing_hand(ui.add_enabled(enabled, egui::Button::new(text)))
}

fn advance_flip(notebook: &mut NotebookState) {
    if let Some(session) = &mut notebook.review {
        session.flipped = true;
    }
}

fn notebook_stats_ui(ui: &mut egui::Ui, stats: &NotebookStats) {
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new(format!("待复习 {} · 新卡 {} · 今日复习 {} · 连续 {} 天",
                stats.due, stats.new_cards, stats.reviewed_today, stats.streak))
                .small()
                .weak(),
        );
    });
    let height = 54.0;
    let (response, painter) =
        ui.allocate_painter(egui::vec2(ui.available_width(), height), egui::Sense::hover());
    let rect = response.rect;
    let max = stats.daily.iter().map(|(_, count)| *count).max().unwrap_or(0).max(1);
    let gap = 2.0;
    let bar_width = ((rect.width() - gap * (stats.daily.len() as f32 - 1.0))
        / stats.daily.len().max(1) as f32)
        .max(1.0);
    let fill = egui::Color32::from_rgb(64, 126, 214);
    for (index, (_, count)) in stats.daily.iter().enumerate() {
        let bar_height = (rect.height() - 4.0) * (*count as f32 / max as f32);
        let left = rect.left() + index as f32 * (bar_width + gap);
        let bar = egui::Rect::from_min_max(
            egui::pos2(left, rect.bottom() - bar_height),
            egui::pos2(left + bar_width, rect.bottom()),
        );
        painter.rect_filled(bar, 1.0, fill);
    }
    if stats.daily.iter().all(|(_, count)| *count == 0) {
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            "近 30 天还没有复习记录",
            egui::FontId::proportional(12.0),
            ui.visuals().weak_text_color(),
        );
    }
}

fn truncate(text: &str, max_chars: usize) -> String {
    let mut truncated: String = text.chars().take(max_chars).collect();
    if text.chars().count() > max_chars {
        truncated.push('…');
    }
    truncated
}

fn relative_time(at: i64, now: i64) -> String {
    let diff = (now - at).max(0);
    if diff < 60 {
        "刚刚".to_string()
    } else if diff < 3600 {
        format!("{} 分钟前", diff / 60)
    } else if diff < 86_400 {
        format!("{} 小时前", diff / 3600)
    } else {
        format!("{} 天前", diff / 86_400)
    }
}
