use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use chromaprint::{Algorithm, Fingerprinter};
use chrono::{Datelike, NaiveDate, Utc};
use flate2::read::GzDecoder;
use reqwest::blocking::{Client, Response};
use reqwest::header::RANGE;
use reqwest::Proxy;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::DecoderOptions;
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

const BASE_URL: &str = "https://data.acoustid.org";
const DEFAULT_PROXY: &str = "http://localhost:7890";
const DEFAULT_INDEX_URL: &str = "http://127.0.0.1:8080/acoustid";
const RETRIES: usize = 5;
const INDEX_BATCH_SIZE: usize = 500;
const QUERY_SIZE: usize = 120;
const QUERY_START: usize = 80;
const QUERY_BIT_MASK: u32 = 0xffff_fff0;
const SILENCE_HASH: u32 = 627_964_279;

#[derive(Clone, Debug, Deserialize)]
struct ListingEntry {
    name: String,
    size: Option<u64>,
}

#[derive(Clone, Debug)]
struct UpdateFile {
    relative_url: String,
    size: u64,
}

#[derive(Debug, Deserialize)]
struct FingerprintUpdate {
    id: u64,
    fingerprint: Vec<i64>,
}

#[derive(Debug, Deserialize)]
struct IndexSearchResult {
    id: u32,
    score: u32,
}

#[derive(Debug, Deserialize)]
struct IndexSearchResponse {
    #[serde(default)]
    results: Vec<IndexSearchResult>,
}

#[derive(Debug, Serialize)]
pub struct Candidate {
    fingerprint_id: u32,
    track_id: i64,
    acoustid_track_id: Option<String>,
    musicbrainz_ids: Vec<String>,
    title: Option<String>,
    artist: Option<String>,
    album: Option<String>,
    album_artist: Option<String>,
    track_number: Option<i64>,
    year: Option<i64>,
    submission_count: i64,
    hash_score: u32,
    score_ratio: f64,
}

#[derive(Debug, Serialize)]
pub struct Identification {
    pub duration_seconds: f64,
    pub query_hashes: usize,
    pub match_kind: &'static str,
    pub candidates: Vec<Candidate>,
}

fn build_client(proxy: Option<&str>) -> Result<Client, String> {
    let mut builder = Client::builder().timeout(Duration::from_secs(120));
    if let Some(proxy) = proxy {
        builder = builder.proxy(Proxy::all(proxy).map_err(|error| error.to_string())?);
    }
    builder.build().map_err(|error| error.to_string())
}

fn retry<T, F>(description: &str, mut operation: F) -> Result<T, String>
where
    F: FnMut() -> Result<T, String>,
{
    let mut last_error = String::new();
    for attempt in 0..RETRIES {
        match operation() {
            Ok(value) => return Ok(value),
            Err(error) => {
                last_error = error;
                if attempt + 1 < RETRIES {
                    thread::sleep(Duration::from_secs(1_u64 << attempt));
                }
            }
        }
    }
    Err(format!(
        "{description} failed after {RETRIES} attempts: {last_error}"
    ))
}

fn get_json<T: for<'de> Deserialize<'de>>(client: &Client, url: &str) -> Result<T, String> {
    retry(url, || {
        client
            .get(url)
            .header("User-Agent", "Sepiola-AcoustID/1.0")
            .send()
            .and_then(Response::error_for_status)
            .and_then(Response::json::<T>)
            .map_err(|error| error.to_string())
    })
}

fn get_listing(client: &Client, url: &str) -> Result<Vec<ListingEntry>, String> {
    get_json(client, url)
}

pub fn download_updates(
    from: NaiveDate,
    to: NaiveDate,
    streams: &[String],
    output: &Path,
    proxy: Option<&str>,
) -> Result<(usize, usize), String> {
    if from > to {
        return Err("start date must not be after end date".to_owned());
    }
    let client = build_client(proxy)?;
    let files = discover_updates(&client, from, to, streams)?;
    let mut downloaded = 0;
    let mut skipped = 0;
    for update in files {
        if download_one(&client, &update, output)? {
            downloaded += 1;
            println!("downloaded {}", update.relative_url);
            thread::sleep(Duration::from_millis(200));
        } else {
            skipped += 1;
        }
    }
    Ok((downloaded, skipped))
}

pub fn list_updates(
    from: NaiveDate,
    to: NaiveDate,
    streams: &[String],
    proxy: Option<&str>,
) -> Result<Vec<(String, u64)>, String> {
    if from > to {
        return Err("start date must not be after end date".to_owned());
    }
    let client = build_client(proxy)?;
    Ok(discover_updates(&client, from, to, streams)?
        .into_iter()
        .map(|file| (file.relative_url, file.size))
        .collect())
}

fn discover_updates(
    client: &Client,
    from: NaiveDate,
    to: NaiveDate,
    streams: &[String],
) -> Result<Vec<UpdateFile>, String> {
    let years = get_listing(client, &format!("{BASE_URL}/index.json"))?;
    let mut files = Vec::new();
    for year_entry in years {
        let Some(year_text) = year_entry.name.strip_suffix('/') else {
            continue;
        };
        let Ok(year) = year_text.parse::<i32>() else {
            continue;
        };
        if year < from.year() || year > to.year() {
            continue;
        }
        let year_prefix = format!("{year_text}/");
        for month_entry in get_listing(client, &format!("{BASE_URL}/{year_prefix}index.json"))? {
            let Some(month_text) = month_entry.name.strip_suffix('/') else {
                continue;
            };
            let Ok(month_start) =
                NaiveDate::parse_from_str(&format!("{month_text}-01"), "%Y-%m-%d")
            else {
                continue;
            };
            if month_start.year() < from.year()
                || (month_start.year() == from.year() && month_start.month() < from.month())
                || month_start.year() > to.year()
                || (month_start.year() == to.year() && month_start.month() > to.month())
            {
                continue;
            }
            let month_prefix = format!("{year_prefix}{month_text}/");
            for entry in get_listing(client, &format!("{BASE_URL}/{month_prefix}index.json"))? {
                let Some(size) = entry.size else { continue };
                let Some((date, stream)) = parse_update_name(&entry.name) else {
                    continue;
                };
                if date >= from && date <= to && streams.iter().any(|selected| selected == stream) {
                    files.push(UpdateFile {
                        relative_url: format!("{month_prefix}{}", entry.name),
                        size,
                    });
                }
            }
        }
    }
    files.sort_by(|left, right| left.relative_url.cmp(&right.relative_url));
    Ok(files)
}

fn parse_update_name(name: &str) -> Option<(NaiveDate, &str)> {
    let stem = name.strip_suffix("-update.jsonl.gz")?;
    if stem.len() < 11 {
        return None;
    }
    let (date_text, stream_suffix) = stem.split_at(10);
    let stream = stream_suffix.strip_prefix('-')?;
    if !matches!(
        stream,
        "fingerprint"
            | "meta"
            | "track"
            | "track_fingerprint"
            | "track_mbid"
            | "track_meta"
            | "track_puid"
    ) {
        return None;
    }
    Some((
        NaiveDate::parse_from_str(date_text, "%Y-%m-%d").ok()?,
        stream,
    ))
}

fn download_one(client: &Client, update: &UpdateFile, output: &Path) -> Result<bool, String> {
    let destination = output.join(&update.relative_url);
    if destination.exists()
        && destination
            .metadata()
            .map_err(|error| error.to_string())?
            .len()
            == update.size
    {
        return Ok(false);
    }
    let parent = destination
        .parent()
        .ok_or_else(|| format!("invalid update path: {}", destination.display()))?;
    fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    let partial = destination.with_extension("jsonl.gz.part");

    retry(&update.relative_url, || {
        let offset = fs::metadata(&partial)
            .map(|metadata| metadata.len())
            .unwrap_or(0);
        if offset >= update.size {
            fs::remove_file(&partial).map_err(|error| error.to_string())?;
        }
        let offset = fs::metadata(&partial)
            .map(|metadata| metadata.len())
            .unwrap_or(0);
        let url = format!("{BASE_URL}/{}", update.relative_url);
        let mut request = client
            .get(&url)
            .header("User-Agent", "Sepiola-AcoustID/1.0");
        if offset > 0 {
            request = request.header(RANGE, format!("bytes={offset}-"));
        }
        let mut response = request
            .send()
            .and_then(Response::error_for_status)
            .map_err(|error| error.to_string())?;
        let append = offset > 0 && response.status() == reqwest::StatusCode::PARTIAL_CONTENT;
        let mut file = OpenOptions::new()
            .create(true)
            .write(true)
            .append(append)
            .truncate(!append)
            .open(&partial)
            .map_err(|error| error.to_string())?;
        std::io::copy(&mut response, &mut file).map_err(|error| error.to_string())?;
        file.flush().map_err(|error| error.to_string())?;
        let actual_size = file.metadata().map_err(|error| error.to_string())?.len();
        if actual_size != update.size {
            return Err(format!(
                "download size mismatch: {actual_size} != {}",
                update.size
            ));
        }
        fs::rename(&partial, &destination).map_err(|error| error.to_string())?;
        Ok(())
    })?;
    Ok(true)
}

pub fn extract_query(hashes: &[u32]) -> Vec<u32> {
    if hashes.is_empty() {
        return Vec::new();
    }
    let clean_count = hashes
        .iter()
        .filter(|&&value| value != SILENCE_HASH)
        .count();
    if clean_count == 0 {
        return Vec::new();
    }
    let start = QUERY_START.min(clean_count.saturating_sub(QUERY_SIZE));
    let mut seen = std::collections::HashSet::new();
    let mut query = Vec::with_capacity(QUERY_SIZE);
    for &value in hashes.iter().skip(start) {
        if value == SILENCE_HASH {
            continue;
        }
        let masked = value & QUERY_BIT_MASK;
        if seen.insert(masked) {
            query.push(masked);
            if query.len() == QUERY_SIZE {
                break;
            }
        }
    }
    query
}

pub fn fingerprint_file(path: &Path) -> Result<(f64, Vec<u32>), String> {
    let file = File::open(path).map_err(|error| format!("{}: {error}", path.display()))?;
    let media = MediaSourceStream::new(Box::new(file), Default::default());
    let hint = Hint::new();
    let probed = symphonia::default::get_probe()
        .format(
            &hint,
            media,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .map_err(|error| format!("audio probe failed: {error}"))?;
    let mut format = probed.format;
    let track = format
        .default_track()
        .ok_or_else(|| "audio file contains no default track".to_owned())?;
    let track_id = track.id;
    let sample_rate = track
        .codec_params
        .sample_rate
        .ok_or_else(|| "audio track has no sample rate".to_owned())?;
    let channels = track
        .codec_params
        .channels
        .ok_or_else(|| "audio track has no channel count".to_owned())?
        .count();
    let channels = u16::try_from(channels).map_err(|_| "unsupported channel count".to_owned())?;
    let duration = track
        .codec_params
        .n_frames
        .map(|frames| frames as f64 / f64::from(sample_rate))
        .unwrap_or(0.0);
    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .map_err(|error| format!("audio decoder creation failed: {error}"))?;
    let mut fingerprinter = Fingerprinter::new(Algorithm::default());
    fingerprinter
        .start(sample_rate, channels)
        .map_err(|error| format!("fingerprint initialization failed: {error}"))?;
    let mut sample_buffer = None;

    loop {
        let packet = match format.next_packet() {
            Ok(packet) => packet,
            Err(SymphoniaError::IoError(error))
                if error.kind() == std::io::ErrorKind::UnexpectedEof =>
            {
                break
            }
            Err(SymphoniaError::ResetRequired) => {
                return Err("audio stream requires a decoder reset".to_owned())
            }
            Err(error) => return Err(format!("audio packet read failed: {error}")),
        };
        if packet.track_id() != track_id {
            continue;
        }
        let decoded = match decoder.decode(&packet) {
            Ok(decoded) => decoded,
            Err(SymphoniaError::DecodeError(_)) => continue,
            Err(error) => return Err(format!("audio decode failed: {error}")),
        };
        if sample_buffer.is_none() {
            sample_buffer = Some(SampleBuffer::<i16>::new(
                decoded.capacity() as u64,
                *decoded.spec(),
            ));
        }
        if let Some(buffer) = &mut sample_buffer {
            buffer.copy_interleaved_ref(decoded);
            fingerprinter
                .feed(buffer.samples())
                .map_err(|error| format!("fingerprint update failed: {error}"))?;
        }
    }
    fingerprinter
        .finish()
        .map_err(|error| format!("fingerprint finalization failed: {error}"))?;
    let hashes = fingerprinter.fingerprint().to_vec();
    if hashes.is_empty() {
        return Err("audio is too short to produce a fingerprint".to_owned());
    }
    Ok((duration, hashes))
}

fn open_database(path: &Path) -> Result<Connection, String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let connection = Connection::open(path).map_err(|error| error.to_string())?;
    connection
        .execute_batch(
            "PRAGMA journal_mode=WAL;
             CREATE TABLE IF NOT EXISTS meta (
                 id INTEGER PRIMARY KEY, track TEXT, artist TEXT, album TEXT,
                 album_artist TEXT, track_no INTEGER, year INTEGER
             );
             CREATE TABLE IF NOT EXISTS track (id INTEGER PRIMARY KEY, gid TEXT);
             CREATE TABLE IF NOT EXISTS track_meta (
                 track_id INTEGER NOT NULL, meta_id INTEGER NOT NULL,
                 submission_count INTEGER NOT NULL DEFAULT 0,
                 PRIMARY KEY(track_id, meta_id)
             );
             CREATE TABLE IF NOT EXISTS track_mbid (
                 track_id INTEGER NOT NULL, mbid TEXT NOT NULL,
                 submission_count INTEGER NOT NULL DEFAULT 0,
                 PRIMARY KEY(track_id, mbid)
             );
             CREATE TABLE IF NOT EXISTS track_fingerprint (
                 track_id INTEGER NOT NULL, fingerprint_id INTEGER NOT NULL,
                 submission_count INTEGER NOT NULL DEFAULT 0,
                 PRIMARY KEY(track_id, fingerprint_id)
             );
             CREATE INDEX IF NOT EXISTS track_fingerprint_by_id ON track_fingerprint(fingerprint_id);
             CREATE INDEX IF NOT EXISTS track_meta_by_track ON track_meta(track_id);
             CREATE INDEX IF NOT EXISTS track_mbid_by_track ON track_mbid(track_id);",
        )
        .map_err(|error| error.to_string())?;
    Ok(connection)
}

fn update_files(data_dir: &Path, stream: &str) -> Result<Vec<PathBuf>, String> {
    let mut files = Vec::new();
    let mut pending = vec![data_dir.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory).map_err(|error| error.to_string())? {
            let entry = entry.map_err(|error| error.to_string())?;
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else if path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.ends_with(&format!("-{stream}-update.jsonl.gz")))
            {
                files.push(path);
            }
        }
    }
    files.sort();
    Ok(files)
}

fn import_jsonl<F>(path: &Path, mut insert: F) -> Result<usize, String>
where
    F: FnMut(Value) -> Result<(), String>,
{
    let file = File::open(path).map_err(|error| error.to_string())?;
    let decoder = GzDecoder::new(file);
    let mut count = 0;
    for (line_index, line) in BufReader::new(decoder).lines().enumerate() {
        let line = line.map_err(|error| error.to_string())?;
        if line.trim().is_empty() {
            continue;
        }
        let record: Value = serde_json::from_str(&line)
            .map_err(|error| format!("{}:{}: {error}", path.display(), line_index + 1))?;
        insert(record)?;
        count += 1;
    }
    Ok(count)
}

pub fn import_metadata(data_dir: &Path, database: &Path) -> Result<usize, String> {
    let mut connection = open_database(database)?;
    let mut imported = 0;
    for stream in [
        "meta",
        "track",
        "track_meta",
        "track_mbid",
        "track_fingerprint",
    ] {
        for path in update_files(data_dir, stream)? {
            let transaction = connection
                .transaction()
                .map_err(|error| error.to_string())?;
            let count = import_jsonl(&path, |item| {
                match stream {
                    "meta" => transaction.execute(
                        "INSERT OR REPLACE INTO meta VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                        params![
                            item["id"].as_i64(),
                            item["track"].as_str(),
                            item["artist"].as_str(),
                            item["album"].as_str(),
                            item["album_artist"].as_str(),
                            item["track_no"].as_i64(),
                            item["year"].as_i64()
                        ],
                    ),
                    "track" => transaction.execute(
                        "INSERT OR REPLACE INTO track VALUES (?1, ?2)",
                        params![item["id"].as_i64(), item["gid"].as_str()],
                    ),
                    "track_meta" => transaction.execute(
                        "INSERT OR REPLACE INTO track_meta VALUES (?1, ?2, ?3)",
                        params![
                            item["track_id"].as_i64(),
                            item["meta_id"].as_i64(),
                            item["submission_count"].as_i64().unwrap_or(0)
                        ],
                    ),
                    "track_mbid" => transaction.execute(
                        "INSERT OR REPLACE INTO track_mbid VALUES (?1, ?2, ?3)",
                        params![
                            item["track_id"].as_i64(),
                            item["mbid"].as_str(),
                            item["submission_count"].as_i64().unwrap_or(0)
                        ],
                    ),
                    _ => transaction.execute(
                        "INSERT OR REPLACE INTO track_fingerprint VALUES (?1, ?2, ?3)",
                        params![
                            item["track_id"].as_i64(),
                            item["fingerprint_id"].as_i64(),
                            item["submission_count"].as_i64().unwrap_or(0)
                        ],
                    ),
                }
                .map(|_| ())
                .map_err(|error| error.to_string())
            })?;
            transaction.commit().map_err(|error| error.to_string())?;
            imported += count;
        }
    }
    Ok(imported)
}

fn update_index(client: &Client, index_url: &str, data_dir: &Path) -> Result<usize, String> {
    let index_url = index_url.trim_end_matches('/');
    retry("create fpindex", || {
        client
            .put(index_url)
            .json(&json!({}))
            .send()
            .and_then(Response::error_for_status)
            .map(|_| ())
            .map_err(|error| error.to_string())
    })?;
    let endpoint = format!("{index_url}/_update");
    let mut batch = Vec::with_capacity(INDEX_BATCH_SIZE);
    let mut indexed = 0;
    for path in update_files(data_dir, "fingerprint")? {
        let file = File::open(&path).map_err(|error| error.to_string())?;
        let decoder = GzDecoder::new(file);
        for (line_number, line) in BufReader::new(decoder).lines().enumerate() {
            let line = line.map_err(|error| error.to_string())?;
            if line.trim().is_empty() {
                continue;
            }
            let update: FingerprintUpdate = serde_json::from_str(&line)
                .map_err(|error| format!("{}:{}: {error}", path.display(), line_number + 1))?;
            let id = u32::try_from(update.id)
                .map_err(|_| format!("fingerprint id {} exceeds u32", update.id))?;
            let raw: Vec<u32> = update
                .fingerprint
                .into_iter()
                .map(|value| value as u32)
                .collect();
            let hashes = extract_query(&raw);
            if hashes.is_empty() {
                continue;
            }
            batch.push(json!({"insert":{"id":id,"hashes":hashes}}));
            indexed += 1;
            if batch.len() >= INDEX_BATCH_SIZE {
                post_batch(client, &endpoint, &mut batch)?;
            }
        }
    }
    post_batch(client, &endpoint, &mut batch)?;
    Ok(indexed)
}

fn post_batch(client: &Client, endpoint: &str, batch: &mut Vec<Value>) -> Result<(), String> {
    if batch.is_empty() {
        return Ok(());
    }
    let changes = std::mem::take(batch);
    retry("fpindex update", || {
        client
            .post(endpoint)
            .json(&json!({"changes":changes}))
            .send()
            .and_then(Response::error_for_status)
            .map(|_| ())
            .map_err(|error| error.to_string())
    })
}

pub fn import_updates(
    data_dir: &Path,
    database: &Path,
    index_url: &str,
) -> Result<(usize, usize), String> {
    let metadata = import_metadata(data_dir, database)?;
    let client = build_client(None)?;
    let indexed = update_index(&client, index_url, data_dir)?;
    Ok((metadata, indexed))
}

pub fn identify_file(
    audio_path: &Path,
    database: &Path,
    index_url: &str,
    limit: usize,
    min_score: u32,
) -> Result<Identification, String> {
    let (duration, raw) = fingerprint_file(audio_path)?;
    let query = extract_query(&raw);
    if query.is_empty() {
        return Ok(Identification {
            duration_seconds: duration,
            query_hashes: 0,
            match_kind: "recording fingerprint candidate; not cross-performance work recognition",
            candidates: Vec::new(),
        });
    }
    let client = build_client(None)?;
    let response: IndexSearchResponse = retry("fpindex search", || {
        client
            .post(format!("{}/_search", index_url.trim_end_matches('/')))
            .json(&json!({"query":query,"limit":limit,"min_score":min_score}))
            .send()
            .and_then(Response::error_for_status)
            .and_then(Response::json::<IndexSearchResponse>)
            .map_err(|error| error.to_string())
    })?;
    let connection = open_database(database)?;
    let mut candidates_by_track = std::collections::HashMap::new();
    for result in response.results {
        let mut statement = connection
            .prepare(
                "SELECT tf.track_id, tf.submission_count, t.gid,
                        m.track, m.artist, m.album, m.album_artist, m.track_no, m.year
                 FROM track_fingerprint tf
                 LEFT JOIN track t ON t.id = tf.track_id
                 LEFT JOIN track_meta tm ON tm.track_id = tf.track_id
                 LEFT JOIN meta m ON m.id = tm.meta_id
                 WHERE tf.fingerprint_id = ?1
                 ORDER BY tf.submission_count DESC, tm.submission_count DESC",
            )
            .map_err(|error| error.to_string())?;
        let rows = statement
            .query_map([i64::from(result.id)], |row| {
                let track_id: i64 = row.get(0)?;
                let mut mbid_statement = connection.prepare(
                    "SELECT mbid FROM track_mbid WHERE track_id = ?1 ORDER BY submission_count DESC",
                )?;
                let musicbrainz_ids = mbid_statement
                    .query_map([track_id], |mbid_row| mbid_row.get(0))?
                    .collect::<rusqlite::Result<Vec<String>>>()?;
                Ok(Candidate {
                    fingerprint_id: result.id,
                    track_id,
                    acoustid_track_id: row.get(2)?,
                    musicbrainz_ids,
                    title: row.get(3)?,
                    artist: row.get(4)?,
                    album: row.get(5)?,
                    album_artist: row.get(6)?,
                    track_number: row.get(7)?,
                    year: row.get(8)?,
                    submission_count: row.get(1)?,
                    hash_score: result.score,
                    score_ratio: f64::from(result.score) / query.len() as f64,
                })
            })
            .map_err(|error| error.to_string())?;
        for candidate in rows
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|error| error.to_string())?
        {
            match candidates_by_track.entry(candidate.track_id) {
                std::collections::hash_map::Entry::Vacant(entry) => {
                    entry.insert(candidate);
                }
                std::collections::hash_map::Entry::Occupied(mut entry)
                    if candidate.hash_score > entry.get().hash_score =>
                {
                    entry.insert(candidate);
                }
                std::collections::hash_map::Entry::Occupied(_) => {}
            }
        }
    }
    let mut candidates: Vec<Candidate> = candidates_by_track.into_values().collect();
    candidates.sort_by(|left, right| {
        right
            .hash_score
            .cmp(&left.hash_score)
            .then_with(|| right.submission_count.cmp(&left.submission_count))
    });
    Ok(Identification {
        duration_seconds: duration,
        query_hashes: query.len(),
        match_kind: "recording fingerprint candidate; not cross-performance work recognition",
        candidates,
    })
}

#[cfg(test)]
mod tests {
    use super::{extract_query, parse_update_name};
    use chrono::NaiveDate;

    #[test]
    fn query_hashes_skip_silence_mask_and_deduplicate() {
        assert_eq!(
            extract_query(&[0x8123_4567, 0x8123_456f, 627_964_279, 0x1020_304f]),
            vec![0x8123_4560, 0x1020_3040]
        );
    }

    #[test]
    fn long_fingerprints_start_at_query_offset() {
        let hashes: Vec<u32> = (0..200).map(|index| (index << 4) | (index & 0xf)).collect();
        assert_eq!(extract_query(&hashes)[..5], [1280, 1296, 1312, 1328, 1344]);
    }

    #[test]
    fn update_names_reject_unknown_streams() {
        assert_eq!(
            parse_update_name("2026-10-29-fingerprint-update.jsonl.gz"),
            Some((
                NaiveDate::from_ymd_opt(2026, 10, 29).unwrap(),
                "fingerprint"
            ))
        );
        assert!(parse_update_name("2026-10-29-unknown-update.jsonl.gz").is_none());
    }
}
