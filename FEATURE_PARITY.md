# Сверка функций с оригиналом

Сверено с кодом и README оригинального проекта: https://github.com/rdavydov/Twitch-Channel-Points-Miner-v2

## Полностью портировано в Rust

- **Аутентификация Twitch**: TV Device-Code OAuth (`https://www.twitch.tv/activate`), сохранение и автопроверка сессии, автообновление `Client-Version` (`window.__twilightBuildID`), `Client-Session-Id`, опция `disable_ssl_cert_verification`, автоожидание восстановления интернета.
- **Каналы и настройки**:
  - Ручной список каналов, автозагрузка фолловов (`ChannelFollows`, `ASC`/`DESC`), `blacklist`.
  - Шаблон настроек по умолчанию (`default_streamer_settings`) + индивидуальное переопределение для любого канала (`streamer_settings`).
  - Одновременная работа с несколькими Twitch-аккаунтами в одном процессе (сверх оригинала) и обратная совместимость со старыми `config.json`.
- **Фарм очков и PubSub**:
  - Симуляция просмотра (`PlaybackAccessToken` → HLS master/media `.m3u8` → `HEAD` сегмента → `minute-watched` в Spade).
  - Автосбор бонусных сундуков (+50) как при старте и периодическом обновлении контекста (каждые 30 минут), так и мгновенно по PubSub (`claim-available`).
  - Пул WebSocket-соединений с автоматической разбивкой по лимиту Twitch (**до 50 топиков на сокет**), `PING`/`PONG`-контролем, обработкой `RECONNECT` и дедупликацией сообщений.
  - Точный учёт баланса по `points-earned` / `points-spent`, переходы по рейдам (`raid_update_v2` с защитой от дублей), автосбор `Moments` (`community-moments-channel-v1`).
- **Приоритеты просмотра (`Priority`)**:
  - `Streak` (удержание нового эфира после >30 мин оффлайна до получения `WATCH_STREAK` или 7 минут просмотра),
  - `Drops` (проверка реальных активных кампаний и привязки к игре/каналу),
  - `Subscribed` (фильтрация по `activeMultipliers` и сортировка по суммарному множителю по убыванию),
  - `Order`, `PointsAscending`, `PointsDescending`.
- **Полноценная система Drops (`sync_campaigns`)**:
  - Синхронизация `ViewerDropsDashboard`, пакетная загрузка `DropCampaignDetails` (по 20 кампаний), проверка `DropsHighlightService_AvailableDrops` для канала, сверка дат `startAt`/`endAt` и `hasPreconditionsMet`, трекинг прогресса из `Inventory` с прогресс-баром (`DROP_STATUS`) и автосбор готовых наград (`DROP_CLAIM`).
- **Цели сообщества (`Community Goals`)**:
  - Загрузка целей из `ChannelPointsContext`, подписка на `community-points-channel-v1` (`created`/`updated`/`deleted`), проверка `UserPointsContribution` (`userPointsContributedThisStream`) и вклад в рамках `perStreamUserMaximumContribution`.
- **Прогнозы и ставки (`Predictions`)**:
  - Живое обновление исходов и коэффициентов по `event-updated` вплоть до момента размещения ставки, парсинг `top_predictors` (`TOP_POINTS`), подписка на `predictions-user-v1` (`prediction-made` и `prediction-result` с расчётом чистого профита/рефанда).
  - Все 13 стратегий (`Smart`, `MostVoted`, `HighOdds`, `Percentage`, `SmartMoney`, `Number1`–`Number8`), все метрики `FilterCondition`, режимы задержки (`FromStart`, `FromEnd`, `Percentage`), корректный `stealth_mode` (`top_points - 1..4`) и проверка минимальной ставки Twitch (`>= 10`).
- **IRC-чат**:
  - Режимы `Never`, `Online`, `Offline`, `Always`, парсинг `PRIVMSG` (с IRCv3-тегами), отслеживание упоминаний `@username` и опция `disable_at_in_nickname` с отправкой события `CHAT_MENTION`.
- **Уведомления и фильтрация по 19 событиям (`Events`)**:
  - Провайдеры **Telegram** (включая `disable_notification`), **Discord**, **Webhook** (`GET` и `POST`), **Gotify** (с `priority`), **Matrix** и **Pushover** (`priority`, `sound`).
  - Индивидуальный фильтр из всех 19 событий оригинала (`STREAMER_ONLINE`, `STREAMER_OFFLINE`, `GAIN_FOR_*`, `BET_*`, `BONUS_CLAIM`, `MOMENT_CLAIM`, `JOIN_RAID`, `DROP_CLAIM`, `DROP_STATUS`, `CHAT_MENTION`).
- **Логирование (`LoggerSettings`) и финальный отчёт**:
  - Раздельные уровни для консоли и файла (`console_level`, `file_level`), ежедневная ротация с хранением 7 дней (`save`, `auto_clear`), `time_zone`, `less`, `emoji`, `colored`, настраиваемая `ColorPalette` по событиям и префикс `[username]`.
  - Полный финальный отчёт сессии при завершении (`Ctrl+C`): длительность, рекап всех ставок и сводка нафармленных очков по каждому каналу с разбивкой по причинам.
- **Веб-аналитика**:
  - Интерактивный график баланса с тултипами, цветными аннотациями (`WATCH_STREAK`, `PREDICTION_MADE`, `WIN`, `LOSE`), выбором диапазона дат (`startDate`/`endDate`, `days_ago`), переключением тёмной/светлой темы, просмотром live-логов (`/log`) и совместимыми с оригиналом маршрутами (`/streamers`, `/json/<streamer>`, `/json_all`).
