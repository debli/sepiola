use std::path::{Path, PathBuf};

use clap::Parser;
use tracing::Level;

use crate::config::Config;
use crate::error::Result;
use crate::pipeline;

#[derive(Parser)]
#[command(name = "sepiola", about = "RAW photo batch auto color grading")]
pub struct Cli {
    /// Input directory containing RAW photos
    pub input_dir: String,

    /// Output directory for processed images
    #[arg(short = 'o', long)]
    pub output_dir: String,

    /// Path to config file; defaults to config.toml beside the executable, then ./config.toml
    #[arg(short, long)]
    pub config: Option<String>,
}

/// 未指定 `-c` 时的默认配置路径：优先可执行文件同目录，其次当前工作目录；
/// 两处都没有则返回 `None`，使用内置默认值。
fn default_config_path() -> Option<PathBuf> {
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf));
    find_config(exe_dir.as_deref(), Path::new(""))
}

/// 在给定目录中查找 `config.toml`，返回实际存在的路径。
fn find_config(exe_dir: Option<&Path>, cwd: &Path) -> Option<PathBuf> {
    if let Some(candidate) = exe_dir.map(|dir| dir.join("config.toml")) {
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    let local = cwd.join("config.toml");
    local.is_file().then_some(local)
}

pub fn run() -> Result<()> {
    tracing_subscriber::fmt().with_max_level(Level::INFO).init();

    let cli = Cli::parse();
    let config_path = match &cli.config {
        Some(path) => Some(PathBuf::from(path)),
        None => default_config_path(),
    };
    let config = match config_path {
        Some(path) => {
            tracing::info!(config = %path.display(), "loading config");
            Config::load_with_paths(&path, &cli.input_dir, &cli.output_dir)?
        }
        None => {
            let config = Config {
                input_dir: cli.input_dir,
                output_dir: cli.output_dir,
                ..Config::default()
            };
            config.validate()?;
            config
        }
    };

    let summary = pipeline::run_batch(&config)?;
    if summary.cancelled > 0 {
        println!(
            "done: {} processed, {} ok, {} failed, {} cancelled",
            summary.total, summary.succeeded, summary.failed, summary.cancelled
        );
    } else {
        println!(
            "done: {} processed, {} ok, {} failed",
            summary.total, summary.succeeded, summary.failed
        );
    }
    println!("log: {}", summary.csv_path.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir(tag: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!("sepiola-config-{tag}-{nanos}"));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    #[test]
    fn config_argument_is_optional() {
        let cli = Cli::try_parse_from(["sepiola", "in", "-o", "out"]).expect("parse");
        assert!(cli.config.is_none());

        let cli = Cli::try_parse_from(["sepiola", "in", "-o", "out", "-c", "my.toml"])
            .expect("parse");
        assert_eq!(cli.config.as_deref(), Some("my.toml"));
    }

    #[test]
    fn config_beside_executable_takes_priority() {
        let exe_dir = temp_dir("exe");
        let cwd = temp_dir("cwd");
        std::fs::write(exe_dir.join("config.toml"), "").expect("write exe config");
        std::fs::write(cwd.join("config.toml"), "").expect("write cwd config");

        assert_eq!(
            find_config(Some(&exe_dir), &cwd),
            Some(exe_dir.join("config.toml"))
        );

        let _ = std::fs::remove_dir_all(&exe_dir);
        let _ = std::fs::remove_dir_all(&cwd);
    }

    #[test]
    fn config_falls_back_to_working_directory() {
        let exe_dir = temp_dir("exe");
        let cwd = temp_dir("cwd");
        std::fs::write(cwd.join("config.toml"), "").expect("write cwd config");

        assert_eq!(
            find_config(Some(&exe_dir), &cwd),
            Some(cwd.join("config.toml"))
        );

        let _ = std::fs::remove_dir_all(&exe_dir);
        let _ = std::fs::remove_dir_all(&cwd);
    }

    #[test]
    fn config_absent_returns_none() {
        let exe_dir = temp_dir("exe");
        let cwd = temp_dir("cwd");

        assert_eq!(find_config(Some(&exe_dir), &cwd), None);
        assert_eq!(find_config(None, &cwd), None);

        let _ = std::fs::remove_dir_all(&exe_dir);
        let _ = std::fs::remove_dir_all(&cwd);
    }
}
