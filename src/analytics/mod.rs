//! Simple analytics: JSON event log + lightweight HTTP dashboard
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tracing::{info, warn};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnalyticsEvent {
    /// unix ms
    pub x: i64,
    /// points balance or delta
    pub y: i64,
    /// event type label
    pub z: String,
    pub streamer: String,
    #[serde(default)]
    pub account: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct StreamerSeries {
    pub streamer: String,
    pub account: String,
    pub series: Vec<AnalyticsEvent>,
}

pub struct Analytics {
    path: PathBuf,
    data: Arc<Mutex<HashMap<String, StreamerSeries>>>,
}

impl Analytics {
    pub fn new(base_dir: impl AsRef<Path>) -> Self {
        let path = base_dir.as_ref().join("analytics");
        std::fs::create_dir_all(&path).ok();
        let mut data = HashMap::new();
        // load existing
        if let Ok(entries) = std::fs::read_dir(&path) {
            for e in entries.flatten() {
                if e.path().extension().and_then(|s| s.to_str()) == Some("json") {
                    if let Ok(text) = std::fs::read_to_string(e.path()) {
                        if let Ok(series) = serde_json::from_str::<StreamerSeries>(&text) {
                            let key = analytics_key(&series.account, &series.streamer);
                            data.insert(key, series);
                        }
                    }
                }
            }
        }
        Self {
            path,
            data: Arc::new(Mutex::new(data)),
        }
    }

    pub fn record(&self, account: &str, streamer: &str, points: i64, kind: &str) {
        let mut map = self.data.lock().unwrap();
        let key = analytics_key(account, streamer);
        let entry = map.entry(key).or_insert_with(|| StreamerSeries {
            streamer: streamer.to_string(),
            account: account.to_string(),
            series: Vec::new(),
        });
        entry.series.push(AnalyticsEvent {
            x: Utc::now().timestamp_millis(),
            y: points,
            z: kind.to_string(),
            streamer: streamer.to_string(),
            account: account.to_string(),
        });
        // persist
        let account_dir = if account.is_empty() {
            self.path.clone()
        } else {
            self.path.join(safe_file_part(account))
        };
        let _ = std::fs::create_dir_all(&account_dir);
        let file = account_dir.join(format!("{}.json", safe_file_part(streamer)));
        if let Ok(json) = serde_json::to_string_pretty(entry) {
            let _ = std::fs::write(file, json);
        }
    }

    pub fn snapshot(&self) -> HashMap<String, StreamerSeries> {
        self.data.lock().unwrap().clone()
    }

    pub async fn serve(self: Arc<Self>, bind: &str) {
        let listener = match TcpListener::bind(bind).await {
            Ok(l) => l,
            Err(e) => {
                warn!("Analytics server bind failed: {}", e);
                return;
            }
        };
        info!("📊 Analytics dashboard: http://{}/", bind);

        loop {
            let (mut socket, _) = match listener.accept().await {
                Ok(s) => s,
                Err(_) => continue,
            };
            let analytics = self.clone();
            tokio::spawn(async move {
                let mut buf = vec![0u8; 2048];
                let n = match socket.read(&mut buf).await {
                    Ok(n) if n > 0 => n,
                    _ => return,
                };
                let req = String::from_utf8_lossy(&buf[..n]);
                let path = req
                    .lines()
                    .next()
                    .unwrap_or("")
                    .split_whitespace()
                    .nth(1)
                    .unwrap_or("/");

                let (status, content_type, body) = if path == "/" || path == "/index.html" {
                    ("200 OK", "text/html; charset=utf-8", dashboard_html())
                } else if path == "/api/data" {
                    let data = analytics.snapshot();
                    let json = serde_json::to_string(&data).unwrap_or_else(|_| "{}".into());
                    ("200 OK", "application/json", json)
                } else if path.starts_with("/api/streamer/") {
                    let name = path.trim_start_matches("/api/streamer/");
                    let data = analytics.snapshot();
                    let json = data
                        .get(name)
                        .map(|s| serde_json::to_string(s).unwrap_or_else(|_| "{}".into()))
                        .unwrap_or_else(|| "{}".into());
                    ("200 OK", "application/json", json)
                } else {
                    ("404 Not Found", "text/plain", "not found".into())
                };

                let resp = format!(
                    "HTTP/1.1 {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nAccess-Control-Allow-Origin: *\r\nConnection: close\r\n\r\n{}",
                    status,
                    content_type,
                    body.len(),
                    body
                );
                let _ = socket.write_all(resp.as_bytes()).await;
            });
        }
    }
}

fn analytics_key(account: &str, streamer: &str) -> String {
    if account.is_empty() {
        streamer.to_string()
    } else {
        format!("{account} / {streamer}")
    }
}

fn safe_file_part(value: &str) -> String {
    value
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

fn dashboard_html() -> String {
    r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8"/>
<title>Twitch Points Miner – Analytics</title>
<style>
  body { font-family: system-ui, sans-serif; margin: 2rem; background: #0e0e10; color: #efeff1; }
  h1 { color: #bf94ff; }
  .card { background: #18181b; border-radius: 8px; padding: 1rem 1.25rem; margin: 1rem 0; }
  .points { font-size: 1.4rem; color: #00c8b4; }
  table { width: 100%; border-collapse: collapse; }
  th, td { text-align: left; padding: 0.4rem 0.6rem; border-bottom: 1px solid #2d2d33; }
  th { color: #adadb8; font-weight: 500; }
  .tag { display: inline-block; padding: 0.15rem 0.5rem; border-radius: 4px; font-size: 0.8rem; background: #2d2d33; }
</style>
</head>
<body>
<h1>🦀 Twitch Points Miner – Analytics</h1>
<p>Auto-refreshes every 30s</p>
<div id="root">Loading…</div>
<script>
async function load() {
  const res = await fetch('/api/data');
  const data = await res.json();
  const root = document.getElementById('root');
  const names = Object.keys(data).sort();
  if (!names.length) { root.innerHTML = '<p>No data yet. Keep the miner running.</p>'; return; }
  let html = '';
  for (const name of names) {
    const s = data[name];
    const last = s.series[s.series.length - 1];
    const first = s.series[0];
    const delta = last && first ? last.y - first.y : 0;
    html += `<div class="card"><h2>${name}</h2>
      <div class="points">${last ? last.y.toLocaleString() : '—'} points
        <span style="color:${delta>=0?'#00c8b4':'#f55'}">(${delta>=0?'+':''}${delta.toLocaleString()})</span></div>
      <p>${s.series.length} events</p>
      <table><tr><th>Time</th><th>Points</th><th>Event</th></tr>`;
    for (const e of s.series.slice(-15).reverse()) {
      const t = new Date(e.x).toLocaleString();
      html += `<tr><td>${t}</td><td>${e.y.toLocaleString()}</td><td><span class="tag">${e.z}</span></td></tr>`;
    }
    html += '</table></div>';
  }
  root.innerHTML = html;
}
load();
setInterval(load, 30000);
</script>
</body>
</html>"#.to_string()
}
