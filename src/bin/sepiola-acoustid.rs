use std::path::PathBuf;

use chrono::{NaiveDate, Utc};
use clap::{Parser, Subcommand};
use sepiola::acoustid;

const DEFAULT_PROXY: &str = "http://localhost:7890";
const DEFAULT_INDEX_URL: &str = "http://127.0.0.1:8080/acoustid";
const UPDATE_STREAMS: &[&str] = &[
    "fingerprint",
    "meta",
    "track",
    "track_fingerprint",
    "track_mbid",
    "track_meta",
    "track_puid",
];

#[derive(Parser)]
#[command(
    name = "sepiola-acoustid",
    about = "AcoustID updates and local recording recognition"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Download public daily update streams.
    Download {
        #[arg(long, value_parser = parse_date)]
        from_date: NaiveDate,
        #[arg(long, value_parser = parse_date, default_value_t = Utc::now().date_naive())]
        to_date: NaiveDate,
        #[arg(
            long,
            value_delimiter = ',',
            default_value = "fingerprint,meta,track,track_fingerprint,track_mbid,track_meta,track_puid"
        )]
        streams: Vec<String>,
        #[arg(long, default_value = "data/acoustid")]
        output: PathBuf,
        #[arg(long, default_value = DEFAULT_PROXY)]
        proxy: String,
        #[arg(long)]
        no_proxy: bool,
        #[arg(long)]
        dry_run: bool,
    },
    /// Import downloaded metadata and fingerprints into SQLite and fpindex.
    Import {
        #[arg(long, default_value = "data/acoustid")]
        data: PathBuf,
        #[arg(long, default_value = "data/acoustid/lookup.sqlite")]
        database: PathBuf,
        #[arg(long, default_value = DEFAULT_INDEX_URL)]
        index_url: String,
    },
    /// Fingerprint an audio file and list local AcoustID recording candidates.
    Identify {
        audio: PathBuf,
        #[arg(long, default_value = "data/acoustid/lookup.sqlite")]
        database: PathBuf,
        #[arg(long, default_value = DEFAULT_INDEX_URL)]
        index_url: String,
        #[arg(long, default_value_t = 50)]
        limit: usize,
        #[arg(long, default_value_t = 1)]
        min_score: u32,
    },
}

fn parse_date(value: &str) -> Result<NaiveDate, String> {
    NaiveDate::parse_from_str(value, "%Y-%m-%d").map_err(|_| "use YYYY-MM-DD".to_owned())
}

fn run() -> Result<(), String> {
    match Cli::parse().command {
        Command::Download {
            from_date,
            to_date,
            streams,
            output,
            proxy,
            no_proxy,
            dry_run,
        } => {
            let proxy = (!no_proxy).then_some(proxy.as_str());
            if dry_run {
                let files = acoustid::list_updates(from_date, to_date, &streams, proxy)?;
                let total_size: u64 = files.iter().map(|(_, size)| size).sum();
                println!(
                    "{} files, {:.2} GiB compressed",
                    files.len(),
                    total_size as f64 / 1024_f64.powi(3)
                );
                for (path, size) in files {
                    println!("{size:>12}  {path}");
                }
            } else {
                let (downloaded, skipped) =
                    acoustid::download_updates(from_date, to_date, &streams, &output, proxy)?;
                println!("done: {downloaded} downloaded, {skipped} already present");
            }
        }
        Command::Import {
            data,
            database,
            index_url,
        } => {
            let (metadata, fingerprints) = acoustid::import_updates(&data, &database, &index_url)?;
            println!("metadata records imported: {metadata}");
            println!("fingerprints submitted to fpindex: {fingerprints}");
        }
        Command::Identify {
            audio,
            database,
            index_url,
            limit,
            min_score,
        } => {
            if limit == 0 || min_score == 0 {
                return Err("--limit and --min-score must be positive".to_owned());
            }
            let result = acoustid::identify_file(&audio, &database, &index_url, limit, min_score)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&result).map_err(|error| error.to_string())?
            );
        }
    }
    Ok(())
}

fn main() {
    if let Err(error) = run() {
        eprintln!("sepiola-acoustid: {error}");
        std::process::exit(1);
    }
}
