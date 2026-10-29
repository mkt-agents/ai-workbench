//! Bounded, metadata-only diagnostics; not conversation or business-history storage.
//! Query order is append order (newest first), never a timestamp sort. A cursor is
//! a process-authenticated file-identity/position snapshot: rename-only rotation
//! survives; append, replacement, deletion, retention expiry or restart is stale.
//! Retention cleanup is lazy on writes; reads additionally exclude expired events.

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::fs::{self, File, Metadata, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const FILE_BYTES: u64 = 5 * 1024 * 1024;
const FILE_COUNT: usize = 5; // active file plus four archives, not five archives
const RETENTION: Duration = Duration::from_secs(7 * 24 * 60 * 60);
const EVENT_BYTES: usize = 8 * 1024; // includes the JSONL newline
const SCAN_BYTES: usize = 1024 * 1024;
const PAGE_LIMIT: usize = 200;
const READ_CHUNK: usize = 64 * 1024;
const JS_SAFE_INTEGER: u64 = 9_007_199_254_740_991;
const STALE: &str = "DIAGNOSTICS_CURSOR_STALE: 诊断文件已变化或游标已过期，请刷新列表";

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Event {
    pub schema_version: u32,
    pub timestamp: String,
    pub request_id: String,
    pub phase: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    pub metadata: Value,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct QueryFilter {
    pub request_id: Option<String>,
    pub phase: Option<String>,
    pub status: Option<String>,
    pub from: Option<String>,
    pub to: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Health {
    /// unknown: no successful write observed in this process; ok / degraded.
    pub status: String,
    pub write_failures: u64,
    pub last_error: Option<String>,
    pub last_success_at: Option<String>,
}

impl Default for Health {
    fn default() -> Self {
        Self {
            status: "unknown".into(),
            write_failures: 0,
            last_error: None,
            last_success_at: None,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ReadError {
    /// Stable machine-readable category, never a raw I/O error or source text.
    pub code: String,
    pub count: u64,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QueryPage {
    pub events: Vec<Event>,
    pub next_cursor: Option<String>,
    /// True when more snapshot bytes remain OR any bytes/records were unreadable.
    pub partial: bool,
    pub scanned_bytes: usize,
    pub read_errors: Vec<String>,
    pub health: Health,
}

impl Event {
    fn new(
        request: &str,
        phase: &str,
        status: &str,
        code: Option<&str>,
        duration_ms: Option<u64>,
        metadata: Value,
    ) -> Self {
        Self {
            schema_version: 1,
            timestamp: now(),
            request_id: request_identifier(request),
            phase: label(phase),
            status: label(status),
            code: code.map(label),
            duration_ms: duration_ms.map(|n| n.min(JS_SAFE_INTEGER)),
            metadata: safe_metadata(&metadata, 0, &mut 64),
        }
    }

    /// Disk contents are untrusted too: project again before query/export.
    fn sanitize(&mut self) {
        self.request_id = request_identifier(&self.request_id);
        self.phase = label(&self.phase);
        self.status = label(&self.status);
        self.code = self.code.as_deref().map(label);
        self.duration_ms = self.duration_ms.map(|n| n.min(JS_SAFE_INTEGER));
        self.metadata = safe_metadata(&self.metadata, 0, &mut 64);
    }
}

fn now() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn request_identifier(value: &str) -> String {
    match value.strip_prefix("wecom-preflight-") {
        Some(id) => identifier(&format!("wecom-{id}")),
        None => identifier(value),
    }
}

/// All request/user/chat/session identifiers are hashed, including values
/// that merely *look* like safe ASCII IDs. Existing digests remain searchable.
fn identifier(value: &str) -> String {
    if value.len() == 71
        && value.starts_with("sha256:")
        && value[7..]
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    {
        value.into()
    } else {
        format!("sha256:{}", digest(value.as_bytes()))
    }
}

/// Labels are code identifiers, not prose. Known credential syntax is redacted
/// before validation; rejected values become opaque digests instead of excerpts.
fn label(value: &str) -> String {
    if value.len() <= 48
        && !value.is_empty()
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b))
        && crate::wecom_sanitize::redact_diagnostic(value) == value
    {
        value.into()
    } else {
        identifier(value)
    }
}

/// Whitelist by key AND type. No prompt/body/error-message/url/path fields, no
/// arbitrary string arrays. Unknown keys are dropped at every depth. Counters
/// are unsigned safe JS integers; bounded groups allow structured counters only.
fn safe_metadata(value: &Value, depth: usize, remaining: &mut usize) -> Value {
    let mut out = Map::new();
    if depth > 2 || *remaining == 0 {
        return Value::Object(out);
    }
    let Some(obj) = value.as_object() else {
        return Value::Object(out);
    };
    for (key, value) in obj.iter().take(64) {
        if out.len() >= 24 || *remaining == 0 {
            break;
        }
        *remaining -= 1;
        let safe = match key.as_str() {
            "attempt" | "retryCount" | "requestCount" | "evidenceCount" | "shownCount"
            | "warnCount" | "truncatedCount" | "incompletePages" | "cacheHits" | "inputChars"
            | "outputChars" | "toolEvents" | "calls" | "firstEventMs" | "queueMs" | "elapsedMs"
            | "totalMs" | "bytes" | "count" | "httpStatus" | "exitCode" | "activeJobs"
            | "waitingJobs" | "maxJobs" | "round" | "validationFailures" | "droppedCount"
            | "pid" | "processId" | "channelGeneration" | "page" | "rows" => {
                value.as_u64().map(|n| Value::from(n.min(JS_SAFE_INTEGER)))
            }
            "source" => enum_value(value, &["apply", "restart", "sync"]),
            "changedCount" => value.as_u64().map(Value::from),
            "changedFields" => value.as_array().map(|fields| Value::Array(fields.iter().take(40)
                .filter_map(|v| v.as_str()).filter(|s| s.len() <= 48 && s.bytes().all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b)))
                .map(|s| Value::String(s.to_string())).collect())),
            "errcode" => value
                .as_i64()
                .map(|n| Value::from(n.clamp(-(JS_SAFE_INTEGER as i64), JS_SAFE_INTEGER as i64))),
            "partial" | "truncated" | "sourceReady" | "cancelled" | "timeout" | "reused"
            | "success" | "final" => value.as_bool().map(Value::Bool),
            "requestId" => value.as_str().map(|s| Value::String(request_identifier(s))),
            "sessionId" | "sessionKey" | "streamId" | "messageId" | "msgId" | "chatId"
            | "userId" | "botId" => value.as_str().map(|s| Value::String(identifier(s))),
            "conditionHash" => value
                .as_str()
                .filter(|s| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()))
                .map(|s| Value::String(s.to_ascii_lowercase())),
            "model" => value.as_str().map(|s| Value::String(public_model(s))),
            "buildLabel" => value.as_str().map(|s| Value::String(label(s))),
            "selectionState" => enum_value(
                value,
                &[
                    "applied",
                    "selected",
                    "pending",
                    "failed",
                    "unavailable",
                    "unknown",
                    "configured",
                    "not_observed",
                ],
            ),
            "engine" => enum_value(value, &["dsh", "tianji"]),
            "sessionAction" => enum_value(value, &["new", "reuse", "resume", "reset", "unknown"]),
            "chatType" => enum_value(value, &["dm", "group", "shared", "unknown"]),
            "effort" => enum_value(
                value,
                &["low", "medium", "high", "xhigh", "none", "unknown"],
            ),
            "counts" | "timings" | "flags" if depth < 2 => {
                Some(safe_metadata(value, depth + 1, remaining))
            }
            _ => None,
        };
        if let Some(safe) = safe {
            out.insert(key.clone(), safe);
        }
    }
    Value::Object(out)
}

fn public_model(value: &str) -> String {
    let lower = value.to_ascii_lowercase();
    if !value.is_empty()
        && value.len() <= 160
        && !value.contains("://")
        && !["sk-", "bearer", "enc:"]
            .iter()
            .any(|prefix| lower.starts_with(prefix))
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-/:".contains(&b))
        && crate::wecom_sanitize::redact_diagnostic(value) == value
    {
        value.into()
    } else {
        "unknown".into()
    }
}

fn enum_value(value: &Value, choices: &[&str]) -> Option<Value> {
    value
        .as_str()
        .filter(|s| choices.contains(s))
        .map(|s| Value::String(s.into()))
}

struct Store {
    dir: Result<PathBuf, String>,
    state: Mutex<Health>,
    cursor_key: [u8; 32],
    max_file_bytes: u64,
}

impl Store {
    fn new(dir: Result<PathBuf, String>) -> Self {
        Self {
            dir,
            state: Mutex::new(Health::default()),
            cursor_key: rand::random(),
            max_file_bytes: FILE_BYTES,
        }
    }

    fn lock(&self) -> MutexGuard<'_, Health> {
        self.state.lock().unwrap_or_else(|e| {
            let mut health = e.into_inner();
            health.status = "degraded".into();
            health.last_error = Some("DIAGNOSTICS_LOCK_RECOVERED".into());
            health
        })
    }

    fn path(&self, index: usize) -> Result<PathBuf, String> {
        Ok(self
            .dir
            .as_ref()
            .map_err(Clone::clone)?
            .join(if index == 0 {
                "wecom.jsonl".to_string()
            } else {
                format!("wecom.jsonl.{index}")
            }))
    }

    fn record(&self, event: Event) {
        let mut health = self.lock();
        match self.append(event) {
            Ok(()) => {
                health.status = "ok".into();
                health.last_error = None;
                health.last_success_at = Some(now());
            }
            Err(code) => {
                health.status = "degraded".into();
                health.write_failures = health.write_failures.saturating_add(1);
                health.last_error = Some(code);
            }
        }
        // Deliberately no logging here: a failed writer must not call itself.
    }

    fn append(&self, event: Event) -> Result<(), String> {
        let mut bytes = serde_json::to_vec(&event).map_err(|_| "DIAGNOSTICS_ENCODE".to_string())?;
        bytes.push(b'\n');
        if bytes.len() > EVENT_BYTES {
            return Err("DIAGNOSTICS_EVENT_TOO_LARGE".into());
        }
        let dir = self.dir.as_ref().map_err(Clone::clone)?;
        // Check existing parents before create_dir_all can follow a junction.
        for candidate in [Some(dir.as_path()), dir.parent()].into_iter().flatten() {
            match fs::symlink_metadata(candidate) {
                Ok(meta) if !meta.is_dir() || is_reparse(&meta) => {
                    return Err("DIAGNOSTICS_UNSAFE_DIRECTORY".into())
                }
                Ok(_) => (),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
                Err(_) => return Err("DIAGNOSTICS_DIRECTORY".into()),
            }
        }
        fs::create_dir_all(dir).map_err(|_| "DIAGNOSTICS_CREATE_DIRECTORY".to_string())?;
        ensure_plain_directory(dir)?;
        for i in 0..FILE_COUNT {
            let path = self.path(i)?;
            if let Some(meta) = plain_metadata(&path)? {
                // Creation time, not mtime: continuous append must not extend
                // the lifetime of the oldest event in the active file.
                if expired(&meta, SystemTime::now())? {
                    fs::remove_file(path).map_err(|_| "DIAGNOSTICS_RETENTION".to_string())?;
                }
            }
        }
        let path = self.path(0)?;
        if plain_metadata(&path)?
            .is_some_and(|m| m.len().saturating_add(bytes.len() as u64) > self.max_file_bytes)
        {
            self.rotate()?;
        }
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .map_err(|_| "DIAGNOSTICS_OPEN_WRITE".to_string())?;
        file.write_all(&bytes)
            .and_then(|_| file.flush())
            .map_err(|_| "DIAGNOSTICS_WRITE".to_string())
    }

    fn rotate(&self) -> Result<(), String> {
        let oldest = self.path(FILE_COUNT - 1)?;
        if plain_metadata(&oldest)?.is_some() {
            fs::remove_file(oldest).map_err(|_| "DIAGNOSTICS_ROTATE".to_string())?;
        }
        for i in (0..FILE_COUNT - 1).rev() {
            let source = self.path(i)?;
            if plain_metadata(&source)?.is_some() {
                fs::rename(source, self.path(i + 1)?)
                    .map_err(|_| "DIAGNOSTICS_ROTATE".to_string())?;
            }
        }
        Ok(())
    }

    fn inventory(&self, errors: &mut Vec<ReadError>) -> Result<Vec<(PathBuf, Stamp)>, String> {
        let dir = match self.dir.as_ref() {
            Ok(dir) => dir,
            Err(code) => {
                read_error(errors, code);
                return Ok(Vec::new());
            }
        };
        if dir.exists() {
            if let Err(code) = ensure_plain_directory(dir) {
                read_error(errors, &code);
                return Ok(Vec::new());
            }
        }
        let mut files = Vec::new();
        for index in 0..FILE_COUNT {
            let path = self.path(index)?;
            match plain_metadata(&path).and_then(|meta| match meta {
                Some(meta) if !expired(&meta, SystemTime::now())? => {
                    let file =
                        File::open(&path).map_err(|_| "DIAGNOSTICS_OPEN_READ".to_string())?;
                    Ok(Some(stamp(&file)?))
                }
                _ => Ok(None),
            }) {
                Ok(Some(stamp)) => files.push((path, stamp)),
                Ok(None) => (),
                Err(code) => read_error(errors, &code),
            }
        }
        Ok(files)
    }

    fn query(
        &self,
        filter: QueryFilter,
        cursor: Option<String>,
        limit: Option<usize>,
    ) -> Result<QueryPage, String> {
        let filter = ValidFilter::new(filter)?;
        let limit = limit.unwrap_or(100).clamp(1, PAGE_LIMIT);
        let health = self.lock(); // serialize snapshots with our writer/rotation
        let mut errors = Vec::new();
        let inventory = self.inventory(&mut errors)?;
        let mut state = match cursor {
            Some(token) => {
                let state = self.decode_cursor(&token)?;
                if state.filter != filter.fingerprint
                    || state.files.len() > FILE_COUNT
                    || state.index >= state.files.len()
                    || state.position > state.files[state.index].len
                {
                    return Err(STALE.into());
                }
                // Resolve identity, not rotation slot. Any ambiguity/change fails closed.
                for expected in &state.files {
                    if inventory.iter().filter(|(_, s)| s == expected).count() != 1 {
                        return Err(STALE.into());
                    }
                }
                state
            }
            None => Cursor {
                files: inventory.iter().map(|(_, s)| s.clone()).collect(),
                index: 0,
                position: inventory.first().map_or(0, |(_, s)| s.len),
                skip_fragment: false,
                filter: filter.fingerprint.clone(),
                cutoff: (Utc::now() - chrono::Duration::days(7)).to_rfc3339(),
            },
        };
        let cutoff = parse_time(&state.cutoff)?;
        let mut events = Vec::new();
        let mut scanned = 0;
        'scan: while state.index < state.files.len() && scanned < SCAN_BYTES && events.len() < limit
        {
            let expected = &state.files[state.index];
            let Some((path, _)) = inventory.iter().find(|(_, s)| s == expected) else {
                return Err(STALE.into());
            };
            let mut file = match File::open(path) {
                Ok(file) => file,
                Err(_) => {
                    read_error(&mut errors, "DIAGNOSTICS_OPEN_READ");
                    advance_file(&mut state);
                    continue;
                }
            };
            if stamp(&file)? != *expected {
                return Err(STALE.into());
            }
            while state.position > 0 && scanned < SCAN_BYTES && events.len() < limit {
                let count =
                    (state.position.min(READ_CHUNK as u64) as usize).min(SCAN_BYTES - scanned);
                let start = state.position - count as u64;
                let mut buf = vec![0; count];
                if file.seek(SeekFrom::Start(start)).is_err() {
                    read_error(&mut errors, "DIAGNOSTICS_SEEK");
                    advance_file(&mut state);
                    continue 'scan;
                }
                // Count actual acquired bytes, including corrupt/partial line bytes.
                let mut got = 0;
                while got < count {
                    match file.read(&mut buf[got..]) {
                        Ok(0) => break,
                        Ok(n) => {
                            got += n;
                            scanned += n;
                        }
                        Err(_) => break,
                    }
                }
                if got != count {
                    read_error(&mut errors, "DIAGNOSTICS_READ");
                    advance_file(&mut state);
                    continue 'scan;
                }
                if stamp(&file)? != *expected {
                    return Err(STALE.into());
                }
                let mut end = buf.len();
                while end > 0 && events.len() < limit {
                    let content_end = if buf[end - 1] == b'\n' { end - 1 } else { end };
                    let separator = buf[..content_end].iter().rposition(|b| *b == b'\n');
                    if separator.is_none() && start > 0 {
                        // The prefix may start mid-UTF8 or mid-JSON. Never parse
                        // it as a record. Short prefixes are reread with their
                        // preceding bytes; oversized records drain across pages.
                        if state.skip_fragment || end > EVENT_BYTES {
                            if !state.skip_fragment {
                                read_error(&mut errors, "DIAGNOSTICS_OVERSIZE_LINE");
                            }
                            state.skip_fragment = true;
                            state.position = start;
                        } else {
                            state.position = start + end as u64;
                        }
                        break;
                    }
                    let line_start = separator.map_or(0, |i| i + 1);
                    let line = &buf[line_start..content_end];
                    state.position = start + line_start as u64;
                    if state.skip_fragment {
                        state.skip_fragment = false;
                    } else if line.len() + 1 > EVENT_BYTES {
                        read_error(&mut errors, "DIAGNOSTICS_OVERSIZE_LINE");
                    } else if !line.is_empty() {
                        match std::str::from_utf8(line)
                            .ok()
                            .and_then(|s| serde_json::from_str::<Event>(s).ok())
                        {
                            Some(mut event) => match DateTime::parse_from_rfc3339(&event.timestamp)
                            {
                                Ok(time) if event.schema_version == 1 => {
                                    event.sanitize();
                                    if time >= cutoff && filter.matches(&event, time) {
                                        events.push(event);
                                    }
                                }
                                _ => read_error(&mut errors, "DIAGNOSTICS_INVALID_EVENT"),
                            },
                            None => read_error(&mut errors, "DIAGNOSTICS_INVALID_LINE"),
                        }
                    }
                    end = line_start;
                }
            }
            if state.position == 0 {
                advance_file(&mut state);
            }
        }
        // Include changes during the read in stale detection, even on page 1.
        for (path, expected) in &inventory {
            let current = File::open(path).ok().and_then(|f| stamp(&f).ok());
            if current.as_ref() != Some(expected) {
                return Err(STALE.into());
            }
        }
        let next_cursor = if state.index < state.files.len() {
            Some(self.encode_cursor(&state)?)
        } else {
            None
        };
        Ok(QueryPage {
            partial: next_cursor.is_some() || !errors.is_empty(),
            events,
            next_cursor,
            scanned_bytes: scanned,
            read_errors: errors
                .into_iter()
                .map(|e| format!("{} (count={})", e.code, e.count))
                .collect(),
            health: health.clone(),
        })
    }

    fn encode_cursor(&self, cursor: &Cursor) -> Result<String, String> {
        let data =
            serde_json::to_vec(cursor).map_err(|_| "DIAGNOSTICS_CURSOR_ENCODE".to_string())?;
        Ok(format!(
            "{}.{}",
            URL_SAFE_NO_PAD.encode(&data),
            URL_SAFE_NO_PAD.encode(self.mac(&data))
        ))
    }

    fn decode_cursor(&self, token: &str) -> Result<Cursor, String> {
        if token.len() > 8192 {
            return Err(STALE.into());
        }
        let (data, mac) = token.split_once('.').ok_or_else(|| STALE.to_string())?;
        let data = URL_SAFE_NO_PAD
            .decode(data)
            .map_err(|_| STALE.to_string())?;
        let mac = URL_SAFE_NO_PAD.decode(mac).map_err(|_| STALE.to_string())?;
        let expected = self.mac(&data);
        if mac.len() != expected.len()
            || mac.iter().zip(expected).fold(0u8, |v, (a, b)| v | (a ^ b)) != 0
        {
            return Err(STALE.into());
        }
        serde_json::from_slice(&data).map_err(|_| STALE.to_string())
    }

    /// HMAC-SHA256 using the already available sha2 dependency.
    fn mac(&self, data: &[u8]) -> [u8; 32] {
        let mut inner = [0x36; 64];
        let mut outer = [0x5c; 64];
        for i in 0..32 {
            inner[i] ^= self.cursor_key[i];
            outer[i] ^= self.cursor_key[i];
        }
        let mut hash = Sha256::new();
        hash.update(inner);
        hash.update(data);
        let inside = hash.finalize();
        let mut hash = Sha256::new();
        hash.update(outer);
        hash.update(inside);
        hash.finalize().into()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Stamp {
    id: String,
    len: u64,
    modified: u128,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Cursor {
    files: Vec<Stamp>,
    index: usize,
    position: u64,
    skip_fragment: bool,
    filter: String,
    cutoff: String,
}

fn advance_file(state: &mut Cursor) {
    state.index += 1;
    state.position = state.files.get(state.index).map_or(0, |s| s.len);
    state.skip_fragment = false;
}

fn stamp(file: &File) -> Result<Stamp, String> {
    let meta = file
        .metadata()
        .map_err(|_| "DIAGNOSTICS_METADATA".to_string())?;
    let modified = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .ok_or_else(|| "DIAGNOSTICS_FILE_IDENTITY".to_string())?
        .as_nanos();
    #[cfg(windows)]
    let id = {
        use std::os::windows::io::AsRawHandle;
        use windows::Win32::Foundation::HANDLE;
        use windows::Win32::Storage::FileSystem::{
            GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION,
        };
        let mut info = BY_HANDLE_FILE_INFORMATION::default();
        unsafe { GetFileInformationByHandle(HANDLE(file.as_raw_handle()), &mut info) }
            .map_err(|_| "DIAGNOSTICS_FILE_IDENTITY".to_string())?;
        format!(
            "{}:{}:{}",
            info.dwVolumeSerialNumber, info.nFileIndexHigh, info.nFileIndexLow
        )
    };
    #[cfg(unix)]
    let id = {
        use std::os::unix::fs::MetadataExt;
        format!("{}:{}", meta.dev(), meta.ino())
    };
    #[cfg(not(any(windows, unix)))]
    let id = {
        return Err("DIAGNOSTICS_FILE_IDENTITY_UNSUPPORTED".into());
    };
    Ok(Stamp {
        id,
        len: meta.len(),
        modified,
    })
}

fn is_reparse(meta: &Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        meta.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        meta.file_type().is_symlink()
    }
}

fn ensure_plain_directory(dir: &Path) -> Result<(), String> {
    // Only examine the fixed application/diagnostics directories, not user files.
    for path in [Some(dir), dir.parent()].into_iter().flatten() {
        let meta = fs::symlink_metadata(path).map_err(|_| "DIAGNOSTICS_DIRECTORY".to_string())?;
        if !meta.is_dir() || is_reparse(&meta) {
            return Err("DIAGNOSTICS_UNSAFE_DIRECTORY".into());
        }
    }
    Ok(())
}

fn plain_metadata(path: &Path) -> Result<Option<Metadata>, String> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_file() && !is_reparse(&meta) => Ok(Some(meta)),
        Ok(_) => Err("DIAGNOSTICS_UNSAFE_FILE".into()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err("DIAGNOSTICS_METADATA".into()),
    }
}

fn expired(meta: &Metadata, clock: SystemTime) -> Result<bool, String> {
    // No mtime fallback: that would silently extend retention on every append.
    let created = meta
        .created()
        .map_err(|_| "DIAGNOSTICS_CREATION_TIME".to_string())?;
    Ok(clock.duration_since(created).unwrap_or_default() >= RETENTION)
}

fn read_error(errors: &mut Vec<ReadError>, code: &str) {
    if let Some(error) = errors.iter_mut().find(|error| error.code == code) {
        error.count = error.count.saturating_add(1);
    } else if errors.len() < 32 {
        errors.push(ReadError {
            code: code.into(),
            count: 1,
        });
    }
}

fn parse_time(value: &str) -> Result<DateTime<chrono::FixedOffset>, String> {
    if value.len() > 64 {
        return Err("DIAGNOSTICS_INVALID_TIME: 时间须为 RFC3339".into());
    }
    DateTime::parse_from_rfc3339(value)
        .map_err(|_| "DIAGNOSTICS_INVALID_TIME: 时间须为 RFC3339".into())
}

struct ValidFilter {
    filter: QueryFilter,
    from: Option<DateTime<chrono::FixedOffset>>,
    to: Option<DateTime<chrono::FixedOffset>>,
    fingerprint: String,
}

impl ValidFilter {
    fn new(mut filter: QueryFilter) -> Result<Self, String> {
        let from = filter.from.as_deref().map(parse_time).transpose()?;
        let to = filter.to.as_deref().map(parse_time).transpose()?;
        if from.zip(to).is_some_and(|(a, b)| a > b) {
            return Err("DIAGNOSTICS_INVALID_RANGE: 起始时间晚于结束时间".into());
        }
        filter.request_id = filter.request_id.as_deref().map(request_identifier);
        filter.phase = filter.phase.as_deref().map(label);
        filter.status = filter.status.as_deref().map(label);
        let fingerprint =
            digest(&serde_json::to_vec(&filter).map_err(|_| "DIAGNOSTICS_FILTER".to_string())?);
        Ok(Self {
            filter,
            from,
            to,
            fingerprint,
        })
    }

    fn matches(&self, event: &Event, time: DateTime<chrono::FixedOffset>) -> bool {
        self.filter
            .request_id
            .as_ref()
            .is_none_or(|v| v == &event.request_id)
            && self.filter.phase.as_ref().is_none_or(|v| v == &event.phase)
            && self
                .filter
                .status
                .as_ref()
                .is_none_or(|v| v == &event.status)
            && self.from.is_none_or(|v| time >= v)
            && self.to.is_none_or(|v| time <= v)
    }
}

// Transport correlations are metadata-only and process-local. ACKs without a
// retained binding stay uncorrelated, never guessed from a platform request ID.
fn transport_bindings(
) -> &'static Mutex<std::collections::VecDeque<(String, String, std::time::Instant)>> {
    static BINDINGS: OnceLock<
        Mutex<std::collections::VecDeque<(String, String, std::time::Instant)>>,
    > = OnceLock::new();
    BINDINGS.get_or_init(|| Mutex::new(std::collections::VecDeque::new()))
}

pub fn bind_transport(platform_req: &str, request: &str) {
    if platform_req.is_empty() || request.is_empty() {
        return;
    }
    let key = identifier(platform_req);
    let mut bindings = transport_bindings()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    bindings.retain(|(k, _, time)| k != &key && time.elapsed() < RETENTION);
    while bindings.len() >= 1024 {
        bindings.pop_front();
    }
    bindings.push_back((key, request_identifier(request), std::time::Instant::now()));
}

pub fn transport_request(platform_req: &str) -> Option<String> {
    if platform_req.is_empty() {
        return None;
    }
    let key = identifier(platform_req);
    let mut bindings = transport_bindings()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    bindings.retain(|(_, _, time)| time.elapsed() < RETENTION);
    bindings
        .iter()
        .find(|(k, _, _)| k == &key)
        .map(|(_, request, _)| request.clone())
}

fn store() -> &'static Store {
    static STORE: OnceLock<Store> = OnceLock::new();
    STORE.get_or_init(|| {
        #[cfg(not(test))]
        let dir = std::env::var_os("APPDATA")
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .map(|p| p.join("com.ai-workbench.app").join("diagnostics"))
            .ok_or_else(|| "DIAGNOSTICS_APPDATA_UNAVAILABLE".into());
        // Even accidental calls to the public API under cargo test never touch APPDATA.
        #[cfg(test)]
        let dir = Ok(std::env::temp_dir().join(format!(
            "wecom-diagnostics-api-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        )));
        Store::new(dir)
    })
}

/// Metadata only. Invalid keys/types are dropped before persistence. Write
/// failures are observable via QueryPage.health and never recursively logged.
pub fn record(
    request: &str,
    phase: &str,
    status: &str,
    code: Option<&str>,
    duration_ms: Option<u64>,
    metadata: Value,
) {
    store().record(Event::new(
        request,
        phase,
        status,
        code,
        duration_ms,
        metadata,
    ));
}

pub fn query(
    filter: QueryFilter,
    cursor: Option<String>,
    limit: Option<usize>,
) -> Result<QueryPage, String> {
    store().query(filter, cursor, limit)
}

/// Single bounded page: at most 1 MiB read and 200 matches, potentially truncated.
/// Accepts either the original request identifier or the returned sha256 identifier.
pub fn request(request_id: &str) -> Result<QueryPage, String> {
    query(
        QueryFilter {
            request_id: Some(request_id.into()),
            ..Default::default()
        },
        None,
        Some(PAGE_LIMIT),
    )
}

fn export_page(page: QueryPage) -> Result<String, String> {
    serde_json::to_string_pretty(&serde_json::json!({
        "schemaVersion": 1, "kind": "wecom-diagnostics", "exportedAt": now(),
        "events": page.events,
        "coverage": {
            "partial": page.partial, "scannedBytes": page.scanned_bytes,
            "readErrors": page.read_errors, "hasMore": page.next_cursor.is_some(),
            "maxScannedBytes": SCAN_BYTES, "maxEvents": PAGE_LIMIT,
            "order": "reverseAppend", "retentionDays": 7,
            "scope": "retainedDiagnosticMetadataOnly"
        },
        "health": page.health
    }))
    .map_err(|_| "DIAGNOSTICS_EXPORT".into())
}

/// Returns sanitized JSON for the frontend save dialog; takes no output path and
/// does not export business text. Coverage describes this single bounded query.
pub fn export(request_id: &str) -> Result<String, String> {
    export_page(request(request_id)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::Arc;

    struct Temp {
        path: PathBuf,
    }
    impl Temp {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "wecom-diagnostics-{}-{}",
                std::process::id(),
                rand::random::<u64>()
            ));
            fs::create_dir_all(&path).unwrap();
            Self { path }
        }
        fn store(&self) -> Store {
            Store::new(Ok(self.path.clone()))
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
    fn event(id: &str, n: u64) -> Event {
        Event::new(
            id,
            "collect",
            "ok",
            Some("DONE"),
            Some(n),
            json!({"count": n}),
        )
    }
    fn page(store: &Store) -> QueryPage {
        store
            .query(QueryFilter::default(), None, Some(PAGE_LIMIT))
            .unwrap()
    }

    #[test]
    fn camel_case_and_constructor_redaction_are_bounded() {
        let secret = "Bearer fixture-sensitive-token";
        let e = Event::new(
            secret,
            "collect",
            "ok",
            Some(secret),
            Some(u64::MAX),
            json!({
                "prompt": secret, "body": "业务正文", "token": secret,
                "count": 3, "inputChars": 100, "model": secret, "userId": "alice@example.test",
                "engine": secret, "effort": "high", "flags": {"success": true, "body": secret},
                "counts": {"counts": {"counts": {"count": 5}}, "count": 9},
                "requestCount": "secret", "chatType": "group"
            }),
        );
        let v = serde_json::to_value(&e).unwrap();
        let encoded = serde_json::to_string(&v).unwrap();
        assert!(v.get("requestId").is_some());
        assert!(v.get("schemaVersion").is_some());
        assert!(encoded.len() < EVENT_BYTES);
        for raw in [secret, "业务正文", "alice@example.test", "prompt", "token"] {
            assert!(!encoded.contains(raw));
        }
        assert_eq!(e.duration_ms, Some(JS_SAFE_INTEGER));
        assert_eq!(e.metadata["count"], 3);
        assert_eq!(e.metadata["flags"]["success"], true);
        assert!(e.metadata.get("engine").is_none());
        assert!(e.metadata["counts"]["counts"].get("counts").is_none());
        assert_eq!(identifier(&e.request_id), e.request_id);
    }

    #[test]
    fn filters_and_rfc3339_offsets_are_validated() {
        let temp = Temp::new();
        let store = temp.store();
        store.record(event("a", 1));
        store.record(event("b", 2));
        store.record(Event::new("a", "reply", "error", None, None, json!({})));
        let filter = QueryFilter {
            request_id: Some("a".into()),
            phase: Some("collect".into()),
            status: Some("ok".into()),
            from: Some((Utc::now() - chrono::Duration::hours(1)).to_rfc3339()),
            to: Some((Utc::now() + chrono::Duration::hours(1)).to_rfc3339()),
        };
        let p = store.query(filter, None, None).unwrap();
        assert_eq!(p.events.len(), 1);
        assert_eq!(p.events[0].metadata["count"], 1);
        assert!(store
            .query(
                QueryFilter {
                    from: Some("yesterday".into()),
                    ..Default::default()
                },
                None,
                None
            )
            .is_err());
        assert!(ValidFilter::new(QueryFilter {
            from: Some("2026-01-02T00:00:00Z".into()),
            to: Some("2026-01-01T00:00:00Z".into()),
            ..Default::default()
        })
        .is_err());
        let f = ValidFilter::new(QueryFilter {
            from: Some("2026-01-01T08:00:00+08:00".into()),
            to: Some("2026-01-01T00:00:00Z".into()),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(f.from, f.to);
    }

    #[test]
    fn bounded_page_and_cursor_do_not_duplicate_events() {
        let temp = Temp::new();
        let store = temp.store();
        for n in 0..260 {
            store.record(event("a", n));
        }
        let p = store
            .query(QueryFilter::default(), None, Some(9999))
            .unwrap();
        assert_eq!(p.events.len(), 200);
        assert!(p.partial);
        assert!(p.scanned_bytes <= SCAN_BYTES);
        assert_eq!(p.events[0].metadata["count"], 259);
        let p2 = store
            .query(QueryFilter::default(), p.next_cursor, Some(200))
            .unwrap();
        assert_eq!(p2.events.len(), 60);
        assert!(!p2.partial);
        assert!(p2.next_cursor.is_none());
        assert_eq!(p2.events[0].metadata["count"], 59);
    }

    #[test]
    fn corrupt_lines_and_utf8_are_reported_and_export_is_sanitized() {
        let temp = Temp::new();
        let store = temp.store();
        store.record(event("a", 1));
        let mut file = OpenOptions::new()
            .append(true)
            .open(store.path(0).unwrap())
            .unwrap();
        file.write_all(b"{broken}\n\xff\xfe\n").unwrap();
        let mut raw = event("a", 2);
        raw.metadata = json!({"prompt": "private body", "userId": "private-user", "count": 2});
        writeln!(file, "{}", serde_json::to_string(&raw).unwrap()).unwrap();
        drop(file);
        let p = page(&store);
        assert_eq!(p.events.len(), 2);
        assert!(p.partial);
        assert!(p.read_errors[0].ends_with("(count=2)"));
        let exported = export_page(p).unwrap();
        assert!(!exported.contains("private"));
        let value: Value = serde_json::from_str(&exported).unwrap();
        assert_eq!(value["coverage"]["partial"], true);
        assert_eq!(value["coverage"]["maxScannedBytes"], SCAN_BYTES);
    }

    #[test]
    fn huge_corrupt_line_drains_with_bounded_reads_and_utf8_prefix_is_never_parsed() {
        let temp = Temp::new();
        let store = temp.store();
        store.record(event("a", 1));
        let mut file = OpenOptions::new()
            .append(true)
            .open(store.path(0).unwrap())
            .unwrap();
        // Non-ASCII bytes deliberately span chunk and page boundaries.
        file.write_all("界".repeat(SCAN_BYTES / 2).as_bytes())
            .unwrap();
        file.write_all(b"\n").unwrap();
        drop(file);
        store.record(event("a", 2));
        let p = page(&store);
        assert_eq!(p.scanned_bytes, SCAN_BYTES);
        assert!(p.partial);
        assert_eq!(p.events.len(), 1);
        assert!(!p.read_errors.is_empty());
        let p2 = store
            .query(QueryFilter::default(), p.next_cursor, Some(200))
            .unwrap();
        assert!(p2.scanned_bytes <= SCAN_BYTES);
        assert_eq!(p2.events.len(), 1);
        assert_eq!(p2.events[0].metadata["count"], 1);
    }

    #[test]
    fn rotation_keeps_five_files_and_identity_cursor_survives_rename() {
        let temp = Temp::new();
        let mut store = temp.store();
        store.max_file_bytes = 800;
        for n in 0..10 {
            store.record(event("a", n));
        }
        let before = store.query(QueryFilter::default(), None, Some(1)).unwrap();
        // Pure rename rotation without deleting an existing snapshot file.
        let temp2 = Temp::new();
        let s2 = temp2.store();
        s2.record(event("a", 1));
        s2.record(event("a", 2));
        let p = s2.query(QueryFilter::default(), None, Some(1)).unwrap();
        {
            let _guard = s2.lock();
            s2.rotate().unwrap();
        }
        let next = s2
            .query(QueryFilter::default(), p.next_cursor, Some(1))
            .unwrap();
        assert_eq!(next.events[0].metadata["count"], 1);
        for n in 10..40 {
            store.record(event("a", n));
        }
        let files: Vec<_> = fs::read_dir(&temp.path).unwrap().collect();
        assert_eq!(files.len(), FILE_COUNT);
        for i in 0..FILE_COUNT {
            assert!(fs::metadata(store.path(i).unwrap()).unwrap().len() <= 800);
        }
        assert!(store
            .query(QueryFilter::default(), before.next_cursor, None)
            .unwrap_err()
            .contains("CURSOR_STALE"));
    }

    #[test]
    fn append_replace_filter_change_and_tampered_cursor_are_stale() {
        let temp = Temp::new();
        let store = temp.store();
        store.record(event("a", 1));
        store.record(event("a", 2));
        let token = store
            .query(QueryFilter::default(), None, Some(1))
            .unwrap()
            .next_cursor
            .unwrap();
        assert!(store
            .query(
                QueryFilter {
                    status: Some("error".into()),
                    ..Default::default()
                },
                Some(token.clone()),
                None
            )
            .is_err());
        assert!(store
            .query(QueryFilter::default(), Some(format!("{token}x")), None)
            .is_err());
        store.record(event("a", 3));
        assert!(store
            .query(QueryFilter::default(), Some(token), None)
            .unwrap_err()
            .contains("CURSOR_STALE"));
        let token = store
            .query(QueryFilter::default(), None, Some(1))
            .unwrap()
            .next_cursor
            .unwrap();
        let path = store.path(0).unwrap();
        fs::remove_file(&path).unwrap();
        fs::write(path, b"{}\n").unwrap();
        assert!(store
            .query(QueryFilter::default(), Some(token), None)
            .is_err());
    }

    #[test]
    fn concurrent_writes_are_complete_and_health_reports_failure_without_recursion() {
        let temp = Temp::new();
        let store = Arc::new(temp.store());
        let mut jobs = Vec::new();
        for i in 0..8 {
            let s = store.clone();
            jobs.push(std::thread::spawn(move || {
                for n in 0..20 {
                    s.record(event(&format!("r{i}"), n));
                }
            }));
        }
        for job in jobs {
            job.join().unwrap();
        }
        let p = page(&store);
        assert_eq!(p.events.len(), 160);
        assert!(p.read_errors.is_empty());
        assert_eq!(p.health.status, "ok");
        assert_eq!(p.health.write_failures, 0);
        let bad = Store::new(Err("DIAGNOSTICS_APPDATA_UNAVAILABLE".into()));
        bad.record(event("a", 0));
        assert_eq!(bad.lock().write_failures, 1);
        assert_eq!(bad.lock().status, "degraded");
        let bad_page = page(&bad);
        assert_eq!(bad_page.health.status, "degraded");
        assert!(bad_page.partial && !bad_page.read_errors.is_empty());
    }

    #[test]
    fn request_normalization_metadata_contract_and_transport_hashes() {
        assert_eq!(
            request_identifier("wecom-preflight-123"),
            request_identifier("wecom-123")
        );
        let value = safe_metadata(
            &json!({
                "model": "workbuddy/deepseek-v4.1-flash", "conditionHash": "a".repeat(64),
                "sessionKey": "private-session", "msgId": "private-msg", "channelGeneration": 3,
                "processId": 12, "errcode": -1, "page": 2, "rows": 40,
                "buildLabel": "report-v2", "selectionState": "applied"
            }),
            0,
            &mut 64,
        );
        assert_eq!(value["model"], "workbuddy/deepseek-v4.1-flash");
        assert_eq!(value["conditionHash"], "a".repeat(64));
        assert_eq!(value["errcode"], -1);
        assert!(!value.to_string().contains("private"));
        for raw in [
            "sk-secret",
            "https://server/model",
            "Bearer secret",
            "enc:v1:secret",
        ] {
            assert_eq!(public_model(raw), "unknown");
        }
        bind_transport("transport-fixture-unique", "wecom-preflight-123");
        assert_eq!(
            transport_request("transport-fixture-unique"),
            Some(request_identifier("wecom-123"))
        );
        assert_eq!(transport_request("missing-transport-fixture"), None);
    }

    #[test]
    fn reads_records_across_chunk_boundaries_without_duplicates() {
        let temp = Temp::new();
        let store = temp.store();
        // A raw unsupported field carries UTF8 across boundaries; projection drops it.
        let mut file = File::create(store.path(0).unwrap()).unwrap();
        for n in 0..190 {
            let mut e = event("a", n);
            e.metadata = json!({"count": n, "body": "界".repeat(300)});
            writeln!(file, "{}", serde_json::to_string(&e).unwrap()).unwrap();
        }
        drop(file);
        let p = page(&store);
        assert_eq!(p.events.len(), 190);
        assert!(p.read_errors.is_empty());
        for (i, e) in p.events.iter().enumerate() {
            assert_eq!(e.metadata["count"], 189 - i);
        }
    }

    #[test]
    fn retention_uses_creation_time_not_append_mtime() {
        let temp = Temp::new();
        let store = temp.store();
        store.record(event("a", 1));
        let meta = fs::metadata(store.path(0).unwrap()).unwrap();
        assert!(!expired(&meta, SystemTime::now()).unwrap());
        assert!(expired(
            &meta,
            SystemTime::now() + RETENTION + Duration::from_secs(1)
        )
        .unwrap());
        let mut old = event("a", 2);
        old.timestamp = (Utc::now() - chrono::Duration::days(8)).to_rfc3339();
        store.record(old);
        assert_eq!(page(&store).events.len(), 1);
    }
}
