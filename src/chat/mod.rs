//! IRC chat presence for watch-time / StreamElements points and `@mention` notifications
use crate::constants::{IRC_HOST, IRC_PORT};
use crate::error::{MinerError, Result};
use crate::logger::{LogLevel, SessionLogger};
use crate::settings::MinerEvent;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tracing::{debug, info, warn};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChatPresence {
    /// Never join chat
    #[serde(alias = "NEVER", alias = "Never")]
    Never,
    /// Join only when stream is online
    #[serde(alias = "ONLINE", alias = "Online")]
    Online,
    /// Join only when stream is offline
    #[serde(alias = "OFFLINE", alias = "Offline")]
    Offline,
    /// Always stay in chat
    #[serde(alias = "ALWAYS", alias = "Always")]
    Always,
}

impl Default for ChatPresence {
    fn default() -> Self {
        Self::Online
    }
}

pub struct ChatClient {
    username: String,
    token: String,
    channel: String,
    disable_at_in_nickname: bool,
    logger: Option<Arc<SessionLogger>>,
    running: Arc<AtomicBool>,
}

impl ChatClient {
    pub fn new(
        username: &str,
        token: &str,
        channel: &str,
        disable_at_in_nickname: bool,
        logger: Option<Arc<SessionLogger>>,
    ) -> Self {
        Self {
            username: username.to_lowercase(),
            token: if token.starts_with("oauth:") {
                token.to_string()
            } else {
                format!("oauth:{}", token)
            },
            channel: channel.to_lowercase(),
            disable_at_in_nickname,
            logger,
            running: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn stop(&self) {
        self.running.store(false, Ordering::Relaxed);
    }

    pub async fn run(&self) -> Result<()> {
        self.running.store(true, Ordering::Relaxed);
        if let Some(log) = &self.logger {
            log.info("💬", &format!("Join IRC Chat: #{}", self.channel));
        } else {
            info!("IRC: joining #{}", self.channel);
        }

        while self.running.load(Ordering::Relaxed) {
            match self.connect_once().await {
                Ok(_) => {
                    if !self.running.load(Ordering::Relaxed) {
                        break;
                    }
                    warn!(
                        "IRC disconnected from #{}, reconnecting in 10s...",
                        self.channel
                    );
                }
                Err(e) => {
                    if !self.running.load(Ordering::Relaxed) {
                        break;
                    }
                    warn!("IRC error on #{}: {}, retry in 15s", self.channel, e);
                }
            }
            tokio::time::sleep(std::time::Duration::from_secs(15)).await;
        }
        Ok(())
    }

    async fn connect_once(&self) -> Result<()> {
        let addr = format!("{IRC_HOST}:{IRC_PORT}");
        let stream = TcpStream::connect(&addr)
            .await
            .map_err(|e| MinerError::Other(format!("IRC connect: {}", e)))?;

        let (reader, mut writer) = stream.into_split();
        let mut reader = BufReader::new(reader);

        writer
            .write_all(format!("PASS {}\r\n", self.token).as_bytes())
            .await?;
        writer
            .write_all(format!("NICK {}\r\n", self.username).as_bytes())
            .await?;
        writer
            .write_all(b"CAP REQ :twitch.tv/tags twitch.tv/commands\r\n")
            .await?;
        writer
            .write_all(format!("JOIN #{}\r\n", self.channel).as_bytes())
            .await?;
        writer.flush().await?;

        let needle = if self.disable_at_in_nickname {
            self.username.clone()
        } else {
            format!("@{}", self.username)
        };

        let mut line = String::new();
        loop {
            if !self.running.load(Ordering::Relaxed) {
                let _ = writer.write_all(b"QUIT :Bye, cruel world!\r\n").await;
                break;
            }

            line.clear();
            let n = tokio::time::timeout(
                std::time::Duration::from_secs(240),
                reader.read_line(&mut line),
            )
            .await
            .map_err(|_| MinerError::Other("IRC read timeout".into()))?
            .map_err(|e| MinerError::Other(format!("IRC read: {}", e)))?;

            if n == 0 {
                break;
            }

            let raw = line.trim_end();
            if raw.starts_with("PING") {
                let pong = raw.replacen("PING", "PONG", 1);
                writer.write_all(format!("{}\r\n", pong).as_bytes()).await?;
                writer.flush().await?;
                debug!("IRC PONG on #{}", self.channel);
            } else if let Some((sender_nick, chat_text)) = parse_irc_privmsg(raw) {
                if !sender_nick.eq_ignore_ascii_case(&self.username)
                    && !needle.is_empty()
                    && chat_text.to_lowercase().contains(&needle)
                {
                    let formatted = format!(
                        "{} at #{} wrote: {}",
                        sender_nick, self.channel, chat_text
                    );
                    if let Some(log) = &self.logger {
                        log.emit(
                            LogLevel::Info,
                            "💬",
                            Some(MinerEvent::ChatMention),
                            &formatted,
                        )
                        .await;
                    } else {
                        info!("💬 {formatted}");
                    }
                }
            }
        }
        Ok(())
    }
}

/// Extract `(sender_nick, message_body)` from a Twitch IRC `PRIVMSG` line (with or without IRCv3 tags).
fn parse_irc_privmsg(raw: &str) -> Option<(&str, &str)> {
    let rest = if raw.starts_with('@') {
        let space = raw.find(' ')?;
        raw[space + 1..].trim_start()
    } else {
        raw
    };

    let rest = rest.strip_prefix(':')?;
    let privmsg_idx = rest.find(" PRIVMSG ")?;
    let prefix = &rest[..privmsg_idx];
    let nick = prefix.split('!').next().unwrap_or(prefix);

    let after_cmd = &rest[privmsg_idx + " PRIVMSG ".len()..];
    let colon_idx = after_cmd.find(" :")?;
    let body = &after_cmd[colon_idx + 2..];
    Some((nick, body))
}

/// Manage multiple IRC chat connections
pub struct ChatPool {
    username: String,
    token: String,
    disable_at_in_nickname: bool,
    logger: Option<Arc<SessionLogger>>,
    handles: Mutex<Vec<(String, Arc<AtomicBool>, tokio::task::JoinHandle<()>)>>,
}

impl ChatPool {
    pub fn new(username: &str, token: &str) -> Self {
        Self::with_options(username, token, false, None)
    }

    pub fn with_options(
        username: &str,
        token: &str,
        disable_at_in_nickname: bool,
        logger: Option<Arc<SessionLogger>>,
    ) -> Self {
        Self {
            username: username.to_string(),
            token: token.to_string(),
            disable_at_in_nickname,
            logger,
            handles: Mutex::new(Vec::new()),
        }
    }

    pub fn ensure_joined(&self, channel: &str) {
        let ch_lower = channel.to_lowercase();
        let mut handles = self.handles.lock().unwrap();
        if handles.iter().any(|(c, _, _)| c == &ch_lower) {
            return;
        }
        let client = ChatClient::new(
            &self.username,
            &self.token,
            &ch_lower,
            self.disable_at_in_nickname,
            self.logger.clone(),
        );
        let running = client.running.clone();
        let handle = tokio::spawn(async move {
            let _ = client.run().await;
        });
        handles.push((ch_lower, running, handle));
    }

    pub fn leave(&self, channel: &str) {
        let ch_lower = channel.to_lowercase();
        let mut handles = self.handles.lock().unwrap();
        if let Some(pos) = handles.iter().position(|(c, _, _)| c == &ch_lower) {
            let (_, running, handle) = handles.remove(pos);
            running.store(false, Ordering::Relaxed);
            handle.abort();
            if let Some(log) = &self.logger {
                log.info("💬", &format!("Leave IRC Chat: #{}", ch_lower));
            } else {
                info!("IRC: left #{}", ch_lower);
            }
        }
    }

    pub fn leave_all(&self) {
        let mut handles = self.handles.lock().unwrap();
        for (ch, running, handle) in handles.drain(..) {
            running.store(false, Ordering::Relaxed);
            handle.abort();
            if let Some(log) = &self.logger {
                log.info("💬", &format!("Leave IRC Chat: #{}", ch));
            } else {
                info!("IRC: left #{}", ch);
            }
        }
    }
}
