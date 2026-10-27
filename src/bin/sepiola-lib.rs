//! 媒体库命令行：扫描库目录、查看相册、浏览与重新打标签。
//!
//! ```bash
//! cargo run --bin sepiola-lib -- scan ~/Photos
//! cargo run --bin sepiola-lib -- classify-images ~/Photos -c config.toml
//! cargo run --bin sepiola-lib -- extract-books ~/Photos
//! cargo run --bin sepiola-lib -- albums ~/Photos
//! cargo run --bin sepiola-lib -- list ~/Photos --album 人像
//! cargo run --bin sepiola-lib -- retag ~/Photos sub/a.jpg 生日
//! ```

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use clap::{Parser, Subcommand};
use sepiola::config::Config;
use sepiola::error::Result;
use sepiola::library::{self, LibraryEvent};
use tracing::Level;

#[derive(Parser)]
#[command(name = "sepiola-lib", about = "媒体库：按类型归类、图片相册、浏览与重打标签")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// 扫描文件类型并更新库索引，不运行图片/书籍内容分类
    Scan { dir: String },
    /// 仅对未处理的图片运行人像检测与相册分类
    ClassifyImages {
        dir: String,
        #[arg(short, long)]
        config: Option<String>,
        #[arg(long)]
        force: bool,
    },
    /// 仅对书籍提取内容级元数据；--retry-failed 重试之前未成功的条目
    ExtractBooks {
        dir: String,
        #[arg(long)]
        retry_failed: bool,
    },
    /// 列出相册与条目数
    Albums { dir: String },
    /// 浏览库内容（默认全部；--album 只看某相册）
    List {
        dir: String,
        #[arg(long)]
        album: Option<String>,
    },
    /// 重新打标签：把图片移动到指定相册
    Retag {
        dir: String,
        /// 相对库根的文件路径
        rel: String,
        /// 目标相册名
        album: String,
    },
}

fn main() {
    tracing_subscriber::fmt().with_max_level(Level::WARN).init();
    let cli = Cli::parse();
    let root = PathBuf::from(&match cli.command {
        Command::Scan { ref dir }
        | Command::ClassifyImages { ref dir, .. }
        | Command::ExtractBooks { ref dir, .. }
        | Command::Albums { ref dir }
        | Command::List { ref dir, .. }
        | Command::Retag { ref dir, .. } => dir.clone(),
    });
    let result = match cli.command {
        Command::Scan { .. } => run_scan(&root),
        Command::ClassifyImages { config, force, .. } => {
            run_classify_images(&root, config.as_deref(), force)
        }
        Command::ExtractBooks { retry_failed, .. } => {
            run_extract_books(&root, retry_failed)
        }
        Command::Albums { .. } => run_albums(&root),
        Command::List { album, .. } => run_list(&root, album.as_deref()),
        Command::Retag { rel, album, .. } => library::retag(&root, Path::new(&rel), &album)
            .map(|_| println!("已移动 {rel} → {album}")),
    };
    if let Err(error) = result {
        eprintln!("sepiola-lib: {error}");
        std::process::exit(1);
    }
}

fn run_scan(root: &Path) -> Result<()> {
    let config = Config::default();
    let cancel = AtomicBool::new(false);
    let report = library::scan(root, &config, |event| {
        match event {
            LibraryEvent::Discovered { total } => {
                println!("发现 {total} 个待归类文件");
            }
            LibraryEvent::Processed {
                completed,
                total,
                rel_path,
            } => {
                print!("\x1b[2K\r  [{completed}/{total}] {}", rel_path.display());
            }
        }
    }, &cancel)?;
    println!();
    println!(
        "类型扫描完成：新增 {}，移除 {}；索引 {}",
        report.added,
        report.removed,
        root.join(".sepiola").join("library.json").display()
    );
    run_albums(root)
}

fn run_classify_images(root: &Path, config_path: Option<&str>, force: bool) -> Result<()> {
    let config = match config_path {
        Some(path) => Config::load_file(&PathBuf::from(path))?,
        None => Config::default(),
    };
    let cancel = AtomicBool::new(false);
    let report = library::classify_images(root, &config, force, print_progress, &cancel)?;
    println!("\n{}：{} 张{}", if force { "重新分类完成" } else { "图片分类完成" }, report.classified, if report.detector_missing {
        "（人脸模型不可用，未处理图片保持原标签）"
    } else {
        ""
    });
    run_albums(root)
}

fn run_extract_books(root: &Path, retry_failed: bool) -> Result<()> {
    let cancel = AtomicBool::new(false);
    let report = library::extract_book_metadata(root, retry_failed, print_progress, &cancel)?;
    println!("\n书籍元数据处理完成：{} 本", report.metadata_attempted);
    Ok(())
}

fn print_progress(event: LibraryEvent) {
    match event {
        LibraryEvent::Discovered { total } => println!("发现 {total} 个待处理条目"),
        LibraryEvent::Processed {
            completed,
            total,
            rel_path,
        } => print!("\x1b[2K\r  [{completed}/{total}] {}", rel_path.display()),
    }
}

fn run_albums(root: &Path) -> Result<()> {
    let index = library::LibraryIndex::load(root)?;
    let albums = index.albums();
    if albums.is_empty() {
        println!("相册为空（库中还没有图片）");
    } else {
        println!("相册：");
        for (name, count) in &albums {
            println!("  {name}：{count} 张");
        }
    }
    let kinds: std::collections::BTreeMap<_, _> = index
        .entries
        .iter()
        .fold(std::collections::BTreeMap::new(), |mut acc, entry| {
            *acc.entry(entry.kind.as_str()).or_insert(0usize) += 1;
            acc
        });
    println!(
        "共 {} 个条目：{}",
        index.entries.len(),
        kinds
            .iter()
            .map(|(kind, count)| format!("{kind} {count}"))
            .collect::<Vec<_>>()
            .join("，")
    );
    Ok(())
}

fn run_list(root: &Path, album: Option<&str>) -> Result<()> {
    let index = library::LibraryIndex::load(root)?;
    let entries: Vec<&library::LibraryEntry> = match album {
        Some(name) => index.images_in(name).into_iter().collect(),
        None => index.entries.iter().collect(),
    };
    if entries.is_empty() {
        println!("没有条目");
        return Ok(());
    }
    for entry in entries {
        let person = entry
            .person
            .as_deref()
            .map(|label| format!(" · {label}"))
            .unwrap_or_default();
        let album_label = if entry.album.is_empty() {
            String::new()
        } else {
            format!(" [{}{}]", entry.album, person)
        };
        println!(
            "{:<8} {}{}{}",
            entry.kind.as_str(),
            entry.rel_path.display(),
            album_label,
            if entry.manual { " (手动)" } else { "" }
        );
    }
    Ok(())
}
