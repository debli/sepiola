//! 共享 AI 后端（M46）：Google Gemini 原生接口、OpenAI 兼容 HTTP 接口或外部 CLI
//! （copilot / 自定义命令）。
//!
//! 提示词统一作为 CLI 的最后一个 argv 追加，避免引号与空格转义；HTTP 路径沿用
//! `{base_url}/chat/completions`。所有调用都是阻塞的，由 GUI 放到后台线程执行。

use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use reqwest::Proxy;
use serde_json::json;

use crate::config::{AiProvider, ReaderAiSettings};

/// 单次请求最多携带的字符数，避免超出上下文。
pub const MAX_CHARS: usize = 4000;
/// CLI 默认超时。
pub const CLI_TIMEOUT: Duration = Duration::from_secs(60);

/// 未配置后端时的统一提示。
pub const MISSING_HINT: &str = "未配置 AI 后端：请在设置页选择后端并填写接口或 CLI 参数";

/// AI 调用日志文件路径：`SEPIOLA_AI_LOG` 可覆盖，默认 `~/.config/sepiola/ai.log`。
pub fn log_path() -> PathBuf {
    if let Some(path) = std::env::var_os("SEPIOLA_AI_LOG") {
        if !path.is_empty() {
            return PathBuf::from(path);
        }
    }
    let dir = if let Some(config) = std::env::var_os("XDG_CONFIG_HOME") {
        PathBuf::from(config).join("sepiola")
    } else if let Some(home) = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
    {
        PathBuf::from(home).join(".config").join("sepiola")
    } else {
        std::env::temp_dir()
    };
    dir.join("ai.log")
}

/// 追加一条带时间戳的 AI 调用日志（同时写到 stderr）；写文件失败时静默忽略。
pub fn log(message: impl AsRef<str>) {
    let line = format!(
        "{} [ai] {}\n",
        chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f"),
        message.as_ref()
    );
    eprint!("{line}");
    let path = log_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(&path) {
        // 一次 write_all 写入整行，避免多处并发日志互相穿插。
        let _ = file.write_all(line.as_bytes());
    }
}

/// 单次 AI 调用的耗时与额度消耗；CLI 后端拿不到 tokens 数据（记 `None`）。
#[derive(Clone, Debug, Default)]
pub struct AiUsage {
    pub model: String,
    pub prompt_tokens: Option<u32>,
    pub completion_tokens: Option<u32>,
    pub elapsed_secs: f64,
}

impl AiUsage {
    /// 一行摘要，如 `耗时 1.2s · tokens 123 + 456`；tokens 未知时只显示耗时。
    pub fn summary(&self) -> String {
        let mut parts = vec![format!("耗时 {:.1}s", self.elapsed_secs)];
        match (self.prompt_tokens, self.completion_tokens) {
            (Some(input), Some(output)) => parts.push(format!("tokens {input} + {output}")),
            (None, Some(output)) => parts.push(format!("tokens 输出 {output}")),
            _ => {}
        }
        parts.join(" · ")
    }
}

/// AI 用量统计日志路径：`SEPIOLA_AI_USAGE_LOG` 可覆盖，默认 `~/.config/sepiola/ai-usage.jsonl`。
pub fn usage_log_path() -> PathBuf {
    if let Some(path) = std::env::var_os("SEPIOLA_AI_USAGE_LOG") {
        if !path.is_empty() {
            return PathBuf::from(path);
        }
    }
    let dir = if let Some(config) = std::env::var_os("XDG_CONFIG_HOME") {
        PathBuf::from(config).join("sepiola")
    } else if let Some(home) = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
    {
        PathBuf::from(home).join(".config").join("sepiola")
    } else {
        std::env::temp_dir()
    };
    dir.join("ai-usage.jsonl")
}

/// 追加一条 JSONL 用量记录（时间 / 类型 / 模型 / tokens / 耗时），便于以后统计各 API 消耗。
fn log_usage(provider: &str, usage: &AiUsage, sent_chars: usize, reply_chars: usize) {
    let record = json!({
        "time": chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f").to_string(),
        "provider": provider,
        "model": usage.model,
        "prompt_tokens": usage.prompt_tokens,
        "completion_tokens": usage.completion_tokens,
        "elapsed_secs": usage.elapsed_secs,
        "sent_chars": sent_chars,
        "reply_chars": reply_chars,
    });
    log(format!(
        "用量 provider={provider} model={} {} 发送={sent_chars} 字 回复={reply_chars} 字",
        usage.model,
        usage.summary()
    ));
    let path = usage_log_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(file, "{record}");
    }
}

/// `ai-usage.jsonl` 中的一条调用记录。
#[derive(Clone, Debug, Default, serde::Deserialize)]
pub struct UsageRecord {
    #[serde(default)]
    pub time: String,
    #[serde(default)]
    pub provider: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub prompt_tokens: Option<u32>,
    #[serde(default)]
    pub completion_tokens: Option<u32>,
    #[serde(default)]
    pub elapsed_secs: f64,
}

/// 按 provider + 模型聚合的用量。
#[derive(Clone, Debug, Default)]
pub struct UsageByModel {
    pub provider: String,
    pub model: String,
    pub calls: usize,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub elapsed_secs: f64,
}

/// AI 用量汇总，供设置页展示。
#[derive(Clone, Debug, Default)]
pub struct UsageSummary {
    pub calls: usize,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub elapsed_secs: f64,
    pub today_calls: usize,
    pub today_prompt_tokens: u64,
    pub today_completion_tokens: u64,
    pub by_model: Vec<UsageByModel>,
}

/// 读取 `ai-usage.jsonl` 并汇总；文件缺失或单行损坏时跳过，不影响展示。
pub fn usage_summary() -> UsageSummary {
    let Ok(text) = std::fs::read_to_string(usage_log_path()) else {
        return UsageSummary::default();
    };
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    let mut summary = UsageSummary::default();
    let mut groups: std::collections::BTreeMap<(String, String), UsageByModel> =
        std::collections::BTreeMap::new();
    for line in text.lines() {
        let Ok(record) = serde_json::from_str::<UsageRecord>(line) else {
            continue;
        };
        let prompt = record.prompt_tokens.unwrap_or_default() as u64;
        let completion = record.completion_tokens.unwrap_or_default() as u64;
        summary.calls += 1;
        summary.prompt_tokens += prompt;
        summary.completion_tokens += completion;
        summary.elapsed_secs += record.elapsed_secs;
        if record.time.starts_with(&today) {
            summary.today_calls += 1;
            summary.today_prompt_tokens += prompt;
            summary.today_completion_tokens += completion;
        }
        let group = groups
            .entry((record.provider.clone(), record.model.clone()))
            .or_insert_with(|| UsageByModel {
                provider: record.provider.clone(),
                model: record.model.clone(),
                ..UsageByModel::default()
            });
        group.calls += 1;
        group.prompt_tokens += prompt;
        group.completion_tokens += completion;
        group.elapsed_secs += record.elapsed_secs;
    }
    summary.by_model = groups.into_values().collect();
    summary.by_model.sort_by(|a, b| {
        (b.prompt_tokens + b.completion_tokens)
            .cmp(&(a.prompt_tokens + a.completion_tokens))
            .then_with(|| b.calls.cmp(&a.calls))
    });
    summary
}

/// 按配置的后端发送一次请求；`system_prompt` 为角色说明，`user_text` 为用户内容。
pub fn ask(
    settings: &ReaderAiSettings,
    system_prompt: &str,
    user_text: &str,
) -> Result<String, String> {
    ask_with_usage(settings, system_prompt, user_text).map(|(text, _)| text)
}

/// 同 `ask`，但可指定携带的最大字符数（整章分析需要发送更长的正文）。
pub fn ask_with_limit(
    settings: &ReaderAiSettings,
    system_prompt: &str,
    user_text: &str,
    max_chars: usize,
) -> Result<String, String> {
    ask_with_usage_with_limit(settings, system_prompt, user_text, max_chars).map(|(text, _)| text)
}

/// 同 `ask`，额外返回耗时与 token 用量（结果显示与统计用）。
pub fn ask_with_usage(
    settings: &ReaderAiSettings,
    system_prompt: &str,
    user_text: &str,
) -> Result<(String, AiUsage), String> {
    ask_with_usage_with_limit(settings, system_prompt, user_text, MAX_CHARS)
}

/// 同 `ask_with_limit`，额外返回耗时与 token 用量。
pub fn ask_with_usage_with_limit(
    settings: &ReaderAiSettings,
    system_prompt: &str,
    user_text: &str,
    max_chars: usize,
) -> Result<(String, AiUsage), String> {
    if !settings.enabled() {
        return Err(MISSING_HINT.to_string());
    }
    let user_text = user_text.trim();
    if user_text.is_empty() {
        return Err("没有可处理的文本".to_string());
    }
    let user_text: String = user_text.chars().take(max_chars).collect();
    let started = Instant::now();
    log(format!(
        "请求开始 provider={} 正文={} 字 上限={} 字",
        settings.provider.key(),
        user_text.chars().count(),
        max_chars
    ));
    let result = match settings.provider {
        AiProvider::Api => ask_api(settings, system_prompt, &user_text),
        AiProvider::Gemini => ask_gemini(settings, system_prompt, &user_text),
        AiProvider::Copilot | AiProvider::Command => {
            let prompt = format!("{system_prompt}\n\n{user_text}");
            let cli_started = Instant::now();
            ask_cli(settings, &prompt).map(|text| {
                (
                    text,
                    AiUsage {
                        model: model_hint(&cli_args_for(settings)),
                        prompt_tokens: None,
                        completion_tokens: None,
                        elapsed_secs: cli_started.elapsed().as_secs_f64(),
                    },
                )
            })
        }
    };
    if let Ok((reply, usage)) = &result {
        log_usage(
            settings.provider.key(),
            usage,
            user_text.chars().count(),
            reply.chars().count(),
        );
    }
    match &result {
        Ok((reply, usage)) => log(format!(
            "请求完成 provider={} 耗时={:.2}s 回复={} 字 model={}",
            settings.provider.key(),
            started.elapsed().as_secs_f64(),
            reply.chars().count(),
            usage.model
        )),
        Err(error) => log(format!(
            "请求失败 provider={} 耗时={:.2}s 错误={error}",
            settings.provider.key(),
            started.elapsed().as_secs_f64()
        )),
    }
    result
}

/// 调用 `{base_url}/chat/completions`。
pub fn ask_api(
    settings: &ReaderAiSettings,
    system_prompt: &str,
    user_text: &str,
) -> Result<(String, AiUsage), String> {
    let endpoint = format!(
        "{}/chat/completions",
        settings.base_url.trim().trim_end_matches('/')
    );
    log(format!(
        "api 请求 endpoint={endpoint} model={}",
        settings.model.trim()
    ));
    let started = Instant::now();
    let body = json!({
        "model": settings.model.trim(),
        "temperature": 0.3,
        "messages": [
            {"role": "system", "content": system_prompt},
            {"role": "user", "content": user_text},
        ]
    });
    let client = build_client(settings)?;
    let mut request = client.post(endpoint).json(&body);
    if !settings.api_key.trim().is_empty() {
        request = request.bearer_auth(settings.api_key.trim());
    }
    let response = request.send().map_err(|error| format!("请求失败：{error}"))?;
    let status = response.status();
    log(format!(
        "api 响应 status={status} 耗时={:.2}s",
        started.elapsed().as_secs_f64()
    ));
    let payload: serde_json::Value = response
        .json()
        .map_err(|error| format!("接口响应无法解析：{error}"))?;
    if !status.is_success() {
        let message = payload["error"]["message"].as_str().unwrap_or("未知错误");
        return Err(format!("接口返回 {status}：{message}"));
    }
    let content = payload["choices"][0]["message"]["content"]
        .as_str()
        .map(str::trim)
        .filter(|content| !content.is_empty())
        .map(str::to_string)
        .ok_or_else(|| "接口未返回文本内容".to_string())?;
    let usage = AiUsage {
        model: payload["model"]
            .as_str()
            .unwrap_or(settings.model.trim())
            .to_string(),
        prompt_tokens: payload["usage"]["prompt_tokens"].as_u64().map(|v| v as u32),
        completion_tokens: payload["usage"]["completion_tokens"]
            .as_u64()
            .map(|v| v as u32),
        elapsed_secs: started.elapsed().as_secs_f64(),
    };
    Ok((content, usage))
}

/// 按设置构建带 60 秒超时的 reqwest 客户端；`proxy_http` / `proxy_https`
/// 非空时分别作为 HTTP 与 HTTPS 代理（HTTPS 留空回退 HTTP）。
fn build_client(settings: &ReaderAiSettings) -> Result<reqwest::blocking::Client, String> {
    let mut builder = reqwest::blocking::Client::builder().timeout(Duration::from_secs(60));
    let http = settings.proxy_http.trim();
    let mut https = settings.proxy_https.trim();
    if https.is_empty() {
        https = http;
    }
    if !http.is_empty() {
        builder = builder.proxy(Proxy::http(http).map_err(|error| format!("HTTP 代理无效：{error}"))?);
    }
    if !https.is_empty() {
        builder = builder.proxy(Proxy::https(https).map_err(|error| format!("HTTPS 代理无效：{error}"))?);
    }
    builder.build().map_err(|error| format!("创建请求失败：{error}"))
}

/// Google Gemini 原生接口：`{base_url}/v1beta/models/{model}:generateContent`。
///
/// 鉴权用 `x-goog-api-key` 头；`base_url` 留空时使用官方地址。
pub fn ask_gemini(
    settings: &ReaderAiSettings,
    system_prompt: &str,
    user_text: &str,
) -> Result<(String, AiUsage), String> {
    const DEFAULT_BASE: &str = "https://generativelanguage.googleapis.com";
    let base = settings.base_url.trim().trim_end_matches('/');
    let base = if base.is_empty() { DEFAULT_BASE } else { base };
    let model = settings.model.trim();
    let endpoint = format!("{base}/v1beta/models/{model}:generateContent");
    log(format!("gemini 请求 endpoint={endpoint} model={model}"));
    let started = Instant::now();
    let body = json!({
        "system_instruction": {"parts": [{"text": system_prompt}]},
        "contents": [{"role": "user", "parts": [{"text": user_text}]}],
        "generationConfig": {"temperature": 0.3},
    });
    let client = build_client(settings)?;
    let response = client
        .post(endpoint)
        .header("x-goog-api-key", settings.api_key.trim())
        .json(&body)
        .send()
        .map_err(|error| format!("请求失败：{error}"))?;
    let status = response.status();
    log(format!(
        "gemini 响应 status={status} 耗时={:.2}s",
        started.elapsed().as_secs_f64()
    ));
    let payload: serde_json::Value = response
        .json()
        .map_err(|error| format!("接口响应无法解析：{error}"))?;
    if !status.is_success() {
        let message = payload["error"]["message"].as_str().unwrap_or("未知错误");
        return Err(format!("接口返回 {status}：{message}"));
    }
    // `candidates[0].content.parts[*].text`，多段时拼接。
    if let Some(parts) = payload["candidates"][0]["content"]["parts"].as_array() {
        let text: String = parts
            .iter()
            .filter_map(|part| part["text"].as_str())
            .collect::<Vec<_>>()
            .join("");
        let text = text.trim();
        if !text.is_empty() {
            let usage = AiUsage {
                model: model.to_string(),
                prompt_tokens: payload["usageMetadata"]["promptTokenCount"]
                    .as_u64()
                    .map(|v| v as u32),
                completion_tokens: payload["usageMetadata"]["candidatesTokenCount"]
                    .as_u64()
                    .map(|v| v as u32),
                elapsed_secs: started.elapsed().as_secs_f64(),
            };
            return Ok((text.to_string(), usage));
        }
    }
    Err("接口未返回文本内容".to_string())
}

/// 执行 CLI，提示词作为最后一个参数；默认 60 秒超时。
pub fn ask_cli(settings: &ReaderAiSettings, prompt: &str) -> Result<String, String> {
    ask_cli_with_timeout(settings, prompt, CLI_TIMEOUT)
}

pub fn ask_cli_with_timeout(
    settings: &ReaderAiSettings,
    prompt: &str,
    timeout: Duration,
) -> Result<String, String> {
    if !settings.enabled() {
        return Err(MISSING_HINT.to_string());
    }
    let args = cli_args_for(settings);
    log(format!(
        "cli 请求 路径={} 参数={:?} 提示词={} 字 超时={:.1}s model={}",
        settings.cli_path(),
        args,
        prompt.chars().count(),
        timeout.as_secs_f64(),
        model_hint(&args)
    ));
    let started = Instant::now();
    let mut command = Command::new(settings.cli_path());
    command.args(&args).arg(prompt);
    // CLI 子进程继承代理环境变量（如 copilot 需要代理访问模型服务）。
    let http = settings.proxy_http.trim();
    let mut https = settings.proxy_https.trim();
    if https.is_empty() {
        https = http;
    }
    if !http.is_empty() {
        command.env("HTTP_PROXY", http).env("http_proxy", http);
    }
    if !https.is_empty() {
        command.env("HTTPS_PROXY", https).env("https_proxy", https);
    }
    let outcome = run_command(command, timeout);
    match &outcome {
        Ok((status, stdout, stderr)) => log(format!(
            "cli 退出 状态={} 耗时={:.2}s stdout={} 字 stderr={} 字",
            status.code().unwrap_or(-1),
            started.elapsed().as_secs_f64(),
            stdout.chars().count(),
            stderr.chars().count()
        )),
        Err(error) => log(format!(
            "cli 失败 耗时={:.2}s 错误={error}",
            started.elapsed().as_secs_f64()
        )),
    }
    let (status, stdout, stderr) = outcome?;
    if !status.success() {
        let message = stderr.trim();
        return Err(format!(
            "CLI 退出码 {}：{}",
            status.code().unwrap_or(-1),
            if message.is_empty() {
                "无错误输出"
            } else {
                message
            }
        ));
    }
    let text = stdout.trim();
    if text.is_empty() {
        let message = stderr.trim();
        return Err(if message.is_empty() {
            "CLI 未返回文本".to_string()
        } else {
            format!("CLI 未返回文本：{message}")
        });
    }
    Ok(text.to_string())
}

/// 探测 CLI：依次尝试 `--version` 与 `--help`，返回首个成功命令的输出。
pub fn probe_cli(path: &str, timeout: Duration) -> Result<String, String> {
    let path = {
        let trimmed = path.trim();
        if trimmed.is_empty() {
            "copilot"
        } else {
            trimmed
        }
    };
    let mut errors = Vec::new();
    for flag in ["--version", "--help"] {
        let mut command = Command::new(path);
        command.arg(flag);
        match run_command(command, timeout) {
            Ok((status, stdout, stderr)) => {
                let output = if !stdout.trim().is_empty() {
                    stdout.trim()
                } else {
                    stderr.trim()
                };
                if status.success() {
                    let first = output.lines().next().unwrap_or_default().trim();
                    return Ok(if first.is_empty() {
                        format!("{path} {flag} 正常退出")
                    } else {
                        format!("{first}")
                    });
                }
                errors.push(format!(
                    "{flag} 退出码 {}：{}",
                    status.code().unwrap_or(-1),
                    output.lines().next().unwrap_or("无输出")
                ));
            }
            Err(error) => errors.push(format!("{flag}：{error}")),
        }
    }
    Err(format!("{path} 探测失败：{}", errors.join("；")))
}

/// 实际传给 CLI 的参数：copilot 后端自动补上提速参数与模型，自定义命令保持原样。
fn cli_args_for(settings: &ReaderAiSettings) -> Vec<String> {
    let mut args = split_args(&settings.cli_args);
    if settings.provider == AiProvider::Copilot {
        // 必须插在 `-p/--prompt` 之前：`-p` 的值是紧随其后的提示词，中间不能夹参数。
        let mut extra = Vec::new();
        if !args.iter().any(|arg| arg == "-s" || arg == "--silent") {
            extra.push("-s".to_string());
        }
        if !args.iter().any(|arg| arg == "--disable-builtin-mcps") {
            extra.push("--disable-builtin-mcps".to_string());
        }
        if !args.iter().any(|arg| arg == "--model" || arg == "-m") {
            extra.push("--model".to_string());
            extra.push(settings.cli_model().to_string());
        }
        args.splice(0..0, extra);
    }
    args
}

/// 日志里展示 CLI 实际指定的模型；没有 `--model` 时说明用的是 CLI 默认。
fn model_hint(args: &[String]) -> String {
    args.iter()
        .position(|arg| arg == "--model" || arg == "-m")
        .and_then(|index| args.get(index + 1))
        .cloned()
        .unwrap_or_else(|| "未指定（使用 CLI 默认模型）".to_string())
}

/// 简单 shell 风格参数切分：支持单/双引号与反斜杠转义，足够覆盖 `-p --flag "a b"`。
pub fn split_args(text: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut escaped = false;
    let mut started = false;
    for ch in text.chars() {
        if escaped {
            current.push(ch);
            escaped = false;
            started = true;
            continue;
        }
        match ch {
            '\\' if quote != Some('\'') => {
                escaped = true;
                started = true;
            }
            '"' | '\'' => {
                match quote {
                    Some(active) if active == ch => quote = None,
                    Some(_) => current.push(ch),
                    None => quote = Some(ch),
                }
                started = true;
            }
            ch if ch.is_whitespace() && quote.is_none() => {
                if started {
                    args.push(std::mem::take(&mut current));
                    started = false;
                }
            }
            ch => {
                current.push(ch);
                started = true;
            }
        }
    }
    if escaped {
        current.push('\\');
    }
    if started {
        args.push(current);
    }
    args
}

/// 从 AI 返回中截取第一个 JSON 对象（容忍 ```json 围栏与前后说明文字）。
pub fn json_object(text: &str) -> Option<&str> {
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    (end > start).then(|| &text[start..=end])
}

fn run_command(
    mut command: Command,
    timeout: Duration,
) -> Result<(ExitStatus, String, String), String> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command
        .spawn()
        .map_err(|error| format!("启动命令失败：{error}"))?;
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let stdout_reader = std::thread::spawn(move || read_all(stdout));
    let stderr_reader = std::thread::spawn(move || read_all(stderr));
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if Instant::now() >= deadline {
                    log(format!("命令超时 {:.1} 秒，终止进程", timeout.as_secs_f64()));
                    let _ = child.kill();
                    let _ = child.wait();
                    // CLI 的子进程可能仍持有管道；只做短暂等待，避免超时处理被拖成几分钟。
                    let grace = Instant::now() + Duration::from_millis(300);
                    while Instant::now() < grace
                        && !(stdout_reader.is_finished() && stderr_reader.is_finished())
                    {
                        std::thread::sleep(Duration::from_millis(20));
                    }
                    let stdout = stdout_reader
                        .is_finished()
                        .then(|| stdout_reader.join().unwrap_or_default());
                    let stderr = stderr_reader
                        .is_finished()
                        .then(|| stderr_reader.join().unwrap_or_default());
                    log(format!(
                        "超时时已收集输出 stdout={} stderr={} 尾部={}",
                        stdout
                            .as_ref()
                            .map(|text| text.chars().count().to_string())
                            .unwrap_or_else(|| "未关闭".to_string()),
                        stderr
                            .as_ref()
                            .map(|text| text.chars().count().to_string())
                            .unwrap_or_else(|| "未关闭".to_string()),
                        stderr
                            .as_deref()
                            .map(|text| tail(text, 300))
                            .unwrap_or_default()
                    ));
                    let hint = stderr
                        .as_deref()
                        .map(|text| tail(text, 200))
                        .unwrap_or_default();
                    return Err(if hint.is_empty() {
                        format!("命令超过 {} 秒未返回，已终止", timeout.as_secs())
                    } else {
                        format!(
                            "命令超过 {} 秒未返回，已终止（stderr：{hint}）",
                            timeout.as_secs()
                        )
                    });
                }
                std::thread::sleep(Duration::from_millis(40));
            }
            Err(error) => return Err(format!("等待命令失败：{error}")),
        }
    };
    let stdout = stdout_reader.join().unwrap_or_default();
    let stderr = stderr_reader.join().unwrap_or_default();
    Ok((status, stdout, stderr))
}

fn read_all(stream: Option<impl Read>) -> String {
    let Some(mut stream) = stream else {
        return String::new();
    };
    let mut buffer = Vec::new();
    let _ = stream.read_to_end(&mut buffer);
    String::from_utf8_lossy(&buffer).into_owned()
}

/// 取文本末尾至多 `limit` 个字符并压成一行，用于日志里的 stderr 摘要。
fn tail(text: &str, limit: usize) -> String {
    let trimmed = text.trim();
    let count = trimmed.chars().count();
    let slice: String = if count <= limit {
        trimmed.to_string()
    } else {
        trimmed.chars().skip(count - limit).collect()
    };
    slice.replace(['\r', '\n'], " ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AiProvider;

    #[test]
    fn splits_quoted_arguments() {
        assert_eq!(
            split_args(r#"-p --model "gpt 4" 'a b' c\ d"#),
            ["-p", "--model", "gpt 4", "a b", "c d"]
        );
        assert!(split_args("   ").is_empty());
        assert_eq!(split_args(r#""a 'b'"#), ["a 'b'"]);
    }

    #[test]
    fn copilot_args_include_speed_flags_and_model() {
        let settings = ReaderAiSettings {
            provider: AiProvider::Copilot,
            cli_args: "-p".to_string(),
            ..Default::default()
        };
        assert_eq!(
            cli_args_for(&settings),
            ["-s", "--disable-builtin-mcps", "--model", "gpt-6-luna", "-p"].map(str::to_string)
        );

        let custom = ReaderAiSettings {
            provider: AiProvider::Copilot,
            cli_args: "-p --model custom -s".to_string(),
            ..Default::default()
        };
        let args = cli_args_for(&custom);
        assert_eq!(args.iter().filter(|arg| *arg == "--model").count(), 1);
        assert!(args.contains(&"custom".to_string()));
        assert_eq!(args.iter().filter(|arg| *arg == "-s").count(), 1);
        assert!(args.contains(&"--disable-builtin-mcps".to_string()));

        let command = ReaderAiSettings {
            provider: AiProvider::Command,
            cli_args: "-p".to_string(),
            ..Default::default()
        };
        assert_eq!(cli_args_for(&command), ["-p"].map(str::to_string));
    }

    #[test]
    fn extracts_json_from_fenced_reply() {
        let reply = "说明如下：\n```json\n{\"a\": 1}\n```\n完成";
        assert_eq!(json_object(reply), Some("{\"a\": 1}"));
        assert!(json_object("no json").is_none());
    }

    #[test]
    fn cli_receives_prompt_as_last_argument() {
        #[cfg(unix)]
        {
            use std::io::Write;
            use std::os::unix::fs::PermissionsExt;
            let dir = std::env::temp_dir().join(format!(
                "sepiola-ai-cli-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|duration| duration.as_nanos())
                    .unwrap_or(0)
            ));
            std::fs::create_dir_all(&dir).expect("mkdir");
            let script = dir.join("fake-cli.sh");
            let mut file = std::fs::File::create(&script).expect("create");
            writeln!(file, "#!/bin/sh\nprintf '%s' \"$2\"").expect("write");
            drop(file);
            let mut permissions = std::fs::metadata(&script).expect("meta").permissions();
            permissions.set_mode(0o755);
            std::fs::set_permissions(&script, permissions).expect("chmod");
            let settings = ReaderAiSettings {
                provider: AiProvider::Command,
                cli_path: script.display().to_string(),
                cli_args: "--prompt".to_string(),
                ..Default::default()
            };
            let reply = ask_cli(&settings, "hello prompt").expect("cli reply");
            assert_eq!(reply, "hello prompt");
            std::fs::remove_dir_all(&dir).ok();
        }
    }

    #[test]
    fn cli_timeout_is_reported() {
        #[cfg(unix)]
        {
            use std::io::Write;
            use std::os::unix::fs::PermissionsExt;
            let dir = std::env::temp_dir().join(format!(
                "sepiola-ai-timeout-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|duration| duration.as_nanos())
                    .unwrap_or(0)
            ));
            std::fs::create_dir_all(&dir).expect("mkdir");
            let script = dir.join("slow-cli.sh");
            let mut file = std::fs::File::create(&script).expect("create");
            writeln!(file, "#!/bin/sh\nsleep 5").expect("write");
            drop(file);
            let mut permissions = std::fs::metadata(&script).expect("meta").permissions();
            permissions.set_mode(0o755);
            std::fs::set_permissions(&script, permissions).expect("chmod");
            let settings = ReaderAiSettings {
                provider: AiProvider::Command,
                cli_path: script.display().to_string(),
                ..Default::default()
            };
            let error =
                ask_cli_with_timeout(&settings, "hi", Duration::from_millis(120)).expect_err("timeout");
            assert!(error.contains("超过"), "{error}");
            std::fs::remove_dir_all(&dir).ok();
        }
    }
}
