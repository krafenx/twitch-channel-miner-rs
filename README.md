# Twitch Channel Points Miner (Rust)

Полнофункциональный Rust-порт [Twitch Channel Points Miner v2](https://github.com/rdavydov/Twitch-Channel-Points-Miner-v2) с CLI и нативным desktop-приложением на `egui`. Оба приложения используют единый обратно совместимый `config.json` и поддерживают одновременную работу нескольких Twitch-аккаунтов.

## Сборка и запуск

```powershell
cargo build --release --bins
```

### Запуск GUI

```powershell
.\target\release\gui.exe
```

GUI позволяет управлять несколькими профилями Twitch, запускать и останавливать майнер как **на всех аккаунтах сразу**, так и **отдельно для каждого аккаунта** (с фильтрацией журнала по аккаунтам), задавать шаблон настроек каналов по умолчанию и индивидуальные правила для каждого стримера, настраивать стратегии ставок, приоритеты просмотра, логирование и все 6 сервисов уведомлений с фильтрацией по событиям.

### Запуск CLI

Запуск на всех аккаунтах из общего JSON-конфига:

```powershell
.\target\release\twitch-points-miner.exe --config "$env:APPDATA\twitch-points-miner\config.json"
```

Запуск только выбранного аккаунта (или нескольких через запятую) из JSON-конфига:

```powershell
.\target\release\twitch-points-miner.exe --config "$env:APPDATA\twitch-points-miner\config.json" --account YOUR_NICK
```

Просмотр списка аккаунтов в JSON-конфиге:

```powershell
.\target\release\twitch-points-miner.exe --config "$env:APPDATA\twitch-points-miner\config.json" --list-accounts
```

Либо напрямую через аргументы командной строки:

```powershell
.\target\release\twitch-points-miner.exe -u YOUR_NICK -s streamer1,streamer2 --followers --followers-order asc --chat online
```

## Основные возможности

- **Twitch TV Device-Code OAuth** с сохранением сессии, динамическим обновлением `Client-Version` (`__twilightBuildID`) и устойчивостью к обрывам сети.
- **Автофарм баллов и бонусов**: симуляция просмотра (`minute-watched` через HLS + Spade), мгновенный сбор сундуков `+50` через PubSub (`claim-available`) и фоновая сверка контекста.
- **Масштабируемый PubSub-пул**: автоматическое распределение подписок по нескольким WebSocket-соединениям (до 50 топиков на сокет), `PING`/`PONG` и обработка `RECONNECT`.
- **Приоритеты просмотра**: `Streak` (Watch Streak), `Drops`, `Subscribed` (по множителю очков подписки), `Order`, `PointsAscending`, `PointsDescending`.
- **Полноценные Twitch Drops**: синхронизация активных кампаний дашборда и инвентаря, учёт прогресса с прогресс-барами и автоматический сбор наград.
- **Прогнозы (Predictions)**: живое обновление коэффициентов и `top_predictors` по `event-updated`, все 13 стратегий (`Smart`, `MostVoted`, `HighOdds`, `Percentage`, `SmartMoney`, `Number1`–`Number8`), `stealth_mode`, `FilterCondition` и учёт результатов (`predictions-user-v1`).
- **Рейды, Moments, Community Goals и IRC-чат** (включая уведомления об упоминании `@username` и режим `disable_at_in_nickname`).
- **6 каналов уведомлений** (**Telegram**, **Discord**, **HTTP Webhook** `GET`/`POST`, **Gotify**, **Matrix**, **Pushover**) с индивидуальной фильтрацией по всем 19 типам событий.
- **Веб-аналитика**: интерактивный график очков, цветные аннотации событий, фильтр дат, переключение тёмной/светлой темы и вкладка live-логов (`/`, `/streamers`, `/json/<name>`, `/json_all`, `/log`).
- **Гибкое логирование и финальный отчёт сессии** при остановке (`Ctrl+C`) с ротацией файлов логов, часовым поясом и цветовой палитрой.

Подробная матрица соответствия оригинальному проекту — в [FEATURE_PARITY.md](FEATURE_PARITY.md).
