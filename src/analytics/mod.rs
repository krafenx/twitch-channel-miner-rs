//! Analytics: persistent JSON series + event annotations + full HTTP dashboard
//! Compatible with both upstream `AnalyticsServer.py` (`/streamers`, `/json/<name>`, `/json_all`, `/log`)
//! and multi-account Rust routes (`/api/data`, `/api/streamer/<name>`).
use crate::logger::sanitize_filename;
use chrono::{NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tracing::{info, warn};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnalyticsEvent {
    /// unix ms
    pub x: i64,
    /// points balance
    pub y: i64,
    /// event type label
    pub z: String,
    #[serde(default)]
    pub streamer: String,
    #[serde(default)]
    pub account: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnnotationStyle {
    pub color: String,
    pub background: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnnotationLabel {
    pub style: AnnotationStyle,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnalyticsAnnotation {
    pub x: i64,
    #[serde(rename = "borderColor")]
    pub border_color: String,
    pub label: AnnotationLabel,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct StreamerSeries {
    pub streamer: String,
    pub account: String,
    pub series: Vec<AnalyticsEvent>,
    pub annotations: Vec<AnalyticsAnnotation>,
}

pub struct Analytics {
    base_dir: PathBuf,
    path: PathBuf,
    refresh_minutes: u64,
    days_ago: u64,
    data: Arc<Mutex<HashMap<String, StreamerSeries>>>,
}

impl Analytics {
    pub fn new(base_dir: impl AsRef<Path>) -> Self {
        Self::with_options(base_dir, 5, 7)
    }

    pub fn with_options(base_dir: impl AsRef<Path>, refresh_minutes: u64, days_ago: u64) -> Self {
        let base_dir = base_dir.as_ref().to_path_buf();
        let path = base_dir.join("analytics");
        let _ = fs::create_dir_all(&path);
        let mut data = HashMap::new();
        load_series_dir(&path, None, &mut data);
        if let Ok(entries) = fs::read_dir(&path) {
            for entry in entries.flatten() {
                if entry.path().is_dir() {
                    let folder_account = entry.file_name().to_string_lossy().to_string();
                    load_series_dir(&entry.path(), Some(&folder_account), &mut data);
                }
            }
        }
        Self {
            base_dir,
            path,
            refresh_minutes: refresh_minutes.max(1),
            days_ago: days_ago.max(1),
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
            annotations: Vec::new(),
        });
        let formatted_kind = format_reason_label(kind);
        entry.series.push(AnalyticsEvent {
            x: (Utc::now().timestamp() * 1000),
            y: points,
            z: formatted_kind,
            streamer: streamer.to_string(),
            account: account.to_string(),
        });
        self.persist_entry(account, streamer, entry);
    }

    /// Upstream `Streamer.persistent_annotations(event_type, event_text)`
    pub fn record_annotation(
        &self,
        account: &str,
        streamer: &str,
        event_type: &str,
        event_text: &str,
    ) {
        let upper = event_type.to_ascii_uppercase();
        let primary_color = match upper.as_str() {
            "WATCH_STREAK" => "#45c1ff",
            "PREDICTION_MADE" => "#ffe045",
            "WIN" => "#36b535",
            "LOSE" => "#ff4545",
            _ => return,
        };

        let mut map = self.data.lock().unwrap();
        let key = analytics_key(account, streamer);
        let entry = map.entry(key).or_insert_with(|| StreamerSeries {
            streamer: streamer.to_string(),
            account: account.to_string(),
            series: Vec::new(),
            annotations: Vec::new(),
        });
        entry.annotations.push(AnalyticsAnnotation {
            x: (Utc::now().timestamp() * 1000),
            border_color: primary_color.to_string(),
            label: AnnotationLabel {
                style: AnnotationStyle {
                    color: "#000".to_string(),
                    background: primary_color.to_string(),
                },
                text: event_text.to_string(),
            },
        });
        self.persist_entry(account, streamer, entry);
    }

    fn persist_entry(&self, account: &str, streamer: &str, entry: &StreamerSeries) {
        let account_dir = if account.is_empty() {
            self.path.clone()
        } else {
            self.path.join(safe_file_part(account))
        };
        let _ = fs::create_dir_all(&account_dir);
        let file = account_dir.join(format!("{}.json", safe_file_part(streamer)));
        let temp_file = account_dir.join(format!("{}.json.temp", safe_file_part(streamer)));
        if let Ok(json) = serde_json::to_string_pretty(entry) {
            if fs::write(&temp_file, json).is_ok() {
                let _ = fs::rename(&temp_file, &file);
            }
        }
    }

    pub fn snapshot(&self) -> HashMap<String, StreamerSeries> {
        self.data.lock().unwrap().clone()
    }

    fn find_series<'a>(
        map: &'a HashMap<String, StreamerSeries>,
        raw_name: &str,
    ) -> Option<&'a StreamerSeries> {
        let decoded = urlencoding::decode(raw_name)
            .map(|c| c.into_owned())
            .unwrap_or_else(|_| raw_name.to_string());
        let trimmed = decoded.trim_end_matches(".json");
        if let Some(s) = map.get(trimmed) {
            return Some(s);
        }
        map.values()
            .find(|s| s.streamer.eq_ignore_ascii_case(trimmed))
    }

    fn filter_series(
        series: &StreamerSeries,
        start_ms: Option<i64>,
        end_ms: Option<i64>,
    ) -> StreamerSeries {
        let start = start_ms.unwrap_or(0);
        let end = end_ms.unwrap_or_else(|| Utc::now().timestamp_millis() + 86_400_000);

        let mut filtered_events: Vec<AnalyticsEvent> = series
            .series
            .iter()
            .filter(|e| e.x >= start && e.x <= end)
            .cloned()
            .collect();
        filtered_events.sort_by_key(|e| (e.x, e.y));

        // Upstream fallback: if no events in timeframe, emit flat "No Stream" line from last prior balance
        if filtered_events.is_empty() && !series.series.is_empty() {
            let prior_balance = series
                .series
                .iter()
                .filter(|e| e.x <= start)
                .max_by_key(|e| e.x)
                .or_else(|| series.series.first())
                .map(|e| e.y)
                .unwrap_or(0);
            let effective_start = if start > 0 {
                start
            } else {
                Utc::now().timestamp_millis() - 86_400_000
            };
            filtered_events = vec![
                AnalyticsEvent {
                    x: effective_start,
                    y: prior_balance,
                    z: "No Stream".to_string(),
                    streamer: series.streamer.clone(),
                    account: series.account.clone(),
                },
                AnalyticsEvent {
                    x: end,
                    y: prior_balance,
                    z: "No Stream".to_string(),
                    streamer: series.streamer.clone(),
                    account: series.account.clone(),
                },
            ];
        }

        let mut filtered_annotations: Vec<AnalyticsAnnotation> = series
            .annotations
            .iter()
            .filter(|a| a.x >= start && a.x <= end)
            .cloned()
            .collect();
        filtered_annotations.sort_by_key(|a| a.x);

        StreamerSeries {
            streamer: series.streamer.clone(),
            account: series.account.clone(),
            series: filtered_events,
            annotations: filtered_annotations,
        }
    }

    fn read_log_tail(&self, account: Option<&str>, last_index: usize) -> (String, usize) {
        let logs_dir = self.base_dir.join("logs");
        let candidate_path = if let Some(acc) = account.filter(|a| !a.trim().is_empty()) {
            Some(logs_dir.join(format!("{}.log", sanitize_filename(acc))))
        } else {
            fs::read_dir(&logs_dir).ok().and_then(|entries| {
                entries
                    .flatten()
                    .map(|e| e.path())
                    .find(|p| p.extension().and_then(|s| s.to_str()) == Some("log"))
            })
        };

        let Some(path) = candidate_path else {
            return (String::new(), 0);
        };
        let Ok(content) = fs::read_to_string(path) else {
            return (String::new(), 0);
        };
        let total = content.len();
        if last_index >= total {
            return (String::new(), total);
        }
        // Ensure valid UTF-8 char boundary
        let mut idx = last_index;
        while idx < total && !content.is_char_boundary(idx) {
            idx += 1;
        }
        (content[idx..].to_string(), total)
    }

    pub async fn serve(self: Arc<Self>, bind: &str) {
        let listener = match TcpListener::bind(bind).await {
            Ok(l) => l,
            Err(e) => {
                warn!("Analytics server bind failed on {}: {}", bind, e);
                return;
            }
        };
        info!("📊 Analytics running on http://{}/", bind);

        loop {
            let (mut socket, _) = match listener.accept().await {
                Ok(s) => s,
                Err(_) => continue,
            };
            let analytics = self.clone();
            tokio::spawn(async move {
                let mut buf = vec![0u8; 4096];
                let n = match socket.read(&mut buf).await {
                    Ok(n) if n > 0 => n,
                    _ => return,
                };
                let req = String::from_utf8_lossy(&buf[..n]);
                let raw_target = req
                    .lines()
                    .next()
                    .unwrap_or("")
                    .split_whitespace()
                    .nth(1)
                    .unwrap_or("/");

                let (path, query_map) = split_path_and_query(raw_target);
                let start_ms = query_map
                    .get("startDate")
                    .and_then(|d| parse_date_ms(d, false));
                let end_ms = query_map
                    .get("endDate")
                    .and_then(|d| parse_date_ms(d, true));

                let (status, content_type, body) = if path == "/" || path == "/index.html" {
                    (
                        "200 OK",
                        "text/html; charset=utf-8",
                        dashboard_html(analytics.refresh_minutes, analytics.days_ago),
                    )
                } else if path == "/streamers" {
                    let snap = analytics.snapshot();
                    let mut list: Vec<serde_json::Value> = snap
                        .iter()
                        .map(|(key, s)| {
                            let pts = s.series.last().map(|e| e.y).unwrap_or(0);
                            let last_act = s.series.last().map(|e| e.x).unwrap_or(0);
                            serde_json::json!({
                                "name": key,
                                "streamer": s.streamer,
                                "account": s.account,
                                "points": pts,
                                "last_activity": last_act,
                            })
                        })
                        .collect();
                    list.sort_by(|a, b| {
                        a["name"]
                            .as_str()
                            .unwrap_or("")
                            .cmp(b["name"].as_str().unwrap_or(""))
                    });
                    (
                        "200 OK",
                        "application/json",
                        serde_json::to_string(&list).unwrap_or_else(|_| "[]".into()),
                    )
                } else if let Some(name) = path.strip_prefix("/json/") {
                    let snap = analytics.snapshot();
                    if let Some(series) = Self::find_series(&snap, name) {
                        let filtered = Self::filter_series(series, start_ms, end_ms);
                        (
                            "200 OK",
                            "application/json",
                            serde_json::to_string(&filtered).unwrap_or_else(|_| "{}".into()),
                        )
                    } else {
                        (
                            "404 Not Found",
                            "application/json",
                            r#"{"error":"Streamer analytics not found"}"#.to_string(),
                        )
                    }
                } else if path == "/json_all" {
                    let snap = analytics.snapshot();
                    let mut keys: Vec<&String> = snap.keys().collect();
                    keys.sort();
                    let all: Vec<serde_json::Value> = keys
                        .into_iter()
                        .map(|k| {
                            let filtered = Self::filter_series(&snap[k], start_ms, end_ms);
                            serde_json::json!({
                                "name": k,
                                "data": filtered,
                            })
                        })
                        .collect();
                    (
                        "200 OK",
                        "application/json",
                        serde_json::to_string(&all).unwrap_or_else(|_| "[]".into()),
                    )
                } else if path == "/log" {
                    let last_idx = query_map
                        .get("lastIndex")
                        .and_then(|v| v.parse::<usize>().ok())
                        .unwrap_or(0);
                    let account = query_map.get("account").map(|s| s.as_str());
                    let (chunk, next_idx) = analytics.read_log_tail(account, last_idx);
                    if query_map.contains_key("json") {
                        let payload = serde_json::json!({
                            "text": chunk,
                            "nextIndex": next_idx,
                        });
                        ("200 OK", "application/json", payload.to_string())
                    } else {
                        ("200 OK", "text/plain; charset=utf-8", chunk)
                    }
                } else if path == "/api/data" {
                    let snap = analytics.snapshot();
                    let mut out = HashMap::new();
                    for (k, v) in &snap {
                        if start_ms.is_some() || end_ms.is_some() {
                            out.insert(k.clone(), Self::filter_series(v, start_ms, end_ms));
                        } else {
                            out.insert(k.clone(), v.clone());
                        }
                    }
                    let json = serde_json::to_string(&out).unwrap_or_else(|_| "{}".into());
                    ("200 OK", "application/json", json)
                } else if let Some(name) = path.strip_prefix("/api/streamer/") {
                    let snap = analytics.snapshot();
                    let json = Self::find_series(&snap, name)
                        .map(|s| {
                            let filtered = Self::filter_series(s, start_ms, end_ms);
                            serde_json::to_string(&filtered).unwrap_or_else(|_| "{}".into())
                        })
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

fn load_series_dir(
    dir: &Path,
    fallback_account: Option<&str>,
    data: &mut HashMap<String, StreamerSeries>,
) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_file() && p.extension().and_then(|s| s.to_str()) == Some("json") {
            if let Ok(text) = fs::read_to_string(&p) {
                if let Ok(mut series) = serde_json::from_str::<StreamerSeries>(&text) {
                    if series.streamer.is_empty() {
                        if let Some(stem) = p.file_stem().and_then(|s| s.to_str()) {
                            series.streamer = stem.to_string();
                        }
                    }
                    if series.account.is_empty() {
                        if let Some(acc) = fallback_account {
                            series.account = acc.to_string();
                        }
                    }
                    let key = analytics_key(&series.account, &series.streamer);
                    data.insert(key, series);
                }
            }
        }
    }
}

fn format_reason_label(kind: &str) -> String {
    if kind.contains('_') {
        kind.split('_')
            .map(|w| {
                let mut chars = w.chars();
                match chars.next() {
                    None => String::new(),
                    Some(first) => {
                        first.to_uppercase().collect::<String>() + &chars.as_str().to_lowercase()
                    }
                }
            })
            .collect::<Vec<_>>()
            .join(" ")
    } else {
        kind.to_string()
    }
}

fn split_path_and_query(target: &str) -> (&str, HashMap<String, String>) {
    let mut map = HashMap::new();
    if let Some((path, q)) = target.split_once('?') {
        for pair in q.split('&') {
            if let Some((k, v)) = pair.split_once('=') {
                let dk = urlencoding::decode(k)
                    .map(|c| c.into_owned())
                    .unwrap_or_else(|_| k.to_string());
                let dv = urlencoding::decode(v)
                    .map(|c| c.into_owned())
                    .unwrap_or_else(|_| v.to_string());
                map.insert(dk, dv);
            } else if !pair.is_empty() {
                map.insert(pair.to_string(), String::new());
            }
        }
        (path, map)
    } else {
        (target, map)
    }
}

fn parse_date_ms(date_str: &str, end_of_day: bool) -> Option<i64> {
    let date = NaiveDate::parse_from_str(date_str.trim(), "%Y-%m-%d").ok()?;
    let dt = if end_of_day {
        date.and_hms_opt(23, 59, 59)?
    } else {
        date.and_hms_opt(0, 0, 0)?
    };
    Some(dt.and_utc().timestamp_millis())
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

fn dashboard_html(refresh_minutes: u64, days_ago: u64) -> String {
    let refresh_ms = refresh_minutes.max(1) * 60_000;
    format!(
        r##"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8"/>
<meta name="viewport" content="width=device-width, initial-scale=1"/>
<title>Twitch Points Miner – Analytics</title>
<style>
  :root {{
    --bg: #0e0e10; --panel: #18181b; --border: #2d2d33; --text: #efeff1;
    --muted: #adadb8; --accent: #9147ff; --accent2: #00c8b4; --grid: #26262c;
  }}
  body.light {{
    --bg: #f7f7f8; --panel: #ffffff; --border: #dedee3; --text: #18181b;
    --muted: #53535f; --accent: #772ce8; --accent2: #008a7c; --grid: #e5e5ea;
  }}
  * {{ box-sizing: border-box; }}
  body {{ font-family: system-ui, -apple-system, sans-serif; margin: 0; padding: 1.5rem 2rem; background: var(--bg); color: var(--text); }}
  header {{ display: flex; flex-wrap: wrap; align-items: center; justify-content: space-between; gap: 1rem; margin-bottom: 1.25rem; }}
  h1 {{ margin: 0; font-size: 1.45rem; color: var(--accent); }}
  .controls {{ display: flex; flex-wrap: wrap; align-items: center; gap: 0.75rem; background: var(--panel); padding: 0.75rem 1rem; border-radius: 8px; border: 1px solid var(--border); }}
  input[type="date"], select, button {{ background: var(--bg); color: var(--text); border: 1px solid var(--border); border-radius: 6px; padding: 0.35rem 0.6rem; font-size: 0.9rem; }}
  button {{ cursor: pointer; }}
  button.active {{ background: var(--accent); color: #fff; border-color: var(--accent); }}
  .pills {{ display: flex; flex-wrap: wrap; gap: 0.5rem; margin: 1rem 0; }}
  .pill {{ padding: 0.4rem 0.75rem; border-radius: 999px; border: 1px solid var(--border); background: var(--panel); cursor: pointer; font-size: 0.88rem; }}
  .pill.active {{ background: var(--accent); color: #fff; border-color: var(--accent); }}
  .card {{ background: var(--panel); border: 1px solid var(--border); border-radius: 10px; padding: 1.1rem 1.3rem; margin-bottom: 1.2rem; }}
  .stats {{ display: flex; gap: 2rem; flex-wrap: wrap; align-items: baseline; margin-bottom: 0.75rem; }}
  .points {{ font-size: 1.6rem; font-weight: 700; color: var(--accent2); }}
  .chart-wrap {{ position: relative; width: 100%; height: 340px; }}
  svg {{ width: 100%; height: 100%; overflow: visible; }}
  .tooltip {{ position: absolute; pointer-events: none; background: var(--panel); border: 1px solid var(--accent); border-radius: 6px; padding: 0.45rem 0.65rem; font-size: 0.82rem; display: none; z-index: 10; box-shadow: 0 4px 14px rgba(0,0,0,0.35); }}
  table {{ width: 100%; border-collapse: collapse; font-size: 0.9rem; }}
  th, td {{ text-align: left; padding: 0.45rem 0.6rem; border-bottom: 1px solid var(--border); }}
  th {{ color: var(--muted); font-weight: 500; }}
  .tag {{ display: inline-block; padding: 0.12rem 0.48rem; border-radius: 4px; font-size: 0.78rem; background: var(--border); }}
  pre#logbox {{ background: var(--bg); border: 1px solid var(--border); border-radius: 8px; padding: 0.8rem; max-height: 360px; overflow: auto; font-size: 0.8rem; white-space: pre-wrap; }}
</style>
</head>
<body>
<header>
  <div>
    <h1>Twitch Channel Points Miner – Analytics</h1>
    <small style="color:var(--muted)">Auto-refreshes every {refresh_minutes} min</small>
  </div>
  <div class="controls">
    <button id="tabChart" class="active" onclick="setTab('chart')">Charts</button>
    <button id="tabLogs" onclick="setTab('logs')">Live Logs</button>
    <label>From <input type="date" id="startDate" onchange="reloadData()"/></label>
    <label>To <input type="date" id="endDate" onchange="reloadData()"/></label>
    <label><input type="checkbox" id="showAnnotations" checked onchange="renderCurrent()"/> Annotations</label>
    <label><input type="checkbox" id="darkTheme" checked onchange="toggleTheme()"/> Dark theme</label>
  </div>
</header>

<div id="chartView">
  <div class="pills" id="streamerPills"></div>
  <div id="chartContainer" class="card">Loading…</div>
</div>

<div id="logsView" class="card" style="display:none">
  <h3 style="margin-top:0">Session Log Stream</h3>
  <pre id="logbox">Waiting for logs…</pre>
</div>

<script>
const DAYS_AGO = {days_ago};
const REFRESH_MS = {refresh_ms};
let allData = {{}};
let selectedKey = null;
let logIndex = 0;

function initDates() {{
  const now = new Date();
  const start = new Date(now.getTime() - DAYS_AGO * 86400000);
  document.getElementById('endDate').value = now.toISOString().slice(0, 10);
  document.getElementById('startDate').value = start.toISOString().slice(0, 10);
}}

function toggleTheme() {{
  const dark = document.getElementById('darkTheme').checked;
  document.body.classList.toggle('light', !dark);
  renderCurrent();
}}

function setTab(tab) {{
  document.getElementById('tabChart').classList.toggle('active', tab === 'chart');
  document.getElementById('tabLogs').classList.toggle('active', tab === 'logs');
  document.getElementById('chartView').style.display = tab === 'chart' ? '' : 'none';
  document.getElementById('logsView').style.display = tab === 'logs' ? '' : 'none';
  if (tab === 'logs') pollLogs();
}}

async function reloadData() {{
  const s = document.getElementById('startDate').value;
  const e = document.getElementById('endDate').value;
  const qs = new URLSearchParams();
  if (s) qs.set('startDate', s);
  if (e) qs.set('endDate', e);
  const res = await fetch('/api/data?' + qs.toString());
  allData = await res.json();
  const keys = Object.keys(allData).sort();
  if (!selectedKey || !allData[selectedKey]) selectedKey = keys[0] || null;
  renderPills(keys);
  renderCurrent();
}}

function renderPills(keys) {{
  const wrap = document.getElementById('streamerPills');
  wrap.innerHTML = '';
  for (const k of keys) {{
    const s = allData[k];
    const last = s.series && s.series.length ? s.series[s.series.length - 1].y : 0;
    const btn = document.createElement('button');
    btn.className = 'pill' + (k === selectedKey ? ' active' : '');
    btn.textContent = `${{k}} (${{last.toLocaleString()}})`;
    btn.onclick = () => {{ selectedKey = k; renderPills(keys); renderCurrent(); }};
    wrap.appendChild(btn);
  }}
}}

function renderCurrent() {{
  const box = document.getElementById('chartContainer');
  if (!selectedKey || !allData[selectedKey]) {{
    box.innerHTML = '<p>No analytics data yet. Keep the miner running.</p>';
    return;
  }}
  const s = allData[selectedKey];
  const series = s.series || [];
  const annotations = document.getElementById('showAnnotations').checked ? (s.annotations || []) : [];
  const first = series[0];
  const last = series[series.length - 1];
  const delta = first && last ? last.y - first.y : 0;

  let html = `<div class="stats">
    <h2 style="margin:0">${{selectedKey}}</h2>
    <div class="points">${{last ? last.y.toLocaleString() : '—'}} pts
      <span style="font-size:1rem;color:${{delta >= 0 ? 'var(--accent2)' : '#ff5555'}}">(${{delta >= 0 ? '+' : ''}}${{delta.toLocaleString()}})</span>
    </div>
    <span style="color:var(--muted)">${{series.length}} data points · ${{annotations.length}} annotations</span>
  </div>
  <div class="chart-wrap" id="svgWrap"><div class="tooltip" id="tip"></div></div>
  <h4>Recent Events</h4>
  <table><tr><th>Time</th><th>Balance</th><th>Reason</th></tr>`;

  for (const ev of series.slice(-20).reverse()) {{
    html += `<tr><td>${{new Date(ev.x).toLocaleString()}}</td><td>${{ev.y.toLocaleString()}}</td><td><span class="tag">${{ev.z}}</span></td></tr>`;
  }}
  html += '</table>';
  box.innerHTML = html;
  drawChart(series, annotations);
}}

function drawChart(series, annotations) {{
  const wrap = document.getElementById('svgWrap');
  if (!wrap || !series.length) return;
  const W = wrap.clientWidth || 800;
  const H = wrap.clientHeight || 320;
  const pad = {{ l: 64, r: 24, t: 22, b: 34 }};
  const xs = series.map(p => p.x);
  const ys = series.map(p => p.y);
  let minX = Math.min(...xs), maxX = Math.max(...xs);
  let minY = Math.min(...ys), maxY = Math.max(...ys);
  if (minX === maxX) {{ minX -= 60000; maxX += 60000; }}
  if (minY === maxY) {{ minY = Math.max(0, minY - 50); maxY += 50; }}
  const sx = x => pad.l + ((x - minX) / (maxX - minX)) * (W - pad.l - pad.r);
  const sy = y => H - pad.b - ((y - minY) / (maxY - minY)) * (H - pad.t - pad.b);

  let grid = '';
  for (let i = 0; i <= 4; i++) {{
    const val = Math.round(minY + ((maxY - minY) * i) / 4);
    const y = sy(val);
    grid += `<line x1="${{pad.l}}" y1="${{y}}" x2="${{W - pad.r}}" y2="${{y}}" stroke="var(--grid)" stroke-width="1"/>`;
    grid += `<text x="${{pad.l - 8}}" y="${{y + 4}}" fill="var(--muted)" font-size="11" text-anchor="end">${{val.toLocaleString()}}</text>`;
  }}

  const pts = series.map(p => `${{sx(p.x).toFixed(1)}},${{sy(p.y).toFixed(1)}}`).join(' ');
  const area = `${{sx(series[0].x).toFixed(1)}},${{H - pad.b}} ${{pts}} ${{sx(series[series.length - 1].x).toFixed(1)}},${{H - pad.b}}`;

  let annSvg = '';
  for (const a of annotations) {{
    if (a.x < minX || a.x > maxX) continue;
    const ax = sx(a.x).toFixed(1);
    const col = a.borderColor || '#ffe045';
    const txt = (a.label && a.label.text) ? a.label.text.slice(0, 28) : '';
    annSvg += `<line x1="${{ax}}" y1="${{pad.t}}" x2="${{ax}}" y2="${{H - pad.b}}" stroke="${{col}}" stroke-dasharray="4,3" stroke-width="1.5"/>`;
    if (txt) annSvg += `<text x="${{ax}}" y="${{pad.t - 5}}" fill="${{col}}" font-size="10" text-anchor="middle">${{txt}}</text>`;
  }}

  let circles = '';
  series.forEach((p, idx) => {{
    circles += `<circle cx="${{sx(p.x).toFixed(1)}}" cy="${{sy(p.y).toFixed(1)}}" r="3.5" fill="var(--accent2)" data-idx="${{idx}}"/>`;
  }});

  const svg = document.createElementNS('http://www.w3.org/2000/svg', 'svg');
  svg.setAttribute('viewBox', `0 0 ${{W}} ${{H}}`);
  svg.innerHTML = `${{grid}}
    <polygon points="${{area}}" fill="var(--accent)" fill-opacity="0.13"/>
    <polyline fill="none" stroke="var(--accent)" stroke-width="2.2" points="${{pts}}"/>
    ${{annSvg}}${{circles}}`;
  wrap.appendChild(svg);

  const tip = document.getElementById('tip');
  svg.addEventListener('mousemove', ev => {{
    const rect = svg.getBoundingClientRect();
    const mx = ev.clientX - rect.left;
    let best = null, bestDist = 1e9;
    for (const p of series) {{
      const d = Math.abs(sx(p.x) - mx);
      if (d < bestDist) {{ bestDist = d; best = p; }}
    }}
    if (best && bestDist < 48) {{
      tip.style.display = 'block';
      tip.style.left = Math.min(W - 190, Math.max(8, sx(best.x) + 10)) + 'px';
      tip.style.top = Math.max(8, sy(best.y) - 36) + 'px';
      tip.innerHTML = `<b>${{best.y.toLocaleString()}} pts</b><br/>${{best.z}}<br/><span style="color:var(--muted)">${{new Date(best.x).toLocaleString()}}</span>`;
    }} else {{
      tip.style.display = 'none';
    }}
  }});
  svg.addEventListener('mouseleave', () => {{ tip.style.display = 'none'; }});
}}

async function pollLogs() {{
  try {{
    const res = await fetch(`/log?json=1&lastIndex=${{logIndex}}`);
    const payload = await res.json();
    if (payload.text) {{
      const box = document.getElementById('logbox');
      if (logIndex === 0) box.textContent = '';
      box.textContent += payload.text;
      box.scrollTop = box.scrollHeight;
    }}
    logIndex = payload.nextIndex || logIndex;
  }} catch (_) {{}}
}}

initDates();
reloadData();
setInterval(reloadData, REFRESH_MS);
setInterval(() => {{
  if (document.getElementById('logsView').style.display !== 'none') pollLogs();
}}, 5000);
</script>
</body>
</html>"##
    )
}
