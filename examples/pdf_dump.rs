//! 临时调试：打印 pdf-extract 对指定 PDF 的文本层前若干字符。

fn main() {
    let path = std::env::args().nth(1).expect("usage: pdf_dump <pdf>");
    let bytes = std::fs::read(&path).expect("read");
    let text = pdf_extract::extract_text_from_mem(&bytes).expect("extract");
    let start: usize = std::env::args()
        .nth(2)
        .and_then(|arg| arg.parse().ok())
        .unwrap_or(0);
    let len: usize = std::env::args()
        .nth(3)
        .and_then(|arg| arg.parse().ok())
        .unwrap_or(600);
    let head: String = text.chars().skip(start).take(len).collect();
    println!("chars={} head={:?}", text.chars().count(), head);
}

