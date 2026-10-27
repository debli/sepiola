//! 书籍正文搜索：在后台读取文件，避免阻塞 GUI 帧。

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, TryRecvError};
use std::sync::Arc;

use sepiola::library::LibraryEntry;

use crate::book_filters::{build_book_facets, BookField};

enum SearchEvent {
    Progress(usize),
    Done(HashSet<PathBuf>),
}

#[derive(Default)]
pub struct BookContentSearch {
    query: String,
    matches: HashSet<PathBuf>,
    progress: (usize, usize),
    searching: bool,
    cancel: Option<Arc<AtomicBool>>,
    receiver: Option<Receiver<SearchEvent>>,
}

#[derive(Default)]
pub struct BookSearchState {
    pub query: String,
    pub filters: std::collections::HashMap<BookField, String>,
    pub facets: std::collections::HashMap<BookField, Vec<(String, usize)>>,
    pub search_content: bool,
    pub content: BookContentSearch,
}

impl BookSearchState {
    pub fn reset(&mut self) {
        self.query.clear();
        self.filters.clear();
        self.search_content = false;
        self.content.reset();
    }

    pub fn refresh_facets(&mut self, entries: &[LibraryEntry]) {
        self.facets = build_book_facets(entries);
    }
}

impl BookContentSearch {
    pub fn start(&mut self, root: PathBuf, books: Vec<PathBuf>, query: &str) {
        self.reset();
        let query = query.trim().to_lowercase();
        if query.is_empty() {
            return;
        }

        let total = books.len();
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = Arc::clone(&cancel);
        let (sender, receiver) = channel();
        self.query = query.clone();
        self.progress = (0, total);
        self.searching = true;
        self.cancel = Some(cancel);
        self.receiver = Some(receiver);

        std::thread::spawn(move || {
            let mut matches = HashSet::new();
            for (index, rel) in books.iter().enumerate() {
                if worker_cancel.load(Ordering::Relaxed) {
                    return;
                }
                if sepiola::book::contains_text(&root.join(rel), &query) {
                    matches.insert(rel.clone());
                }
                let completed = index + 1;
                if completed % 16 == 0 || completed == total {
                    let _ = sender.send(SearchEvent::Progress(completed));
                }
            }
            let _ = sender.send(SearchEvent::Done(matches));
        });
    }

    pub fn reset(&mut self) {
        if let Some(cancel) = &self.cancel {
            cancel.store(true, Ordering::Relaxed);
        }
        self.query.clear();
        self.matches.clear();
        self.progress = (0, 0);
        self.searching = false;
        self.cancel = None;
        self.receiver = None;
    }

    pub fn poll(&mut self) {
        let mut finished = false;
        if let Some(receiver) = &self.receiver {
            loop {
                match receiver.try_recv() {
                    Ok(SearchEvent::Progress(completed)) => {
                        self.progress.0 = completed;
                    }
                    Ok(SearchEvent::Done(matches)) => {
                        self.matches = matches;
                        self.searching = false;
                        finished = true;
                        break;
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        self.searching = false;
                        finished = true;
                        break;
                    }
                }
            }
        }
        if finished {
            self.receiver = None;
            self.cancel = None;
        }
    }

    pub fn is_searching(&self) -> bool {
        self.searching
    }

    pub fn progress(&self) -> (usize, usize) {
        self.progress
    }

    pub fn matches_for(&self, query: &str) -> Option<&HashSet<PathBuf>> {
        let query = query.trim().to_lowercase();
        (!self.searching && !query.is_empty() && self.query == query).then_some(&self.matches)
    }
}
