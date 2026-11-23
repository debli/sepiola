use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;

use crate::compare_engine::{self, CompareItem, CompareMode, CompareReport};
use crate::state::SepiolaApp;

#[derive(Clone)]
pub(super) enum ConfirmAction {
    Sync {
        a_to_b: bool,
    },
    Delete {
        keep_a: bool,
        paths: Vec<PathBuf>,
    },
    Copy {
        a_to_b: bool,
        files: Vec<(PathBuf, PathBuf)>,
    },
}

enum CompareMessage {
    Progress(usize, usize),
    ScanPath {
        side_a: bool,
        relative: PathBuf,
    },
    Item(CompareItem),
    Compared(Result<CompareReport, String>),
    DeletePreview {
        keep_a: bool,
        result: Result<Vec<PathBuf>, String>,
    },
    ActionDone(Result<String, String>),
}

pub struct CompareState {
    pub root_a: String,
    pub root_b: String,
    pub mode: CompareMode,
    pub report: Option<CompareReport>,
    pub running: bool,
    pub progress: (usize, usize),
    pub selected_items: HashSet<(Option<PathBuf>, Option<PathBuf>)>,
    pub scanned_files: BTreeMap<PathBuf, (bool, bool)>,
    pub message: String,
    pub(super) cancel: Arc<AtomicBool>,
    receiver: Option<Receiver<CompareMessage>>,
    sender: Option<Sender<CompareMessage>>,
    pub(super) confirm: Option<ConfirmAction>,
}

impl Default for CompareState {
    fn default() -> Self {
        Self {
            root_a: String::new(),
            root_b: String::new(),
            mode: CompareMode::Files,
            report: None,
            running: false,
            progress: (0, 0),
            selected_items: HashSet::new(),
            scanned_files: BTreeMap::new(),
            message: String::new(),
            cancel: Arc::new(AtomicBool::new(false)),
            receiver: None,
            sender: None,
            confirm: None,
        }
    }
}

impl SepiolaApp {
    pub fn poll_compare(&mut self) {
        let messages: Vec<_> = self
            .compare
            .receiver
            .as_ref()
            .map_or_else(Vec::new, |receiver| {
                let mut messages = Vec::new();
                let mut item_count = 0;
                while messages.len() < 128 && item_count < 32 {
                    let Ok(message) = receiver.try_recv() else {
                        break;
                    };
                    if matches!(&message, CompareMessage::Item(_)) {
                        item_count += 1;
                    }
                    messages.push(message);
                }
                messages
            });
        for message in messages {
            match message {
                CompareMessage::Progress(done, total) => {
                    self.compare.progress = (done, total);
                }
                CompareMessage::ScanPath { side_a, relative } => {
                    let scanned = self.compare.scanned_files.entry(relative).or_default();
                    if side_a {
                        scanned.0 = true;
                    } else {
                        scanned.1 = true;
                    }
                    if self.compare.scanned_files.len() > 250 {
                        if let Some(first) = self.compare.scanned_files.keys().next().cloned() {
                            self.compare.scanned_files.remove(&first);
                        }
                    }
                }
                CompareMessage::Item(item) => {
                    self.compare.scanned_files.clear();
                    if let Some(report) = &mut self.compare.report {
                        report.items.push(item);
                    } else {
                        self.compare.report = Some(CompareReport {
                            items: vec![item],
                            files_a: 0,
                            files_b: 0,
                            cancelled: false,
                        });
                    }
                }
                CompareMessage::Compared(result) => {
                    self.compare.running = false;
                    self.compare.scanned_files.clear();
                    match result {
                        Ok(mut report) => {
                            if report.cancelled {
                                if let Some(existing) = self.compare.report.take() {
                                    let mut seen: HashSet<_> = report
                                        .items
                                        .iter()
                                        .map(|item| (item.a.clone(), item.b.clone()))
                                        .collect();
                                    report.items.extend(existing.items.into_iter().filter(
                                        |item| seen.insert((item.a.clone(), item.b.clone())),
                                    ));
                                }
                            }
                            report.items.sort_by(|left, right| {
                                left.a.cmp(&right.a).then(left.b.cmp(&right.b))
                            });
                            self.compare.message = if report.cancelled {
                                "比较已取消".to_string()
                            } else {
                                format!(
                                    "比较完成：A {} 个、B {} 个，发现 {} 项",
                                    report.files_a,
                                    report.files_b,
                                    report.items.len()
                                )
                            };
                            self.compare.report = Some(report);
                        }
                        Err(error) => {
                            self.compare.report = None;
                            self.compare.message = error;
                        }
                    }
                }
                CompareMessage::DeletePreview { keep_a, result } => match result {
                    Ok(paths) => {
                        self.compare.running = false;
                        self.compare.message =
                            format!("找到 {} 个 SHA-256 完全相同的副本", paths.len());
                        self.compare.confirm = Some(ConfirmAction::Delete { keep_a, paths });
                    }
                    Err(error) => {
                        self.compare.running = false;
                        self.compare.message = error;
                    }
                },
                CompareMessage::ActionDone(result) => {
                    self.compare.running = false;
                    self.compare.confirm = None;
                    self.compare.message = result.unwrap_or_else(|error| error);
                }
            }
        }
    }

    pub(super) fn start_compare(&mut self) {
        let a = PathBuf::from(self.compare.root_a.trim());
        let b = PathBuf::from(self.compare.root_b.trim());
        if let Err(error) = validate_roots(&a, &b) {
            self.compare.message = error;
            return;
        }
        self.compare.report = None;
        self.compare.selected_items.clear();
        self.compare.scanned_files.clear();
        self.compare.progress = (0, 0);
        self.compare.running = true;
        self.compare.cancel = Arc::new(AtomicBool::new(false));
        let cancel = Arc::clone(&self.compare.cancel);
        let mode = self.compare.mode;
        let (sender, receiver) = channel();
        self.compare.sender = Some(sender.clone());
        self.compare.receiver = Some(receiver);
        self.compare.message = "正在比较目录…".to_string();
        std::thread::spawn(move || {
            let progress_sender = sender.clone();
            let scan_sender = sender.clone();
            let item_sender = sender.clone();
            let result = compare_engine::compare_roots(
                &a,
                &b,
                mode,
                &cancel,
                move |done, total| {
                    let _ = progress_sender.send(CompareMessage::Progress(done, total));
                },
                move |side_a, relative| {
                    let _ = scan_sender.send(CompareMessage::ScanPath { side_a, relative });
                },
                move |item| {
                    let _ = item_sender.send(CompareMessage::Item(item));
                },
            );
            let _ = sender.send(CompareMessage::Compared(result));
        });
    }

    pub(super) fn start_delete_preview(&mut self, keep_a: bool) {
        let a = PathBuf::from(self.compare.root_a.trim());
        let b = PathBuf::from(self.compare.root_b.trim());
        if let Err(error) = validate_roots(&a, &b) {
            self.compare.message = error;
            return;
        }
        let (keep, remove) = if keep_a { (a, b) } else { (b, a) };
        self.start_background();
        self.compare.message = "正在计算精确重复项…".to_string();
        let cancel = Arc::clone(&self.compare.cancel);
        let mode = self.compare.mode;
        let sender = self.compare.sender.as_ref().unwrap().clone();
        std::thread::spawn(move || {
            let result = compare_engine::exact_duplicate_paths(&keep, &remove, mode, &cancel);
            let _ = sender.send(CompareMessage::DeletePreview { keep_a, result });
        });
    }

    fn start_background(&mut self) {
        self.compare.running = true;
        self.compare.progress = (0, 0);
        self.compare.cancel = Arc::new(AtomicBool::new(false));
        let (sender, receiver) = channel();
        self.compare.sender = Some(sender);
        self.compare.receiver = Some(receiver);
    }

    pub(super) fn execute_confirmed(&mut self, action: ConfirmAction) {
        self.compare.confirm = None;
        self.start_background();
        let cancel = Arc::clone(&self.compare.cancel);
        let sender = self.compare.sender.as_ref().unwrap().clone();
        let mode = self.compare.mode;
        let a = PathBuf::from(self.compare.root_a.trim());
        let b = PathBuf::from(self.compare.root_b.trim());
        self.compare.message = "正在执行…".to_string();
        std::thread::spawn(move || {
            let result = match action {
                ConfirmAction::Sync { a_to_b } => {
                    let (source, target) = if a_to_b { (&a, &b) } else { (&b, &a) };
                    compare_engine::sync_files(source, target, mode, &cancel, |done, total| {
                        let _ = sender.send(CompareMessage::Progress(done, total));
                    })
                    .map(|(copied, total)| format!("同步完成：复制 {copied}/{total} 个文件"))
                }
                ConfirmAction::Delete { keep_a, paths } => {
                    let (keep, remove) = if keep_a { (&a, &b) } else { (&b, &a) };
                    compare_engine::remove_exact_duplicates(keep, remove, mode, &paths, &cancel)
                        .map(|(removed, total)| {
                            format!("清理完成：删除 {removed}/{total} 个精确重复文件")
                        })
                }
                ConfirmAction::Copy { a_to_b, files } => {
                    let (source, target) = if a_to_b { (&a, &b) } else { (&b, &a) };
                    compare_engine::copy_selected_files(
                        source,
                        target,
                        &files,
                        &cancel,
                        |done, total| {
                            let _ = sender.send(CompareMessage::Progress(done, total));
                        },
                    )
                    .map(|(copied, total)| format!("复制完成：复制 {copied}/{total} 个文件"))
                }
            };
            let _ = sender.send(CompareMessage::ActionDone(result));
        });
    }
}

fn validate_roots(a: &Path, b: &Path) -> Result<(), String> {
    if !a.is_dir() || !b.is_dir() {
        return Err("A、B 均须为已存在的目录".to_string());
    }
    let a = a.canonicalize().map_err(|error| error.to_string())?;
    let b = b.canonicalize().map_err(|error| error.to_string())?;
    if a == b || a.starts_with(&b) || b.starts_with(&a) {
        return Err("A、B 目录不能相同，也不能互为父子目录".to_string());
    }
    Ok(())
}
