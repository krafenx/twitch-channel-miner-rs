//! Simple IRC chat presence for watch-time / StreamElements style points
use crate::error::{MinerError, Result};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tracing::{debug, info, warn};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChatPresence {
    /// Never join chat
    Never,
    /// Join only when stream is online
    Online,
    /// Join only when stream is offline
    Offline,
    /// Always stay in chat
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
    running: Arc<AtomicBool>,
}

impl ChatClient {
    pub fn new(username: &str, token: &str, channel: &str) -> Self {
        Self {
            username: username.to_lowercase(),
            // Twitch IRC wants oauth: prefix
            token: if token.starts_with("oauth:") {
                token.to_string()
            } else {
                format!("oauth:{}", token)
            },
            channel: channel.to_lowercase(),
            running: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn stop(&self) {
        self.running.store(false, Ordering::Relaxed);
    }

    pub async fn run(&self) -> Result<()> {
        self.running.store(true, Ordering::Relaxed);
        info!("IRC: joining #{}", self.channel);

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
                    warn!("IRC error on #{}: {}, retry in 15s", self.channel, e);
                }
            }
            tokio::time::sleep(std::time::Duration::from_secs(15)).await;
        }
        Ok(())
    }

    async fn connect_once(&self) -> Result<()> {
        let stream = TcpStream::connect("irc.chat.twitch.tv:6667")
            .await
            .map_err(|e| MinerError::Other(format!("IRC connect: {}", e)))?;

        let (reader, mut writer) = stream.into_split();
        let mut reader = BufReader::new(reader);

        // Auth
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

        info!("IRC: joined #{}", self.channel);

        let mut line = String::new();
        loop {
            if !self.running.load(Ordering::Relaxed) {
                let _ = writer.write_all(b"QUIT\r\n").await;
                break;
            }

            line.clear();
            let n = tokio::time::timeout(
                std::time::Duration::from_secs(120),
                reader.read_line(&mut line),
            )
            .await
            .map_err(|_| MinerError::Other("IRC read timeout".into()))?
            .map_err(|e| MinerError::Other(format!("IRC read: {}", e)))?;

            if n == 0 {
                break;
            }

            let msg = line.trim_end();
            if msg.starts_with("PING") {
                let pong = msg.replacen("PING", "PONG", 1);
                writer.write_all(format!("{}\r\n", pong).as_bytes()).await?;
                writer.flush().await?;
                debug!("IRC PONG");
            } else if msg.contains(" PRIVMSG ") {
                // Optional: detect @mention of our username
                if msg.to_lowercase().contains(&format!("@{}", self.username)) {
                    info!("💬 Mention in #{}: {}", self.channel, msg);
                }
            }
        }
        Ok(())
    }
}

/// Manage multiple chat connections
pub struct ChatPool {
    username: String,
    token: String,
    handles: std::sync::Mutex<Vec<(String, Arc<AtomicBool>, tokio::task::JoinHandle<()>)>>,
}

impl ChatPool {
    pub fn new(username: &str, token: &str) -> Self {
        Self {
            username: username.to_string(),
            token: token.to_string(),
            handles: std::sync::Mutex::new(Vec::new()),
        }
    }

    pub fn ensure_joined(&self, channel: &str) {
        let mut handles = self.handles.lock().unwrap();
        if handles.iter().any(|(c, _, _)| c == channel) {
            return;
        }
        let client = ChatClient::new(&self.username, &self.token, channel);
        let running = client.running.clone();
        let ch = channel.to_string();
        let handle = tokio::spawn(async move {
            let _ = client.run().await;
        });
        handles.push((ch, running, handle));
    }

    pub fn leave(&self, channel: &str) {
        let mut handles = self.handles.lock().unwrap();
        if let Some(pos) = handles.iter().position(|(c, _, _)| c == channel) {
            let (_, running, handle) = handles.remove(pos);
            running.store(false, Ordering::Relaxed);
            handle.abort();
            info!("IRC: left #{}", channel);
        }
    }

    pub fn leave_all(&self) {
        let mut handles = self.handles.lock().unwrap();
        for (ch, running, handle) in handles.drain(..) {
            running.store(false, Ordering::Relaxed);
            handle.abort();
            info!("IRC: left #{}", ch);
        }
    }
}
