//! 人物识别评测入口：对 `<root>/<人物标签>/<图片>` 结构做相似度阈值扫描。
//!
//! ```bash
//! cargo run --bin sepiola-eval -- test_data/identity
//! cargo run --bin sepiola-eval -- test_data/identity -c config.toml --step 0.02
//! ```

use std::path::PathBuf;

use clap::Parser;
use sepiola::config::Config;
use sepiola::eval::{self, Metrics};
use sepiola::identity::FaceIdentifier;
use sepiola::scene::face::FaceClassifier;
use tracing::Level;

#[derive(Parser)]
#[command(name = "sepiola-eval", about = "Face identity threshold sweep on a labelled dataset")]
struct Cli {
    /// 评测集根目录；每个子目录名作为真实人物标签
    root: String,

    /// 配置文件（模型路径、当前阈值）；省略时使用内置默认值
    #[arg(short, long)]
    config: Option<String>,

    /// 扫描步长
    #[arg(long, default_value_t = 0.05)]
    step: f32,

    /// 扫描下界
    #[arg(long, default_value_t = 0.20)]
    min: f32,

    /// 扫描上界
    #[arg(long, default_value_t = 0.90)]
    max: f32,
}

fn main() {
    tracing_subscriber::fmt().with_max_level(Level::INFO).init();
    if let Err(error) = run() {
        eprintln!("sepiola-eval: {error}");
        std::process::exit(1);
    }
}

fn run() -> sepiola::error::Result<()> {
    let cli = Cli::parse();
    let config = match &cli.config {
        Some(path) => Config::load_file(&PathBuf::from(path))?,
        // 评测只用模型路径与阈值，不涉及输入/输出目录，故不做 validate。
        None => Config::default(),
    };

    let detector = FaceClassifier::new(&config.scene_settings)?;
    let identifier = FaceIdentifier::new(&config.identity)?;
    let root = PathBuf::from(&cli.root);
    let dataset = eval::load_dataset(&root, &detector, &identifier)?;

    println!(
        "评测集 {}：{} 张可用（{} 位人物），跳过 {} 张",
        root.display(),
        dataset.samples.len(),
        dataset.person_count(),
        dataset.skipped.len()
    );
    if dataset.multi_face > 0 {
        println!(
            "  其中 {} 张检出多张人脸，按最大人脸参与扫描（真实标签可能不精确）",
            dataset.multi_face
        );
    }
    for skipped in &dataset.skipped {
        println!("  跳过 {}：{}", skipped.path.display(), skipped.reason);
    }
    if dataset.samples.is_empty() {
        return Err(sepiola::error::Error::Config(
            "没有可用于评测的单人样本".to_string(),
        ));
    }

    let rows = eval::sweep(&dataset.samples, cli.min, cli.max, cli.step);
    println!();
    println!(
        "{:>6}  {:>4}  {:>6}  {:>4}  {:>8}  {:>6}  {:>6}",
        "阈值", "聚类", "误合并", "拆分", "精确率", "召回率", "F1"
    );
    for metrics in &rows {
        print_row(metrics, (metrics.threshold - config.identity.threshold).abs() < 1e-6);
    }

    if let Some(best) = eval::best_by_f1(&rows) {
        println!();
        println!(
            "最佳阈值 {:.2}：F1 {:.3}，精确率 {:.3}，召回率 {:.3}，聚类 {}（{} 位人物），误合并 {}，拆分 {}",
            best.threshold,
            best.f1,
            best.precision,
            best.recall,
            best.clusters,
            dataset.person_count(),
            best.merged_persons,
            best.split_persons
        );
        let current = rows
            .iter()
            .find(|metrics| (metrics.threshold - config.identity.threshold).abs() < 1e-6);
        match current {
            Some(metrics) => println!(
                "配置阈值 {:.2}（标记 *）：F1 {:.3}，精确率 {:.3}，召回率 {:.3}",
                config.identity.threshold, metrics.f1, metrics.precision, metrics.recall
            ),
            None => println!(
                "配置阈值 {:.2} 不在扫描区间内，未评估",
                config.identity.threshold
            ),
        }
    }
    Ok(())
}

fn print_row(metrics: &Metrics, current: bool) {
    println!(
        "{:>6.2}  {:>4}  {:>6}  {:>4}  {:>8.3}  {:>6.3}  {:>6.3}{}",
        metrics.threshold,
        metrics.clusters,
        metrics.merged_persons,
        metrics.split_persons,
        metrics.precision,
        metrics.recall,
        metrics.f1,
        if current { "  *" } else { "" }
    );
}
