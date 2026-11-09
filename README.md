# Twitch Channel Points Miner (Rust)

Rust-порт Twitch Channel Points Miner v2. В проекте есть CLI и нативное desktop-приложение на egui. Оба используют общий JSON-конфиг.

## Запуск GUI

```powershell
cargo build --release --bins
.\target-final\release\gui.exe
```

GUI позволяет добавлять несколько Twitch-аккаунтов и переключаться между ними. У каждого профиля свои каналы, настройки каналов, стратегии ставок, чат и уведомления. Запуск майнера запускает все заполненные профили одновременно. Первый вход каждого аккаунта проходит через device-code; ссылки и коды видны в общем журнале окна.

Готовые Windows-бинарники этой сборки: `target-final/release/gui.exe` и `target-final/release/twitch-points-miner.exe`.

В настройках канала блок «Автоставки: стратегия и сумма» задаёт выбор стороны, размер ставки как долю баланса, верхний лимит, порог баланса, фильтр и момент размещения. Правила ставок настраиваются отдельно для каждого канала. В блоке «Очередь выбора каналов» правила проверяются сверху вниз до заполнения лимита одновременного просмотра. Это отдельный порядок от ручного списка каналов: кнопки ↑/↓ у канала меняют его место в ручном списке, который использует правило «Ручной список каналов». Сортировка подписок применяется к загружаемым Twitch-подпискам.

## Запуск CLI

С настройками из GUI:

```powershell
.\target-final\release\twitch-points-miner.exe --config "$env:APPDATA\twitch-points-miner\config.json"
```

Либо передайте параметры напрямую:

```powershell
.\target-final\release\twitch-points-miner.exe -u YOUR_NICK -s streamer1,streamer2 --followers --chat online
```

Сборка обоих исполняемых файлов:

```powershell
cargo build --release --bins
```

## Реализовано в текущем Rust-порте

- Twitch device-code OAuth и сохранение сессии
- Загрузка списка подписок, разрешение каналов, просмотр статуса и баланса очков
- Автосбор бонусов и Drops, watch-streak, community goals и Moments
- Spade/HLS minute-watched, IRC-присутствие и уведомления об упоминании
- PubSub, автоматический переход по рейдам и ставки на Predictions
- Стратегии ставок Smart, Most Voted, High Odds, Percentage, Smart Money и выбор варианта №1–8; фильтры ставок
- Приоритеты каналов, общая HTTP-аналитика по всем аккаунтам с разделением по имени аккаунта, уведомления Telegram, Discord, HTTP webhook и Gotify
- Редактирование каналов и поддерживаемых настроек в GUI; CLI принимает тот же JSON-конфиг через `--config`

Полного соответствия оригиналу пока нет. Текущая сверка и список недостающих функций — в [FEATURE_PARITY.md](FEATURE_PARITY.md).

Конфиг GUI и сессии Twitch сохраняются в `%APPDATA%\twitch-points-miner\`. Токены уведомлений сейчас записываются в `config.json` как обычный текст.
