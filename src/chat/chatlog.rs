//! Opt-in on-disk chat logging, one JSON Lines file per session.
//!
//! Ported from `yc` (commit 9e67efd10c0790ec22df2c944bcee6be1bc37cf8,
//! internal/chatlog: doc.go, event.go, writer.go; internal/cli/export.go for
//! the Super Chat CSV export). `twi` (commit
//! 7c6ad6bbbc3dec1b6af2ddbd03b55f96c0c162cf) has no chat log; `yc` is the
//! single behavioral authority.
//!
//! Every line is one JSON object describing one normalized event, so the file
//! can be processed with standard tools (jq, a spreadsheet import, the Super
//! Chat export) without a custom parser. Only the normalized [`ChatMessage`]
//! model reaches disk — raw platform JSON never does.
//!
//! Files are written with owner-only permissions (0600 in a 0700 directory on
//! Unix): a chat log records what the user was watching and who said what,
//! which is private even though it is not credential material.
//!
//! The log is strictly append-only. A message that is later deleted by
//! moderation was logged when it arrived, with the text as it was RECEIVED;
//! the log is the record of what happened and is never rewritten.
//!
//! Deviations from the reference, both required by this port's contract:
//! * File names use a Unix timestamp (`chat-<unix-ts>.jsonl`, rotations
//!   `chat-<unix-ts>-<n>.jsonl`) instead of yc's `chat-YYYYMMDD-HHMMSS`.
//! * The first write error latches the logger into a no-op instead of
//!   returning the error per append: this app's UI has nowhere useful to put
//!   a per-message disk error, and a broken disk must never cost the chat
//!   session. The failure is logged exactly once.

use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

use anyhow::Context as _;
use chrono::{SecondsFormat, Utc};
use serde::{Deserialize, Serialize};

use crate::chat::{ChatMessage, MessageKind, PlatformMeta};

/// The prefix + suffix mark the files this logger owns. Pruning only ever
/// touches files carrying both, so pointing the log at a populated directory
/// cannot delete someone else's data.
const FILE_PREFIX: &str = "chat-";
const FILE_SUFFIX: &str = ".jsonl";

/// One logged chat event, flattened for line-oriented consumers.
///
/// The JSON field names are the log's file format: changing one is a breaking
/// change for anything that reads existing logs, including the Super Chat
/// export. Optional fields are skipped when empty so an ordinary text message
/// stays a short line.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Record {
    /// The event's own time (when the message was published) in RFC 3339,
    /// not when it was written to the log.
    pub ts: String,
    /// The identifier the app routes this chat under (live chat id, video
    /// id, channel login, …).
    pub chat_id: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub message_id: String,
    /// The fine-grained event kind: YouTube's wire `snippet.type`, Twitch's
    /// USERNOTICE `msg-id`, or the coarse class when the platform gave no
    /// finer name.
    pub kind: String,
    /// The coarse render class ("chat", "paid", "membership", …).
    #[serde(rename = "type")]
    pub kind_class: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub author_channel_id: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub author: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub text: String,
    /// A paid event's value in millionths of the currency unit, exactly as
    /// the API supplies it — never a float, so no rounding can occur between
    /// the wire and the ledger export.
    #[serde(skip_serializing_if = "is_zero_i64")]
    pub amount_micros: i64,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub currency: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub amount_display: String,
    /// YouTube's 1–11 purchase tier, or 0 when absent.
    #[serde(skip_serializing_if = "is_zero_u8")]
    pub tier: u8,
    /// A row rendered from our own send.
    #[serde(skip_serializing_if = "is_false")]
    pub local_echo: bool,
    /// A row from the priming backlog rather than the live tail.
    #[serde(skip_serializing_if = "is_false")]
    pub historical: bool,
}

fn is_zero_i64(v: &i64) -> bool {
    *v == 0
}
fn is_zero_u8(v: &u8) -> bool {
    *v == 0
}
fn is_false(v: &bool) -> bool {
    !*v
}

impl Record {
    /// Flattens a normalized message into its logged form (yc: FromMessage).
    pub fn from_message(chat_id: &str, msg: &ChatMessage) -> Self {
        let kind_class = match msg.kind {
            MessageKind::Chat => "chat",
            MessageKind::Action => "action",
            MessageKind::Notice => "notice",
            MessageKind::Paid => "paid",
            MessageKind::Membership => "membership",
            MessageKind::Unknown => "unknown",
        };
        // The finest name the platform gave us; the coarse class otherwise.
        let kind = match &msg.meta {
            Some(PlatformMeta::YouTube(meta)) if !meta.raw_type.is_empty() => meta.raw_type.clone(),
            Some(PlatformMeta::Twitch(meta)) if !meta.system_event.is_empty() => {
                meta.system_event.clone()
            }
            _ => kind_class.to_string(),
        };
        let mut record = Self {
            // A message with no platform timestamp is stamped at log time:
            // an empty ts would make the line useless as a record.
            ts: msg
                .timestamp
                .unwrap_or_else(Utc::now)
                .to_rfc3339_opts(SecondsFormat::Secs, true),
            chat_id: chat_id.to_string(),
            message_id: msg.id.clone(),
            kind,
            kind_class: kind_class.to_string(),
            author_channel_id: msg.author.id.clone(),
            author: msg.author.display_name.clone(),
            // Logged as RECEIVED even when `deleted` is already set: the
            // append-only log is the record of what was said; the UI's
            // never-reprint rule is a rendering rule, not a storage one.
            text: msg.text.clone(),
            local_echo: msg.local_echo,
            historical: msg.historical,
            ..Self::default()
        };
        if let Some(PlatformMeta::YouTube(meta)) = &msg.meta {
            if let Some(paid) = &meta.paid {
                record.amount_micros = paid.micros;
                record.currency = paid.currency.clone();
                record.amount_display = paid.display.clone();
                record.tier = paid.tier;
            }
        }
        record
    }
}

/// Appends normalized chat events to a session's JSONL file, rotating by
/// size and pruning old sessions.
///
/// Failure-tolerant by contract: the first write error latches the logger
/// into a permanent no-op (logged once). Nothing here can end a chat session.
pub struct ChatLogger {
    dir: PathBuf,
    max_bytes: u64,
    max_files: usize,
    file: Option<File>,
    size: u64,
    /// The session's Unix timestamp, fixed when the first file is created so
    /// every rotation of one session shares a base name.
    session_ts: i64,
    /// Rotation counter: 0 is `chat-<ts>.jsonl`, n>0 is `chat-<ts>-<n>.jsonl`.
    seq: u32,
    /// Set by the first write error; every later append is a no-op.
    failed: bool,
}

impl ChatLogger {
    /// No file is created until the first append, so enabling logging costs
    /// nothing while chat is silent.
    pub fn new(dir: impl Into<PathBuf>, max_bytes: u64, max_files: usize) -> Self {
        Self {
            dir: dir.into(),
            max_bytes: max_bytes.max(1),
            max_files: max_files.max(1),
            file: None,
            size: 0,
            session_ts: 0,
            seq: 0,
            failed: false,
        }
    }

    /// Writes one message as one JSONL line, rotating first when the write
    /// would leave the current file over budget.
    pub fn append(&mut self, chat_id: &str, msg: &ChatMessage) {
        if self.failed {
            return;
        }
        if let Err(err) = self.try_append(chat_id, msg) {
            // Latch: log the failure exactly once, then go silent. A broken
            // disk must never cost the chat session, and repeating the same
            // warning per message would drown the log.
            tracing::warn!("chat logging disabled after a write error: {err:#}");
            self.failed = true;
            self.file = None;
        }
    }

    fn try_append(&mut self, chat_id: &str, msg: &ChatMessage) -> anyhow::Result<()> {
        let record = Record::from_message(chat_id, msg);
        let mut line =
            serde_json::to_vec(&record).context("could not encode a chat log record as JSON")?;
        line.push(b'\n');
        self.ensure_file(line.len() as u64)?;
        // Invariant: ensure_file either returns an error or leaves an open file.
        let file = self
            .file
            .as_mut()
            .expect("ensure_file left an open log file");
        file.write_all(&line)
            .context("could not append to the chat log")?;
        self.size += line.len() as u64;
        Ok(())
    }

    /// Opens the session file on first use and rotates when the pending
    /// write would leave the current file over budget.
    fn ensure_file(&mut self, pending: u64) -> anyhow::Result<()> {
        if self.file.is_some() && self.size + pending > self.max_bytes {
            // The current file is over budget: start a fresh one. The old
            // handle just drops closed; append-only files need no flush
            // ceremony beyond write_all.
            self.file = None;
            self.seq += 1;
        }
        if self.file.is_some() {
            return Ok(());
        }

        create_private_dir(&self.dir).with_context(|| {
            format!(
                "could not create the chat log directory {}",
                self.dir.display()
            )
        })?;
        if self.session_ts == 0 {
            self.session_ts = Utc::now().timestamp();
        }
        // A numeric suffix disambiguates rotations and collisions (two
        // sessions started within the same second look identical otherwise).
        for _ in 0..1000 {
            let name = if self.seq == 0 {
                format!("{FILE_PREFIX}{}{FILE_SUFFIX}", self.session_ts)
            } else {
                format!("{FILE_PREFIX}{}-{}{FILE_SUFFIX}", self.session_ts, self.seq)
            };
            let path = self.dir.join(name);
            match open_private_create_new(&path) {
                Ok(file) => {
                    self.file = Some(file);
                    self.size = 0;
                    self.prune();
                    return Ok(());
                }
                Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                    self.seq += 1;
                }
                Err(err) => {
                    return Err(err).with_context(|| {
                        format!("could not create the chat log file {}", path.display())
                    });
                }
            }
        }
        anyhow::bail!(
            "could not find an unused chat log file name in {}",
            self.dir.display()
        );
    }

    /// Deletes the oldest owned files beyond the retention count. Only files
    /// matching `chat-*.jsonl` are candidates — pointing the log at a
    /// populated directory must never delete someone else's data. Prune
    /// failures are ignored: retention is housekeeping, and a log that grows
    /// one file too many is better than an append that fails over it.
    fn prune(&self) {
        let owned = list_log_files(&self.dir);
        if owned.len() <= self.max_files {
            return;
        }
        for path in &owned[..owned.len() - self.max_files] {
            let _ = fs::remove_file(path);
        }
    }
}

/// Creates the log directory owner-only (0700 on Unix; plain elsewhere).
fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt as _;
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)
    }
    #[cfg(not(unix))]
    {
        fs::create_dir_all(dir)
    }
}

/// Creates a new log file owner-only (0600 on Unix), failing if it exists.
fn open_private_create_new(path: &Path) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    options.open(path)
}

/// The chat log files under `dir`, oldest first (by the timestamp and
/// rotation number embedded in the name, so `chat-5-1.jsonl` sorts after
/// `chat-5.jsonl` even though it does not lexically).
fn list_log_files(dir: &Path) -> Vec<PathBuf> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return Vec::new(),
    };
    let mut owned: Vec<(u64, u32, PathBuf)> = Vec::new();
    for entry in entries.flatten() {
        if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
            continue;
        }
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let Some(stem) = name
            .strip_prefix(FILE_PREFIX)
            .and_then(|s| s.strip_suffix(FILE_SUFFIX))
        else {
            continue;
        };
        // Age key: `<ts>` or `<ts>-<n>`. A name someone hand-crafted that
        // matches the pattern but not the shape still counts as owned (it
        // matches `chat-*.jsonl`); it just sorts as oldest.
        let (ts, seq) = match stem.split_once('-') {
            Some((ts, seq)) => (
                ts.parse::<u64>().unwrap_or(0),
                seq.parse::<u32>().unwrap_or(0),
            ),
            None => (stem.parse::<u64>().unwrap_or(0), 0),
        };
        owned.push((ts, seq, entry.path()));
    }
    owned.sort();
    owned.into_iter().map(|(_, _, path)| path).collect()
}

/// Reads every chat log under `log_dir` and writes one CSV row per paid
/// record (`amount_micros != 0`), returning the row count.
///
/// Ported from yc internal/cli/export.go (exportSuperchatsCSV). Malformed
/// lines are skipped rather than failing the export: a log cut off mid-line
/// by a crash still has every complete record before it, and those records
/// are the reason the log exists. Echo and backlog rows are included, because
/// money was paid whether or not this app watched it live. An empty directory
/// yields a header-only CSV, not an error: "no Super Chats" is an answer.
pub fn export_superchats(log_dir: &Path, out: &mut impl Write) -> anyhow::Result<usize> {
    writeln!(
        out,
        "timestamp,chat_id,author,amount_value,currency,tier,message"
    )
    .context("could not write the Super Chat CSV header")?;
    let mut rows = 0usize;
    for path in list_log_files(log_dir) {
        let file = match File::open(&path) {
            Ok(file) => file,
            // A file that vanished between listing and opening (rotation
            // pruned it) is not a reason to abandon the rest.
            Err(_) => continue,
        };
        let reader = std::io::BufReader::new(file);
        for line in reader.lines() {
            // An unreadable or malformed line (a truncated tail) ends
            // nothing but itself.
            let Ok(line) = line else { break };
            let Ok(record) = serde_json::from_str::<Record>(&line) else {
                continue;
            };
            if record.amount_micros == 0 {
                continue;
            }
            let row = [
                record.ts.as_str(),
                record.chat_id.as_str(),
                record.author.as_str(),
                &format_micros(record.amount_micros),
                record.currency.as_str(),
                &record.tier.to_string(),
                record.text.as_str(),
            ]
            .iter()
            .map(|field| csv_escape(field))
            .collect::<Vec<_>>()
            .join(",");
            writeln!(out, "{row}").with_context(|| {
                format!(
                    "could not write a Super Chat CSV row from {}",
                    path.display()
                )
            })?;
            rows += 1;
        }
    }
    Ok(rows)
}

/// Renders millionths of a currency unit as a decimal with at least two
/// places, trimming only trailing sub-cent zeros ("5000000" → "5.00",
/// "1990000" → "1.99", "1234567" → "1.234567"). Integer arithmetic
/// throughout: a float would round someone's money (yc: formatMicros).
fn format_micros(micros: i64) -> String {
    let negative = micros < 0;
    let magnitude = micros.unsigned_abs();
    let whole = magnitude / 1_000_000;
    let fraction = magnitude % 1_000_000;
    let mut text = format!("{whole}.{fraction:06}");
    // Keep at least two decimal places, then drop trailing zeros beyond them.
    while text.ends_with('0') {
        let dot = text.find('.').unwrap_or(0);
        if text.len() - dot - 1 <= 2 {
            break;
        }
        text.pop();
    }
    if negative {
        format!("-{text}")
    } else {
        text
    }
}

/// Quotes a CSV field when it contains a comma, quote, or line break, and
/// doubles embedded quotes (RFC 4180).
fn csv_escape(field: &str) -> String {
    if field.contains(['"', ',', '\n', '\r']) {
        format!("\"{}\"", field.replace('"', "\"\""))
    } else {
        field.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::{ChatAuthor, PaidAmount, YouTubeMeta};
    use chrono::TimeZone as _;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "msm-chatlog-{tag}-{}-{}",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap_or(0)
        ));
        fs::create_dir_all(&dir).expect("test temp dir");
        dir
    }

    fn chat_message(id: &str, text: &str) -> ChatMessage {
        ChatMessage {
            id: id.into(),
            timestamp: Some(Utc.with_ymd_and_hms(2026, 1, 2, 3, 4, 5).unwrap()),
            author: ChatAuthor {
                id: "UC123".into(),
                display_name: "Alice".into(),
                ..Default::default()
            },
            text: text.into(),
            kind: MessageKind::Chat,
            deleted: false,
            historical: false,
            local_echo: false,
            meta: None,
        }
    }

    fn paid_message(id: &str, micros: i64, text: &str) -> ChatMessage {
        let mut msg = chat_message(id, text);
        msg.kind = MessageKind::Paid;
        msg.meta = Some(PlatformMeta::YouTube(YouTubeMeta {
            raw_type: "superChatEvent".into(),
            paid: Some(PaidAmount {
                micros,
                currency: "USD".into(),
                display: "$5.00".into(),
                tier: 3,
            }),
            membership: None,
        }));
        msg
    }

    #[test]
    fn creation_is_lazy_and_append_round_trips() {
        let dir = temp_dir("roundtrip");
        let log_dir = dir.join("logs");
        let mut logger = ChatLogger::new(&log_dir, 1024 * 1024, 3);
        // No append yet: no directory, no file.
        assert!(!log_dir.exists(), "logger must not touch disk before use");

        logger.append("chat-a", &chat_message("m1", "hello world"));
        logger.append("chat-a", &paid_message("m2", 5_000_000, "thanks!"));

        let files = list_log_files(&log_dir);
        assert_eq!(files.len(), 1);
        let content = fs::read_to_string(&files[0]).expect("read log");
        let lines: Vec<Record> = content
            .lines()
            .map(|l| serde_json::from_str(l).expect("valid record"))
            .collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].chat_id, "chat-a");
        assert_eq!(lines[0].message_id, "m1");
        assert_eq!(lines[0].kind_class, "chat");
        assert_eq!(lines[0].text, "hello world");
        assert_eq!(lines[0].ts, "2026-01-02T03:04:05Z");
        assert_eq!(lines[1].kind, "superChatEvent");
        assert_eq!(lines[1].kind_class, "paid");
        assert_eq!(lines[1].amount_micros, 5_000_000);
        assert_eq!(lines[1].currency, "USD");
        assert_eq!(lines[1].tier, 3);

        // The wire format keeps the documented field names.
        let raw: serde_json::Value = serde_json::from_str(content.lines().nth(1).unwrap()).unwrap();
        assert!(
            raw.get("type").is_some(),
            "coarse class serializes as `type`"
        );
        assert!(raw.get("amount_micros").is_some());

        fs::remove_dir_all(&dir).ok();
    }

    #[cfg(unix)]
    #[test]
    fn files_and_dir_are_owner_only() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = temp_dir("perms");
        let log_dir = dir.join("logs");
        let mut logger = ChatLogger::new(&log_dir, 1024, 3);
        logger.append("c", &chat_message("m1", "x"));
        let dir_mode = fs::metadata(&log_dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(dir_mode, 0o700);
        let file = &list_log_files(&log_dir)[0];
        let file_mode = fs::metadata(file).unwrap().permissions().mode() & 0o777;
        assert_eq!(file_mode, 0o600);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn rotates_when_a_write_would_exceed_max_bytes() {
        let dir = temp_dir("rotate");
        let log_dir = dir.join("logs");
        // Small enough that every message overflows the previous file.
        let mut logger = ChatLogger::new(&log_dir, 200, 10);
        for i in 0..3 {
            logger.append("c", &chat_message(&format!("m{i}"), "some message text"));
        }
        let files = list_log_files(&log_dir);
        assert!(
            files.len() >= 2,
            "expected a rotation, got {} file(s)",
            files.len()
        );
        // Rotated names carry the -<n> suffix of the same session.
        let last = files.last().unwrap().file_name().unwrap().to_str().unwrap();
        assert!(last.starts_with("chat-") && last.ends_with(".jsonl"));
        assert!(
            last.trim_end_matches(".jsonl").contains('-'),
            "rotation file should be chat-<ts>-<n>.jsonl, got {last}"
        );
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn prunes_only_owned_files_down_to_max_files() {
        let dir = temp_dir("prune");
        let log_dir = dir.join("logs");
        fs::create_dir_all(&log_dir).unwrap();
        // Pre-existing owned files (old sessions) and a stranger's file.
        for ts in [100, 200, 300] {
            fs::write(log_dir.join(format!("chat-{ts}.jsonl")), "{}\n").unwrap();
        }
        fs::write(log_dir.join("notes.jsonl"), "keep me\n").unwrap();
        fs::write(log_dir.join("chat-precious.txt"), "keep me too\n").unwrap();

        let mut logger = ChatLogger::new(&log_dir, 1024 * 1024, 2);
        logger.append("c", &chat_message("m1", "hi"));

        let owned = list_log_files(&log_dir);
        assert_eq!(owned.len(), 2, "kept newest {} owned files", 2);
        // The oldest sessions were pruned; the newest session file survives.
        assert!(!log_dir.join("chat-100.jsonl").exists());
        assert!(!log_dir.join("chat-200.jsonl").exists());
        // Files that do not match chat-*.jsonl are never touched.
        assert!(log_dir.join("notes.jsonl").exists());
        assert!(log_dir.join("chat-precious.txt").exists());
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn first_write_error_latches_into_a_no_op() {
        let dir = temp_dir("latch");
        // The "directory" is a file, so creating the log dir must fail.
        let blocker = dir.join("blocked");
        fs::write(&blocker, "not a directory").unwrap();
        let mut logger = ChatLogger::new(&blocker, 1024, 3);
        logger.append("c", &chat_message("m1", "hi"));
        assert!(logger.failed, "the first error must latch");
        // Later appends are no-ops and must not panic or write anywhere.
        logger.append("c", &chat_message("m2", "hi again"));
        assert_eq!(fs::read_to_string(&blocker).unwrap(), "not a directory");
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn export_emits_paid_rows_with_escaping_and_integer_amounts() {
        let dir = temp_dir("export");
        let log_dir = dir.join("logs");
        let mut logger = ChatLogger::new(&log_dir, 1024 * 1024, 5);
        logger.append("chat-a", &chat_message("m1", "free text, no money"));
        logger.append("chat-a", &paid_message("m2", 5_000_000, "plain"));
        // Text that needs CSV escaping: comma, quote, newline.
        let mut tricky = paid_message("m3", 1_234_567, "hi, \"friend\"\nnew line");
        tricky.author.display_name = "Bob, the \"Builder\"".into();
        logger.append("chat-a", &tricky);
        logger.append("chat-a", &paid_message("m4", 1_990_000, ""));

        let mut out = Vec::new();
        let rows = export_superchats(&log_dir, &mut out).expect("export");
        assert_eq!(rows, 3);
        let csv = String::from_utf8(out).unwrap();
        let mut lines = csv.lines();
        assert_eq!(
            lines.next().unwrap(),
            "timestamp,chat_id,author,amount_value,currency,tier,message"
        );
        assert_eq!(
            lines.next().unwrap(),
            "2026-01-02T03:04:05Z,chat-a,Alice,5.00,USD,3,plain"
        );
        // Escaped row: quoted fields with doubled quotes; the newline keeps
        // the record on two physical lines inside one quoted field.
        assert_eq!(
            lines.next().unwrap(),
            "2026-01-02T03:04:05Z,chat-a,\"Bob, the \"\"Builder\"\"\",1.234567,USD,3,\"hi, \"\"friend\"\""
        );
        assert_eq!(lines.next().unwrap(), "new line\"");
        assert_eq!(
            lines.next().unwrap(),
            "2026-01-02T03:04:05Z,chat-a,Alice,1.99,USD,3,"
        );
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn export_skips_a_malformed_tail() {
        let dir = temp_dir("malformed");
        let log_dir = dir.join("logs");
        let mut logger = ChatLogger::new(&log_dir, 1024 * 1024, 5);
        logger.append("c", &paid_message("m1", 2_000_000, "good"));
        // Simulate a crash mid-write: a truncated JSON line at the tail.
        let file = &list_log_files(&log_dir)[0];
        let mut handle = OpenOptions::new().append(true).open(file).unwrap();
        handle
            .write_all(b"{\"ts\":\"2026-01-02T03:04:05Z\",\"amount_mic")
            .unwrap();
        drop(handle);

        let mut out = Vec::new();
        let rows = export_superchats(&log_dir, &mut out).expect("export survives truncation");
        assert_eq!(rows, 1, "the complete record before the tail still counts");
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn export_of_empty_dir_is_header_only() {
        let dir = temp_dir("empty");
        let mut out = Vec::new();
        let rows = export_superchats(&dir, &mut out).expect("empty is an answer");
        assert_eq!(rows, 0);
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "timestamp,chat_id,author,amount_value,currency,tier,message\n"
        );
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn deleted_message_text_is_logged_as_received() {
        let dir = temp_dir("deleted");
        let log_dir = dir.join("logs");
        let mut logger = ChatLogger::new(&log_dir, 1024 * 1024, 5);
        let mut msg = chat_message("m1", "the original words");
        msg.deleted = true;
        logger.append("c", &msg);
        let content = fs::read_to_string(&list_log_files(&log_dir)[0]).unwrap();
        assert!(
            content.contains("the original words"),
            "the log is append-only and records what was received"
        );
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn format_micros_matches_the_reference() {
        assert_eq!(format_micros(5_000_000), "5.00");
        assert_eq!(format_micros(1_990_000), "1.99");
        assert_eq!(format_micros(1_234_567), "1.234567");
        assert_eq!(format_micros(0), "0.00");
        assert_eq!(format_micros(-2_500_000), "-2.50");
        assert_eq!(format_micros(100), "0.0001");
        // i64::MIN must not overflow on negation.
        assert!(format_micros(i64::MIN).starts_with("-9223372036854"));
    }

    #[test]
    fn log_files_sort_rotations_after_their_base() {
        let dir = temp_dir("sort");
        for name in [
            "chat-5.jsonl",
            "chat-5-1.jsonl",
            "chat-5-2.jsonl",
            "chat-6.jsonl",
        ] {
            fs::write(dir.join(name), "{}\n").unwrap();
        }
        let names: Vec<String> = list_log_files(&dir)
            .into_iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            names,
            [
                "chat-5.jsonl",
                "chat-5-1.jsonl",
                "chat-5-2.jsonl",
                "chat-6.jsonl"
            ]
        );
        fs::remove_dir_all(&dir).ok();
    }
}
