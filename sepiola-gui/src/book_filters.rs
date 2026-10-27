//! 书籍字段分面、元信息搜索与筛选组合逻辑。

use sepiola::library::{FileKind, LibraryEntry};
use std::collections::HashMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BookField {
    Format,
    Title,
    Author,
    Publisher,
    Year,
    Translator,
    Edition,
    Isbn,
}

const BOOK_METADATA_FIELDS: [BookField; 8] = [
    BookField::Format,
    BookField::Title,
    BookField::Author,
    BookField::Publisher,
    BookField::Year,
    BookField::Translator,
    BookField::Edition,
    BookField::Isbn,
];

pub const BOOK_FACET_FIELDS: [BookField; 7] = [
    BookField::Format,
    BookField::Author,
    BookField::Publisher,
    BookField::Year,
    BookField::Translator,
    BookField::Edition,
    BookField::Isbn,
];

pub fn field_label(field: BookField) -> &'static str {
    match field {
        BookField::Format => "格式",
        BookField::Title => "书名",
        BookField::Author => "作者",
        BookField::Publisher => "出版社",
        BookField::Year => "年份",
        BookField::Translator => "译者",
        BookField::Edition => "版本",
        BookField::Isbn => "ISBN",
    }
}

pub fn field_value(entry: &LibraryEntry, field: BookField) -> Option<String> {
    let meta = entry.book_meta.as_ref();
    match field {
        BookField::Format => entry
            .rel_path
            .extension()
            .map(|ext| ext.to_string_lossy().to_ascii_lowercase()),
        BookField::Title => Some(meta.and_then(|meta| meta.title.clone()).unwrap_or_else(|| {
            entry
                .rel_path
                .file_stem()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default()
        })),
        BookField::Author => meta.and_then(|meta| meta.author.clone()),
        BookField::Publisher => meta.and_then(|meta| meta.publisher.clone()),
        BookField::Year => meta.and_then(|meta| meta.year.map(|year| year.to_string())),
        BookField::Translator => meta.and_then(|meta| meta.translator.clone()),
        BookField::Edition => meta.and_then(|meta| meta.edition.clone()),
        BookField::Isbn => meta.and_then(|meta| meta.isbn.clone()),
    }
    .map(|value| value.trim().to_string())
    .filter(|value| !value.is_empty())
}

fn facet_options(entries: &[LibraryEntry], field: BookField) -> Vec<(String, usize)> {
    let mut counts = HashMap::new();
    for entry in entries.iter().filter(|entry| entry.kind == FileKind::Book) {
        if let Some(value) = field_value(entry, field) {
            *counts.entry(value).or_insert(0usize) += 1;
        }
    }
    let mut options: Vec<_> = counts.into_iter().collect();
    options.sort_by(|(left_value, left_count), (right_value, right_count)| {
        right_count
            .cmp(left_count)
            .then_with(|| left_value.cmp(right_value))
    });
    options
}

pub fn build_book_facets(entries: &[LibraryEntry]) -> HashMap<BookField, Vec<(String, usize)>> {
    BOOK_FACET_FIELDS
        .iter()
        .map(|field| (*field, facet_options(entries, *field)))
        .collect()
}

fn metadata_matches(entry: &LibraryEntry, query: &str) -> bool {
    let query = query.to_lowercase();
    BOOK_METADATA_FIELDS.iter().any(|field| {
        field_value(entry, *field).is_some_and(|value| value.to_lowercase().contains(&query))
    }) || entry
        .rel_path
        .to_string_lossy()
        .to_lowercase()
        .contains(&query)
}

pub fn matches_book(
    entry: &LibraryEntry,
    filters: &HashMap<BookField, String>,
    query: &str,
    search_content: bool,
    content_match: bool,
) -> bool {
    filters.iter().all(|(field, value)| {
        value.is_empty() || field_value(entry, *field).as_deref() == Some(value)
    }) && (query.trim().is_empty()
        || metadata_matches(entry, query)
        || (search_content && content_match))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use sepiola::book::BookMeta;

    use super::*;

    fn entry(path: &str, title: &str, author: &str) -> LibraryEntry {
        LibraryEntry {
            rel_path: PathBuf::from(path),
            kind: FileKind::Book,
            album: String::new(),
            person: None,
            manual: false,
            book_meta: Some(BookMeta {
                title: Some(title.to_string()),
                author: Some(author.to_string()),
                ..BookMeta::default()
            }),
            image_classification_version: 0,
            book_metadata_attempted: true,
        }
    }

    #[test]
    fn facets_are_unique_and_sorted_by_frequency() {
        let entries = vec![
            entry("one.epub", "One", "Author A"),
            entry("two.epub", "Two", "Author A"),
            entry("three.pdf", "Three", "Author B"),
        ];
        assert_eq!(
            facet_options(&entries, BookField::Format),
            vec![("epub".to_string(), 2), ("pdf".to_string(), 1)]
        );
        assert_eq!(
            facet_options(&entries, BookField::Author),
            vec![("Author A".to_string(), 2), ("Author B".to_string(), 1)]
        );
        assert!(!BOOK_FACET_FIELDS.contains(&BookField::Title));
    }

    #[test]
    fn metadata_search_matches_any_field_and_filename() {
        let book = entry("folder/book.epub", "The Title", "Some Author");
        assert!(metadata_matches(&book, "the title"));
        assert!(metadata_matches(&book, "some author"));
        assert!(metadata_matches(&book, "folder/book"));
        assert!(!metadata_matches(&book, "not present"));
    }

    #[test]
    fn combined_filters_intersect_search_results_union() {
        let book = entry("folder/book.epub", "The Title", "Some Author");
        let mut filters = HashMap::new();
        filters.insert(BookField::Format, "epub".to_string());
        filters.insert(BookField::Author, "Some Author".to_string());
        assert!(matches_book(&book, &filters, "hidden phrase", true, true));
        assert!(!matches_book(&book, &filters, "hidden phrase", false, true));

        filters.insert(BookField::Author, "Other Author".to_string());
        assert!(!matches_book(&book, &filters, "some author", true, true));
    }
}
