use crate::notify::Notifier;
use crate::settings::MinerEvent;
use chrono::{DateTime, FixedOffset, Local, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    #[serde(alias = "DEBUG", alias = "Debug")]
    Debug = 0,
    #[serde(alias = "INFO", alias = "Info")]
    Info = 1,
    #[serde(alias = "WARN", alias = "WARNING", alias = "Warn", alias = "Warning")]
    Warn = 2,
    #[serde(alias = "ERROR", alias = "Error")]
    Error = 3,
}

impl LogLevel {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Debug => "DEBUG",
            Self::Info => "INFO",
            Self::Warn => "WARN",
            Self::Error => "ERROR",
        }
    }
}

impl Default for LogLevel {
    fn default() -> Self {
        Self::Info
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "UPPERCASE")]
pub enum LogColor {
    #[default]
    #[serde(alias = "reset", alias = "Reset")]
    Reset,
    #[serde(alias = "black", alias = "Black")]
    Black,
    #[serde(alias = "red", alias = "Red")]
    Red,
    #[serde(alias = "green", alias = "Green")]
    Green,
    #[serde(alias = "yellow", alias = "Yellow")]
    Yellow,
    #[serde(alias = "blue", alias = "Blue")]
    Blue,
    #[serde(alias = "magenta", alias = "Magenta")]
    Magenta,
    #[serde(alias = "cyan", alias = "Cyan")]
    Cyan,
    #[serde(alias = "white", alias = "White")]
    White,
}

impl LogColor {
    pub fn ansi_prefix(&self) -> &'static str {
        match self {
            Self::Reset => "",
            Self::Black => "\x1b[30m",
            Self::Red => "\x1b[31m",
            Self::Green => "\x1b[32m",
            Self::Yellow => "\x1b[33m",
            Self::Blue => "\x1b[34m",
            Self::Magenta => "\x1b[35m",
            Self::Cyan => "\x1b[36m",
            Self::White => "\x1b[37m",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ColorPalette {
    pub colors: HashMap<MinerEvent, LogColor>,
}

impl Default for ColorPalette {
    fn default() -> Self {
        let mut colors = HashMap::new();
        colors.insert(MinerEvent::BetWin, LogColor::Green);
        colors.insert(MinerEvent::BetLose, LogColor::Red);
        colors.insert(MinerEvent::StreamerOnline, LogColor::Green);
        colors.insert(MinerEvent::StreamerOffline, LogColor::Red);
        Self { colors }
    }
}

impl ColorPalette {
    pub fn get(&self, event: Option<MinerEvent>) -> LogColor {
        event
            .and_then(|ev| self.colors.get(&ev).copied())
            .unwrap_or(LogColor::Reset)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct LoggerSettings {
    pub save: bool,
    pub less: bool,
    pub console_level: LogLevel,
    pub console_username: bool,
    pub auto_clear: bool,
    pub time_zone: String,
    pub file_level: LogLevel,
    pub emoji: bool,
    pub colored: bool,
    pub color_palette: ColorPalette,
}

impl Default for LoggerSettings {
    fn default() -> Self {
        Self {
            save: true,
            less: false,
            console_level: LogLevel::Info,
            console_username: false,
            auto_clear: true,
            time_zone: String::new(),
            file_level: LogLevel::Debug,
            emoji: true,
            colored: true,
            color_palette: ColorPalette::default(),
        }
    }
}

struct RotatingFileState {
    log_path: PathBuf,
    current_date: NaiveDate,
    file: Option<File>,
}

pub struct SessionLogger {
    pub username: String,
    pub settings: LoggerSettings,
    pub notifier: Arc<Notifier>,
    file_state: Option<Mutex<RotatingFileState>>,
}

impl SessionLogger {
    pub fn new(
        username: &str,
        settings: LoggerSettings,
        base_dir: impl AsRef<Path>,
        notifier: Arc<Notifier>,
    ) -> Self {
        let today = current_date_for_tz(&settings.time_zone);
        let file_state = if settings.save {
            let logs_dir = base_dir.as_ref().join("logs");
            let _ = fs::create_dir_all(&logs_dir);
            let safe_name = sanitize_filename(if username.trim().is_empty() {
                "miner"
            } else {
                username
            });
            let log_path = logs_dir.join(format!("{safe_name}.log"));
            // Rotate stale log file from a previous day on startup
            if let Ok(meta) = fs::metadata(&log_path) {
                if meta.len() > 0 {
                    if let Ok(modified) = meta.modified() {
                        let mod_utc: DateTime<Utc> = modified.into();
                        let file_date = date_for_tz(mod_utc, &settings.time_zone);
                        if file_date != today {
                            rotate_log_file(&log_path, file_date);
                        }
                    }
                }
            }
            if settings.auto_clear {
                prune_old_logs(&logs_dir, &safe_name, 7);
            }
            let file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&log_path)
                .ok();
            Some(Mutex::new(RotatingFileState {
                log_path,
                current_date: today,
                file,
            }))
        } else {
            None
        };

        Self {
            username: username.to_string(),
            settings,
            notifier,
            file_state,
        }
    }

    pub fn log_file_path(&self) -> Option<PathBuf> {
        self.file_state
            .as_ref()
            .and_then(|m| m.lock().ok().map(|s| s.log_path.clone()))
    }

    pub fn format_timestamp(&self) -> String {
        let now_utc: DateTime<Utc> = Utc::now();
        if let Some(offset) = parse_timezone_offset(&self.settings.time_zone) {
            now_utc
                .with_timezone(&offset)
                .format("%d/%m/%y %H:%M:%S")
                .to_string()
        } else {
            now_utc
                .with_timezone(&Local)
                .format("%d/%m/%y %H:%M:%S")
                .to_string()
        }
    }

    pub fn log_sync(
        &self,
        level: LogLevel,
        emoji: &str,
        event: Option<MinerEvent>,
        message: &str,
    ) {
        let ts = self.format_timestamp();
        let user_prefix = if self.settings.console_username && !self.username.is_empty() {
            format!("[{}] ", self.username)
        } else {
            String::new()
        };
        let clean_msg = if self.settings.emoji {
            if emoji.is_empty() {
                message.to_string()
            } else {
                format!("{emoji}  {}", message)
            }
        } else {
            strip_non_ascii_symbols(message)
        };

        let plain_line = format!(
            "{} - {} - {}{}",
            ts,
            level.as_str(),
            user_prefix,
            clean_msg
        );

        if level >= self.settings.console_level {
            if self.settings.colored {
                let color = match level {
                    LogLevel::Error => LogColor::Red,
                    LogLevel::Warn => LogColor::Yellow,
                    _ => self.settings.color_palette.get(event),
                };
                let prefix = color.ansi_prefix();
                if prefix.is_empty() {
                    println!("{plain_line}");
                } else {
                    println!("{prefix}{plain_line}\x1b[0m");
                }
            } else {
                println!("{plain_line}");
            }
        }

        if level >= self.settings.file_level {
            self.write_file_line(&plain_line);
        }
    }

    pub async fn emit(
        &self,
        level: LogLevel,
        emoji: &str,
        event: Option<MinerEvent>,
        message: &str,
    ) {
        self.log_sync(level, emoji, event, message);
        if let Some(ev) = event {
            let notif_body = if self.settings.emoji && !emoji.is_empty() {
                format!("{emoji} {message}")
            } else {
                message.to_string()
            };
            self.notifier.notify_event(Some(ev), &notif_body).await;
        }
    }

    pub fn info(&self, emoji: &str, message: &str) {
        self.log_sync(LogLevel::Info, emoji, None, message);
    }

    pub fn debug(&self, message: &str) {
        self.log_sync(LogLevel::Debug, "", None, message);
    }

    pub fn warn(&self, message: &str) {
        self.log_sync(LogLevel::Warn, "⚠️", None, message);
    }

    pub fn error(&self, message: &str) {
        self.log_sync(LogLevel::Error, "❌", None, message);
    }

    fn write_file_line(&self, line: &str) {
        let Some(mutex) = &self.file_state else {
            return;
        };
        let Ok(mut state) = mutex.lock() else {
            return;
        };

        let today = current_date_for_tz(&self.settings.time_zone);
        if today != state.current_date {
            // Rotate current file to `<name>.log.YYYY-MM-DD`
            state.file.take();
            rotate_log_file(&state.log_path, state.current_date);
            state.current_date = today;
            state.file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&state.log_path)
                .ok();
            if self.settings.auto_clear {
                if let Some(parent) = state.log_path.parent() {
                    let safe_name = sanitize_filename(if self.username.trim().is_empty() {
                        "miner"
                    } else {
                        &self.username
                    });
                    prune_old_logs(parent, &safe_name, 7);
                }
            }
        }

        if let Some(file) = state.file.as_mut() {
            let _ = writeln!(file, "{line}");
            let _ = file.flush();
        }
    }
}

pub fn sanitize_filename(value: &str) -> String {
    value
        .to_lowercase()
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

fn date_for_tz(ts: DateTime<Utc>, tz: &str) -> NaiveDate {
    if let Some(offset) = parse_timezone_offset(tz) {
        ts.with_timezone(&offset).date_naive()
    } else {
        ts.with_timezone(&Local).date_naive()
    }
}

fn current_date_for_tz(tz: &str) -> NaiveDate {
    date_for_tz(Utc::now(), tz)
}

fn rotate_log_file(log_path: &Path, file_date: NaiveDate) {
    let rotated_name = format!(
        "{}.{}",
        log_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("miner.log"),
        file_date.format("%Y-%m-%d")
    );
    let rotated_path = log_path.with_file_name(rotated_name);
    if rotated_path.exists() {
        if let Ok(existing) = fs::read(log_path) {
            if let Ok(mut dst) = OpenOptions::new().append(true).open(&rotated_path) {
                let _ = dst.write_all(&existing);
            }
            let _ = fs::remove_file(log_path);
        }
    } else {
        let _ = fs::rename(log_path, &rotated_path);
    }
}

fn prune_old_logs(dir: &Path, safe_username: &str, keep_backups: usize) {
    let prefix = format!("{safe_username}.log.");
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut backups: Vec<PathBuf> = entries
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            if name.starts_with(&prefix) {
                Some(e.path())
            } else {
                None
            }
        })
        .collect();
    backups.sort();
    if backups.len() > keep_backups {
        let remove_count = backups.len() - keep_backups;
        for old in backups.into_iter().take(remove_count) {
            let _ = fs::remove_file(old);
        }
    }
}

fn strip_non_ascii_symbols(input: &str) -> String {
    input
        .chars()
        .filter(|ch| {
            // Keep ASCII, Cyrillic, Latin, punctuation, whitespace; drop emoji codepoints
            let code = *ch as u32;
            !(0x1F000..=0x1FAFF).contains(&code)
                && !(0x2600..=0x27BF).contains(&code)
                && code != 0xFE0F
        })
        .collect::<String>()
        .trim_start()
        .to_string()
}

/// Parse UTC offset strings (`+03:00`, `-07:00`, `UTC`, `UTC+3`) and common IANA timezone names.
pub fn parse_timezone_offset(tz: &str) -> Option<FixedOffset> {
    let tz = tz.trim();
    if tz.is_empty() {
        return None;
    }
    let upper = tz.to_ascii_uppercase();
    if upper == "UTC" || upper == "GMT" || upper == "Z" || upper == "ETC/UTC" {
        return FixedOffset::east_opt(0);
    }

    let stripped = upper
        .strip_prefix("UTC")
        .or_else(|| upper.strip_prefix("GMT"))
        .unwrap_or(tz.trim());

    if stripped.starts_with('+') || stripped.starts_with('-') {
        let sign: i32 = if stripped.starts_with('-') { -1 } else { 1 };
        let rest = &stripped[1..];
        let mut parts = rest.split(':');
        if let Some(h_str) = parts.next() {
            if let Ok(hours) = h_str.parse::<i32>() {
                let mins = parts
                    .next()
                    .and_then(|m| m.parse::<i32>().ok())
                    .unwrap_or(0);
                let total_secs = sign * (hours * 3600 + mins * 60);
                return FixedOffset::east_opt(total_secs);
            }
        }
    }

    // Common IANA zones -> standard UTC offsets
    let hours_offset = match tz {
        "Europe/London" | "Europe/Lisbon" => Some(0),
        "Europe/Berlin" | "Europe/Paris" | "Europe/Rome" | "Europe/Madrid" | "Europe/Warsaw"
        | "Europe/Prague" | "Europe/Vienna" | "Europe/Amsterdam" => Some(1),
        "Europe/Kyiv" | "Europe/Kiev" | "Europe/Helsinki" | "Europe/Bucharest" | "Europe/Athens"
        | "Europe/Riga" | "Europe/Vilnius" | "Europe/Tallinn" | "Europe/Kaliningrad" => Some(2),
        "Europe/Moscow" | "Europe/Minsk" | "Europe/Istanbul" => Some(3),
        "Europe/Samara" | "Asia/Tbilisi" | "Asia/Yerevan" | "Asia/Baku" | "Asia/Dubai" => Some(4),
        "Asia/Yekaterinburg" | "Asia/Tashkent" | "Asia/Almaty" => Some(5),
        "Asia/Omsk" | "Asia/Bishkek" => Some(6),
        "Asia/Novosibirsk" | "Asia/Krasnoyarsk" | "Asia/Bangkok" => Some(7),
        "Asia/Irkutsk" | "Asia/Shanghai" | "Asia/Singapore" | "Asia/Hong_Kong" => Some(8),
        "Asia/Yakutsk" | "Asia/Tokyo" | "Asia/Seoul" => Some(9),
        "Asia/Vladivostok" | "Australia/Sydney" => Some(10),
        "America/New_York" | "America/Toronto" | "US/Eastern" => Some(-5),
        "America/Chicago" | "US/Central" => Some(-6),
        "America/Denver" | "US/Mountain" => Some(-7),
        "America/Los_Angeles" | "America/Vancouver" | "US/Pacific" => Some(-8),
        "America/Sao_Paulo" => Some(-3),
        _ => None,
    };

    hours_offset.and_then(|h| FixedOffset::east_opt(h * 3600))
}
