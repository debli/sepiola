//! 临时调试：走 app 的完整解析路径（pdf_text::extract，含 panic 捕获）打印结果。

fn main() {
    let arg = std::env::args().nth(1).expect("usage: pdf_parse_dump <pdf>");
    let path = std::path::Path::new(&arg);
    match sepiola::reader::BookDocument::open(path) {
        Ok(doc) => {
            println!("chapters={}", doc.chapters.len());
            if let Some(first) = doc.chapters.first() {
                let head: String = first.text.chars().take(300).collect();
                println!("first={:?} title={:?}", head, first.title);
            }
        }
        Err(error) => println!("ERR: {error}"),
    }
}
