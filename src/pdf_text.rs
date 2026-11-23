//! PDF 文本层提取。
//!
//! `pdf-extract` 遇到非标准字形名（常见于内嵌 Dingbats 子集字体）时会直接
//! `println!` 一行告警，一本大书能刷出几十万行。终端打印本身非常耗 CPU，
//! 所以解析期间临时把 stdout 指向 `/dev/null`，结束后恢复。

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::UNIX_EPOCH;

/// 重定向 stdout 是进程级操作，同一时刻只允许一个解析在跑，避免互相干扰。
static STDOUT_LOCK: Mutex<()> = Mutex::new(());

/// 缓存条目上限与总字节上限；PDF 文本层通常有数 MB，只留最近几本。
const MAX_CACHE_ENTRIES: usize = 4;
const MAX_CACHE_BYTES: usize = 64 * 1024 * 1024;

/// 文件身份：路径 + 大小 + 修改时间，任一变化即视为新文件。
type Key = (PathBuf, u64, u128);

struct Cache {
    entries: VecDeque<(Key, String)>,
    bytes: usize,
}

static CACHE: Mutex<Cache> = Mutex::new(Cache {
    entries: VecDeque::new(),
    bytes: 0,
});

fn file_key(path: &Path) -> Option<(Key, u64)> {
    let metadata = std::fs::metadata(path).ok()?;
    let modified = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |duration| duration.as_nanos());
    Some((
        (
            path.canonicalize().unwrap_or_else(|_| path.to_path_buf()),
            metadata.len(),
            modified,
        ),
        metadata.len(),
    ))
}

fn cache_get(key: &Key) -> Option<String> {
    let cache = CACHE.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    cache
        .entries
        .iter()
        .find(|(cached, _)| cached == key)
        .map(|(_, text)| text.clone())
}

fn cache_put(key: Key, text: String) {
    let mut cache = CACHE.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    cache.bytes += text.len();
    cache.entries.push_back((key, text));
    while cache.entries.len() > MAX_CACHE_ENTRIES || cache.bytes > MAX_CACHE_BYTES {
        // 至少保留刚插入的一条。
        if cache.entries.len() <= 1 {
            break;
        }
        if let Some((_, dropped)) = cache.entries.pop_front() {
            cache.bytes = cache.bytes.saturating_sub(dropped.len());
        }
    }
}

/// 按文件身份缓存文本层：元数据扫描、正文搜索、打开阅读不会重复解析同一本 PDF。
pub fn extract_cached(path: &Path, max_bytes: u64) -> Result<String, String> {
    let (key, len) = file_key(path).ok_or_else(|| format!("读取 {} 失败", path.display()))?;
    if let Some(text) = cache_get(&key) {
        return Ok(text);
    }
    if len > max_bytes {
        return Err(format!("PDF 超过 {}MB，暂不支持", max_bytes / 1024 / 1024));
    }
    let bytes = std::fs::read(path).map_err(|error| format!("读取 {} 失败：{error}", path.display()))?;
    let text = extract(&bytes)?;
    cache_put(key, text.clone());
    Ok(text)
}

/// 提取 PDF 文本层；解析期间不输出第三方库的调试信息。
pub fn extract(bytes: &[u8]) -> Result<String, String> {
    let _guard = STDOUT_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let _quiet = QuietStdout::new();
    std::panic::catch_unwind(|| pdf_extract::extract_text_from_mem(bytes))
        .map_err(|_| "PDF 解析器异常退出".to_string())?
        .map_err(|error| format!("PDF 解析失败：{error}"))
}

#[cfg(unix)]
struct QuietStdout {
    saved: Option<std::os::fd::RawFd>,
}

#[cfg(unix)]
impl QuietStdout {
    fn new() -> Self {
        // SAFETY: 调用方持有 `STDOUT_LOCK`，dup/dup2 期间不会有其它解析线程改回 stdout；
        // 文件描述符在 `Drop` 中恢复并关闭。
        unsafe {
            let saved = libc::dup(libc::STDOUT_FILENO);
            if saved < 0 {
                return Self { saved: None };
            }
            let devnull = libc::open(b"/dev/null\0".as_ptr().cast(), libc::O_WRONLY);
            if devnull < 0 {
                libc::close(saved);
                return Self { saved: None };
            }
            libc::dup2(devnull, libc::STDOUT_FILENO);
            libc::close(devnull);
            Self { saved: Some(saved) }
        }
    }
}

#[cfg(unix)]
impl Drop for QuietStdout {
    fn drop(&mut self) {
        if let Some(saved) = self.saved {
            // SAFETY: `saved` 是 `new` 中 dup 出来的有效描述符。
            unsafe {
                libc::dup2(saved, libc::STDOUT_FILENO);
                libc::close(saved);
            }
        }
    }
}

#[cfg(not(unix))]
struct QuietStdout;

#[cfg(not(unix))]
impl QuietStdout {
    fn new() -> Self {
        Self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_text_from_generated_pdf() {
        let bytes = crate::book::build_test_pdf(&["Hello quiet"]);
        let text = extract(&bytes).expect("pdf text");
        assert!(text.contains("Hello quiet"));
    }

    #[test]
    fn caches_by_file_identity_and_invalidates_on_change() {
        let dir = std::env::temp_dir().join(format!(
            "sepiola-pdf-cache-{}",
            std::time::SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).expect("mkdir");
        let path = dir.join("cached.pdf");
        std::fs::write(&path, crate::book::build_test_pdf(&["Alpha"])).expect("write");
        let first = extract_cached(&path, u64::MAX).expect("first");
        assert!(first.contains("Alpha"));
        // 命中缓存：内容不变，结果一致。
        assert_eq!(extract_cached(&path, u64::MAX).expect("hit"), first);
        // 文件被替换后要重新解析（大小与修改时间都变了）。
        std::fs::write(
            &path,
            crate::book::build_test_pdf(&["Beta", "a much longer second line"]),
        )
        .expect("rewrite");
        let second = extract_cached(&path, u64::MAX).expect("second");
        assert!(second.contains("Beta"));
        assert!(!second.contains("Alpha"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn rejects_file_over_limit() {
        let dir = std::env::temp_dir().join(format!(
            "sepiola-pdf-limit-{}",
            std::time::SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).expect("mkdir");
        let path = dir.join("big.pdf");
        std::fs::write(&path, crate::book::build_test_pdf(&["Small"])).expect("write");
        assert!(extract_cached(&path, 1).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }
}
