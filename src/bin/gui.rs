use eframe::egui;
use std::io::{BufRead, BufReader, Read};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use twitch_points_miner::bet::{Comparison, DelayMode, FilterCondition, FilterMetric, Strategy};
use twitch_points_miner::chat::ChatPresence;
use twitch_points_miner::config::{AccountConfig, AppConfig, FollowersOrder};
use twitch_points_miner::entities::StreamerSettings;
use twitch_points_miner::logger::{LogColor, LogLevel};
use twitch_points_miner::notify::{
    DiscordConfig, GotifyConfig, MatrixConfig, PushoverConfig, TelegramConfig, WebhookConfig,
    WebhookMethod,
};
use twitch_points_miner::settings::{MinerEvent, Priority};

struct MinerApp {
    config: AppConfig,
    config_path: PathBuf,
    selected_account: usize,
    streamer_input: String,
    blacklist_input: String,
    child: Option<Child>,
    log_rx: Receiver<String>,
    log_lines: Vec<String>,
    status: String,
}

impl MinerApp {
    fn new() -> Self {
        let mut path = dirs::config_dir().unwrap_or_else(|| PathBuf::from("."));
        path.push("twitch-points-miner");
        path.push("config.json");
        let (mut config, status) = match AppConfig::load(&path) {
            Ok(config) => (config, "Настройки загружены".to_owned()),
            Err(_) => (AppConfig::default(), "Новая конфигурация".to_owned()),
        };
        config.promote_legacy_account();
        config.load_legacy_fields(0);
        let blacklist_input = config.blacklist.join(", ");
        let (_log_tx, log_rx) = mpsc::channel();
        Self {
            config,
            config_path: path,
            selected_account: 0,
            streamer_input: String::new(),
            blacklist_input,
            child: None,
            log_rx,
            log_lines: Vec::new(),
            status,
        }
    }

    fn save(&mut self) {
        self.commit_active_account();
        match self.config.save(&self.config_path) {
            Ok(()) => self.status = format!("Настройки сохранены: {}", self.config_path.display()),
            Err(error) => self.status = format!("Не удалось сохранить настройки: {error}"),
        }
    }

    fn start(&mut self) {
        self.commit_active_account();
        if self.child.is_some() {
            return;
        }
        let accounts = self.config.effective_accounts();
        if accounts.is_empty() {
            self.status = "Добавьте хотя бы один Twitch-аккаунт".into();
            return;
        }
        if accounts
            .iter()
            .any(|account| account.streamers.is_empty() && !account.followers)
        {
            self.status =
                "Для каждого аккаунта добавьте каналы или включите загрузку подписок".into();
            return;
        }
        if let Err(error) = self.config.save(&self.config_path) {
            self.status = format!("Не удалось сохранить настройки: {error}");
            return;
        }
        let executable = match std::env::current_exe() {
            Ok(path) => path.with_file_name(if cfg!(windows) {
                "twitch-points-miner.exe"
            } else {
                "twitch-points-miner"
            }),
            Err(error) => {
                self.status = format!("Не удалось определить путь к CLI: {error}");
                return;
            }
        };
        let (log_tx, log_rx) = mpsc::channel();
        match Command::new(executable)
            .arg("--config")
            .arg(&self.config_path)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
        {
            Ok(mut child) => {
                forward_output(child.stdout.take(), log_tx.clone());
                forward_output(child.stderr.take(), log_tx);
                self.log_rx = log_rx;
                self.log_lines.clear();
                self.child = Some(child);
                self.status = "Майнер запущен. Для первого входа подтвердите код в Twitch.".into();
            }
            Err(error) => self.status = format!("Не удалось запустить CLI: {error}"),
        }
    }

    fn stop(&mut self) {
        if let Some(mut child) = self.child.take() {
            match child.kill() {
                Ok(()) => self.status = "Майнер остановлен".into(),
                Err(error) => self.status = format!("Не удалось остановить майнер: {error}"),
            }
            let _ = child.wait();
        }
    }

    fn add_streamer(&mut self) {
        let name = self
            .streamer_input
            .trim()
            .trim_start_matches('@')
            .to_lowercase();
        if !name.is_empty()
            && !self
                .config
                .streamers
                .iter()
                .any(|item| item.eq_ignore_ascii_case(&name))
        {
            let default_profile = self.config.default_streamer_settings.clone();
            self.config
                .streamer_settings
                .entry(name.clone())
                .or_insert(default_profile);
            self.config.streamers.push(name);
            self.streamer_input.clear();
        }
    }

    fn sync_blacklist(&mut self) {
        self.config.blacklist = self
            .blacklist_input
            .split(',')
            .map(|item| item.trim().trim_start_matches('@').to_lowercase())
            .filter(|item| !item.is_empty())
            .collect();
    }

    fn commit_active_account(&mut self) {
        self.sync_blacklist();
        self.config.save_legacy_fields(self.selected_account);
    }

    fn switch_account(&mut self, index: usize) {
        if index >= self.config.accounts.len() || index == self.selected_account {
            return;
        }
        self.commit_active_account();
        self.selected_account = index;
        self.config.load_legacy_fields(index);
        self.blacklist_input = self.config.blacklist.join(", ");
    }

    fn add_account(&mut self) {
        self.commit_active_account();
        self.config.accounts.push(AccountConfig::default());
        self.selected_account = self.config.accounts.len() - 1;
        self.config.load_legacy_fields(self.selected_account);
        self.blacklist_input.clear();
    }

    fn remove_account(&mut self) {
        if self.config.accounts.len() <= 1 {
            self.config.accounts[0] = AccountConfig::default();
            self.selected_account = 0;
        } else {
            self.config.accounts.remove(self.selected_account);
            self.selected_account = self.selected_account.min(self.config.accounts.len() - 1);
        }
        self.config.load_legacy_fields(self.selected_account);
        self.blacklist_input = self.config.blacklist.join(", ");
    }
}

impl eframe::App for MinerApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        while let Ok(line) = self.log_rx.try_recv() {
            self.log_lines.push(line);
            if self.log_lines.len() > 800 {
                self.log_lines.remove(0);
            }
        }
        if let Some(child) = self.child.as_mut() {
            match child.try_wait() {
                Ok(Some(exit)) => {
                    self.child = None;
                    self.status = format!("Майнер завершился ({exit})");
                }
                Err(error) => {
                    self.child = None;
                    self.status = format!("Не удалось проверить процесс: {error}");
                }
                _ => {}
            }
        }

        egui::CentralPanel::default().show(ctx, |ui| {
            egui::ScrollArea::vertical()
                .id_salt("main_scroll")
                .show(ui, |ui| {
                    ui.heading("Twitch Channel Points Miner (Rust)");
                    ui.label("Полный порт Twitch-Channel-Points-Miner-v2: профили, автоставки, Drops, приоритеты, аналитика и уведомления.");
                    ui.add_space(10.0);

                    let account_labels: Vec<String> = self
                        .config
                        .accounts
                        .iter()
                        .enumerate()
                        .map(|(index, account)| {
                            let name = if account.username.trim().is_empty() {
                                "Новый аккаунт"
                            } else {
                                &account.username
                            };
                            format!("{}. {}", index + 1, name)
                        })
                        .collect();
                    let mut switch_to = None;
                    let mut add_account = false;
                    let mut remove_account = false;
                    ui.horizontal(|ui| {
                        ui.label("Профиль аккаунта");
                        egui::ComboBox::from_id_salt("accounts")
                            .selected_text(
                                account_labels
                                    .get(self.selected_account)
                                    .cloned()
                                    .unwrap_or_default(),
                            )
                            .show_ui(ui, |ui| {
                                for (index, label) in account_labels.iter().enumerate() {
                                    if ui
                                        .selectable_label(index == self.selected_account, label)
                                        .clicked()
                                    {
                                        switch_to = Some(index);
                                    }
                                }
                            });
                        if ui.button("+ Аккаунт").clicked() {
                            add_account = true;
                        }
                        if ui.button("Удалить").clicked() {
                            remove_account = true;
                        }
                    });
                    if let Some(index) = switch_to {
                        self.switch_account(index);
                    }
                    if add_account {
                        self.add_account();
                    }
                    if remove_account {
                        self.remove_account();
                    }

                    egui::Grid::new("account_settings")
                        .num_columns(2)
                        .spacing([12.0, 8.0])
                        .show(ui, |ui| {
                            ui.label("Twitch-аккаунт");
                            ui.text_edit_singleline(&mut self.config.username);
                            ui.end_row();

                            ui.label("Присутствие в чате");
                            egui::ComboBox::from_id_salt("chat_presence")
                                .selected_text(chat_label(self.config.chat))
                                .show_ui(ui, |ui| {
                                    for mode in [
                                        ChatPresence::Never,
                                        ChatPresence::Online,
                                        ChatPresence::Offline,
                                        ChatPresence::Always,
                                    ] {
                                        ui.selectable_value(
                                            &mut self.config.chat,
                                            mode,
                                            chat_label(mode),
                                        );
                                    }
                                });
                            ui.end_row();
                        });

                    ui.add_space(8.0);
                    egui::CollapsingHeader::new("Шаблон настроек каналов по умолчанию")
                        .id_salt("default_streamer_settings")
                        .show(ui, |ui| {
                            ui.small("Применяется ко всем каналам из подписок и новым каналам без индивидуального переопределения.");
                            render_streamer_settings_ui(
                                ui,
                                "__default_template__",
                                &mut self.config.default_streamer_settings,
                            );
                        });

                    ui.add_space(12.0);
                    ui.horizontal(|ui| {
                        ui.heading("Каналы — ручной список");
                        ui.add_space(8.0);
                        let input = ui.add(
                            egui::TextEdit::singleline(&mut self.streamer_input)
                                .hint_text("имя канала"),
                        );
                        if input.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                            self.add_streamer();
                        }
                        if ui.button("Добавить").clicked() {
                            self.add_streamer();
                        }
                    });
                    ui.small("Кнопки ↑/↓ меняют порядок списка. Он используется правилом «Ручной список каналов» ниже.");

                    egui::ScrollArea::vertical()
                        .id_salt("channels_scroll")
                        .max_height(220.0)
                        .show(ui, |ui| {
                            let channels = self.config.streamers.clone();
                            let default_template = self.config.default_streamer_settings.clone();
                            let mut remove = None;
                            let mut move_channel = None;
                            for (index, channel) in channels.iter().enumerate() {
                                ui.horizontal(|ui| {
                                    egui::CollapsingHeader::new(format!("twitch.tv/{channel}"))
                                        .id_salt(channel)
                                        .show(ui, |ui| {
                                            let profile = self
                                                .config
                                                .streamer_settings
                                                .entry(channel.clone())
                                                .or_insert_with(|| default_template.clone());
                                            render_streamer_settings_ui(ui, channel, profile);
                                        });
                                    if ui.small_button("Удалить").clicked() {
                                        remove = Some(index);
                                    }
                                    if ui
                                        .add_enabled(index > 0, egui::Button::new("↑"))
                                        .on_hover_text("Поднять канал в ручном списке")
                                        .clicked()
                                    {
                                        move_channel = Some((index, index - 1));
                                    }
                                    if ui
                                        .add_enabled(
                                            index + 1 < channels.len(),
                                            egui::Button::new("↓"),
                                        )
                                        .on_hover_text("Опустить канал в ручном списке")
                                        .clicked()
                                    {
                                        move_channel = Some((index, index + 1));
                                    }
                                });
                            }
                            if let Some((from, to)) = move_channel {
                                self.config.streamers.swap(from, to);
                            }
                            if let Some(index) = remove {
                                let removed = self.config.streamers.remove(index);
                                self.config.streamer_settings.remove(&removed);
                            }
                        });

                    ui.add_space(12.0);
                    ui.heading("Сбор, аналитика и приоритеты");
                    ui.checkbox(
                        &mut self.config.followers,
                        "Добавлять каналы из подписок Twitch",
                    );
                    ui.horizontal(|ui| {
                        ui.label("Сортировка загруженных подписок");
                        egui::ComboBox::from_id_salt("followers_order")
                            .selected_text(match self.config.followers_order {
                                FollowersOrder::Ascending => "Сначала старые (ASC)",
                                FollowersOrder::Descending => "Сначала новые (DESC)",
                            })
                            .show_ui(ui, |ui| {
                                ui.selectable_value(
                                    &mut self.config.followers_order,
                                    FollowersOrder::Ascending,
                                    "Сначала старые (ASC)",
                                );
                                ui.selectable_value(
                                    &mut self.config.followers_order,
                                    FollowersOrder::Descending,
                                    "Сначала новые (DESC)",
                                );
                            });
                    });
                    ui.horizontal(|ui| {
                        ui.label("Исключить каналы (через запятую)");
                        ui.text_edit_singleline(&mut self.blacklist_input);
                    });
                    ui.checkbox(
                        &mut self.config.claim_drops,
                        "Забирать готовые Drops из инвентаря при запуске",
                    );
                    ui.checkbox(&mut self.config.analytics, "Включить веб-аналитику и запись истории");
                    if self.config.analytics {
                        ui.horizontal(|ui| {
                            ui.label("Адрес панели");
                            ui.text_edit_singleline(&mut self.config.analytics_bind);
                        });
                        ui.horizontal(|ui| {
                            ui.label("Автообновление графика, мин");
                            ui.add(egui::Slider::new(
                                &mut self.config.analytics_refresh_minutes,
                                1..=60,
                            ));
                            ui.label("Период по умолчанию, дней");
                            ui.add(egui::Slider::new(
                                &mut self.config.analytics_days_ago,
                                1..=90,
                            ));
                        });
                    }

                    ui.horizontal(|ui| {
                        ui.label("Одновременно смотреть каналов");
                        ui.add(egui::Slider::new(
                            &mut self.config.miner.max_watch_concurrent,
                            1..=2,
                        ));
                    });
                    ui.horizontal(|ui| {
                        ui.label("Цикл minute-watched, сек");
                        ui.add(egui::Slider::new(
                            &mut self.config.miner.watch_interval_secs,
                            10..=120,
                        ));
                    });
                    ui.horizontal(|ui| {
                        ui.label("Обновлять статус эфира, сек");
                        ui.add(egui::Slider::new(
                            &mut self.config.miner.status_refresh_secs,
                            30..=600,
                        ));
                    });
                    ui.horizontal(|ui| {
                        ui.label("Обновлять дашборд кампаний Drops, сек");
                        ui.add(egui::Slider::new(
                            &mut self.config.miner.drops_claim_interval_secs,
                            300..=7200,
                        ));
                    });
                    ui.horizontal(|ui| {
                        ui.label("Обновлять контекст баллов (ChannelPointsContext), сек");
                        ui.add(egui::Slider::new(
                            &mut self.config.miner.context_refresh_secs,
                            300..=7200,
                        ));
                    });
                    ui.checkbox(
                        &mut self.config.miner.disable_at_in_nickname,
                        "Искать упоминания ника в чате даже без символа @",
                    );
                    ui.checkbox(
                        &mut self.config.miner.disable_ssl_cert_verification,
                        "Отключить проверку SSL-сертификатов (на свой риск)",
                    );

                    ui.add_space(10.0);
                    ui.heading("Очередь выбора каналов");
                    ui.small("Правила проверяются сверху вниз до заполнения лимита одновременного просмотра.");
                    let priorities = self.config.miner.priority.clone();
                    let mut move_item = None;
                    let mut remove_item = None;
                    for (index, priority) in priorities.iter().copied().enumerate() {
                        ui.horizontal(|ui| {
                            ui.label(format!("{}. {}", index + 1, priority_label(priority)))
                                .on_hover_text(priority_help(priority));
                            if ui.add_enabled(index > 0, egui::Button::new("↑")).clicked() {
                                move_item = Some((index, index - 1));
                            }
                            if ui
                                .add_enabled(index + 1 < priorities.len(), egui::Button::new("↓"))
                                .clicked()
                            {
                                move_item = Some((index, index + 1));
                            }
                            if ui.small_button("Удалить").clicked() {
                                remove_item = Some(index);
                            }
                        });
                    }
                    if let Some((from, to)) = move_item {
                        self.config.miner.priority.swap(from, to);
                    }
                    if let Some(index) = remove_item {
                        self.config.miner.priority.remove(index);
                    }
                    ui.label("Добавить правило в конец очереди:");
                    ui.horizontal_wrapped(|ui| {
                        for priority in [
                            Priority::Streak,
                            Priority::Drops,
                            Priority::Subscribed,
                            Priority::Order,
                            Priority::PointsAscending,
                            Priority::PointsDescending,
                        ] {
                            if !self.config.miner.priority.contains(&priority)
                                && ui
                                    .small_button(format!("+ {}", priority_label(priority)))
                                    .clicked()
                            {
                                self.config.miner.priority.push(priority);
                            }
                        }
                    });

                    ui.add_space(12.0);
                    egui::CollapsingHeader::new("Логирование и цветовая палитра (LoggerSettings)")
                        .id_salt("logger_settings")
                        .show(ui, |ui| {
                            let logger = &mut self.config.logger;
                            ui.checkbox(&mut logger.save, "Сохранять логи в файл (logs/<user>.log)");
                            ui.checkbox(
                                &mut logger.auto_clear,
                                "Ежедневная ротация логов (хранить 7 последних дней)",
                            );
                            ui.checkbox(&mut logger.less, "Краткий формат логов и отчёта (less)");
                            ui.checkbox(&mut logger.emoji, "Показывать эмодзи в логах");
                            ui.checkbox(&mut logger.colored, "Цветной вывод в консоль");
                            ui.checkbox(
                                &mut logger.console_username,
                                "Добавлять префикс [username] в консоль и уведомления",
                            );
                            ui.horizontal(|ui| {
                                ui.label("Уровень консоли");
                                egui::ComboBox::from_id_salt("console_level")
                                    .selected_text(logger.console_level.as_str())
                                    .show_ui(ui, |ui| {
                                        for lvl in [
                                            LogLevel::Debug,
                                            LogLevel::Info,
                                            LogLevel::Warn,
                                            LogLevel::Error,
                                        ] {
                                            ui.selectable_value(
                                                &mut logger.console_level,
                                                lvl,
                                                lvl.as_str(),
                                            );
                                        }
                                    });
                                ui.label("Уровень файла");
                                egui::ComboBox::from_id_salt("file_level")
                                    .selected_text(logger.file_level.as_str())
                                    .show_ui(ui, |ui| {
                                        for lvl in [
                                            LogLevel::Debug,
                                            LogLevel::Info,
                                            LogLevel::Warn,
                                            LogLevel::Error,
                                        ] {
                                            ui.selectable_value(
                                                &mut logger.file_level,
                                                lvl,
                                                lvl.as_str(),
                                            );
                                        }
                                    });
                            });
                            ui.horizontal(|ui| {
                                ui.label("Часовой пояс");
                                ui.add(
                                    egui::TextEdit::singleline(&mut logger.time_zone)
                                        .hint_text("напр. Europe/Moscow или +03:00"),
                                );
                            });
                            egui::CollapsingHeader::new("Палитра цветов по событиям")
                                .id_salt("color_palette")
                                .show(ui, |ui| {
                                    for ev in MinerEvent::ALL {
                                        let mut current = logger
                                            .color_palette
                                            .colors
                                            .get(&ev)
                                            .copied()
                                            .unwrap_or(LogColor::Reset);
                                        let prev = current;
                                        ui.horizontal(|ui| {
                                            ui.label(ev.as_str());
                                            egui::ComboBox::from_id_salt(("palette", ev.as_str()))
                                                .selected_text(color_label(current))
                                                .show_ui(ui, |ui| {
                                                    for c in [
                                                        LogColor::Reset,
                                                        LogColor::Green,
                                                        LogColor::Red,
                                                        LogColor::Yellow,
                                                        LogColor::Blue,
                                                        LogColor::Magenta,
                                                        LogColor::Cyan,
                                                        LogColor::White,
                                                        LogColor::Black,
                                                    ] {
                                                        ui.selectable_value(
                                                            &mut current,
                                                            c,
                                                            color_label(c),
                                                        );
                                                    }
                                                });
                                        });
                                        if current != prev {
                                            if current == LogColor::Reset {
                                                logger.color_palette.colors.remove(&ev);
                                            } else {
                                                logger.color_palette.colors.insert(ev, current);
                                            }
                                        }
                                    }
                                });
                        });

                    ui.add_space(10.0);
                    egui::CollapsingHeader::new("Уведомления (Telegram, Discord, Webhook, Gotify, Matrix, Pushover)")
                        .id_salt("notifications_panel")
                        .show(ui, |ui| {
                            let notifications = &mut self.config.notifications;

                            // Telegram
                            let mut telegram_enabled = notifications.telegram.is_some();
                            ui.checkbox(&mut telegram_enabled, "Telegram");
                            if telegram_enabled {
                                let tg = notifications
                                    .telegram
                                    .get_or_insert_with(TelegramConfig::default);
                                ui.horizontal(|ui| {
                                    ui.label("Bot Token");
                                    ui.add(
                                        egui::TextEdit::singleline(&mut tg.bot_token).password(true),
                                    );
                                });
                                ui.horizontal(|ui| {
                                    ui.label("Chat ID");
                                    ui.text_edit_singleline(&mut tg.chat_id);
                                });
                                ui.checkbox(&mut tg.disable_notification, "Без звука (disable_notification)");
                                render_event_filter_ui(ui, "tg_events", &mut tg.events);
                            } else {
                                notifications.telegram = None;
                            }

                            // Discord
                            let mut discord_enabled = notifications.discord.is_some();
                            ui.checkbox(&mut discord_enabled, "Discord Webhook");
                            if discord_enabled {
                                let dc = notifications
                                    .discord
                                    .get_or_insert_with(DiscordConfig::default);
                                ui.horizontal(|ui| {
                                    ui.label("Webhook URL");
                                    ui.text_edit_singleline(&mut dc.webhook_url);
                                });
                                render_event_filter_ui(ui, "dc_events", &mut dc.events);
                            } else {
                                notifications.discord = None;
                            }

                            // Webhook
                            let mut webhook_enabled = notifications.webhook.is_some();
                            ui.checkbox(&mut webhook_enabled, "HTTP Webhook (GET / POST)");
                            if webhook_enabled {
                                let wh = notifications
                                    .webhook
                                    .get_or_insert_with(WebhookConfig::default);
                                ui.horizontal(|ui| {
                                    ui.label("URL");
                                    ui.text_edit_singleline(&mut wh.url);
                                    egui::ComboBox::from_id_salt("webhook_method")
                                        .selected_text(match wh.method {
                                            WebhookMethod::Get => "GET",
                                            WebhookMethod::Post => "POST",
                                        })
                                        .show_ui(ui, |ui| {
                                            ui.selectable_value(
                                                &mut wh.method,
                                                WebhookMethod::Get,
                                                "GET",
                                            );
                                            ui.selectable_value(
                                                &mut wh.method,
                                                WebhookMethod::Post,
                                                "POST",
                                            );
                                        });
                                });
                                render_event_filter_ui(ui, "wh_events", &mut wh.events);
                            } else {
                                notifications.webhook = None;
                            }

                            // Gotify
                            let mut gotify_enabled = notifications.gotify.is_some();
                            ui.checkbox(&mut gotify_enabled, "Gotify");
                            if gotify_enabled {
                                let gotify = notifications
                                    .gotify
                                    .get_or_insert_with(GotifyConfig::default);
                                ui.horizontal(|ui| {
                                    ui.label("URL");
                                    ui.text_edit_singleline(&mut gotify.url);
                                });
                                ui.horizontal(|ui| {
                                    ui.label("Token");
                                    ui.add(
                                        egui::TextEdit::singleline(&mut gotify.token).password(true),
                                    );
                                    ui.label("Приоритет");
                                    ui.add(egui::Slider::new(&mut gotify.priority, 0..=10));
                                });
                                render_event_filter_ui(ui, "gotify_events", &mut gotify.events);
                            } else {
                                notifications.gotify = None;
                            }

                            // Matrix
                            let mut matrix_enabled = notifications.matrix.is_some();
                            ui.checkbox(&mut matrix_enabled, "Matrix");
                            if matrix_enabled {
                                let mx = notifications
                                    .matrix
                                    .get_or_insert_with(MatrixConfig::default);
                                ui.horizontal(|ui| {
                                    ui.label("Homeserver");
                                    ui.text_edit_singleline(&mut mx.homeserver);
                                    ui.label("Room ID");
                                    ui.text_edit_singleline(&mut mx.room_id);
                                });
                                ui.horizontal(|ui| {
                                    ui.label("Username");
                                    ui.text_edit_singleline(&mut mx.username);
                                    ui.label("Password");
                                    ui.add(
                                        egui::TextEdit::singleline(&mut mx.password).password(true),
                                    );
                                });
                                render_event_filter_ui(ui, "matrix_events", &mut mx.events);
                            } else {
                                notifications.matrix = None;
                            }

                            // Pushover
                            let mut pushover_enabled = notifications.pushover.is_some();
                            ui.checkbox(&mut pushover_enabled, "Pushover");
                            if pushover_enabled {
                                let po = notifications
                                    .pushover
                                    .get_or_insert_with(PushoverConfig::default);
                                ui.horizontal(|ui| {
                                    ui.label("User Key");
                                    ui.add(
                                        egui::TextEdit::singleline(&mut po.userkey).password(true),
                                    );
                                    ui.label("App Token");
                                    ui.add(
                                        egui::TextEdit::singleline(&mut po.token).password(true),
                                    );
                                });
                                ui.horizontal(|ui| {
                                    ui.label("Приоритет");
                                    ui.add(egui::Slider::new(&mut po.priority, -2..=2));
                                    ui.label("Звук");
                                    ui.text_edit_singleline(&mut po.sound);
                                });
                                render_event_filter_ui(ui, "pushover_events", &mut po.events);
                            } else {
                                notifications.pushover = None;
                            }

                            ui.small("Секреты уведомлений сохраняются в локальном config.json.");
                        });

                    ui.add_space(14.0);
                    ui.horizontal(|ui| {
                        if ui.button("Сохранить").clicked() {
                            self.save();
                        }
                        if self.child.is_some() {
                            if ui.button("Остановить").clicked() {
                                self.stop();
                            }
                        } else if ui.button("Запустить майнер").clicked() {
                            self.start();
                        }
                    });
                    ui.label(&self.status);
                    egui::CollapsingHeader::new("Журнал майнера")
                        .default_open(true)
                        .show(ui, |ui| {
                            egui::ScrollArea::vertical()
                                .id_salt("logs_scroll")
                                .max_height(160.0)
                                .stick_to_bottom(true)
                                .show(ui, |ui| {
                                    for line in &self.log_lines {
                                        ui.label(egui::RichText::new(line).monospace().size(11.0));
                                    }
                                });
                        });
                });
        });
        ctx.request_repaint_after(std::time::Duration::from_millis(500));
    }
}

fn render_streamer_settings_ui(
    ui: &mut egui::Ui,
    id_key: &str,
    profile: &mut StreamerSettings,
) {
    ui.checkbox(&mut profile.make_predictions, "Автоставки на прогнозы");
    ui.checkbox(&mut profile.follow_raid, "Переходить по рейдам (+250 очков)");
    ui.checkbox(&mut profile.claim_drops, "Отслеживать и забирать Drops");
    ui.checkbox(&mut profile.claim_moments, "Забирать Moments");
    ui.checkbox(&mut profile.watch_streak, "Приоритет серии просмотра (Watch Streak)");
    ui.checkbox(
        &mut profile.community_goals,
        "Вносить баллы в цели сообщества (Community Goals)",
    );
    egui::ComboBox::from_id_salt(("streamer_chat", id_key))
        .selected_text(profile.chat.map(chat_label).unwrap_or("Общая настройка аккаунта"))
        .show_ui(ui, |ui| {
            ui.selectable_value(&mut profile.chat, None, "Общая настройка аккаунта");
            ui.selectable_value(&mut profile.chat, Some(ChatPresence::Never), "Чат: никогда");
            ui.selectable_value(
                &mut profile.chat,
                Some(ChatPresence::Online),
                "Чат: когда в эфире",
            );
            ui.selectable_value(
                &mut profile.chat,
                Some(ChatPresence::Offline),
                "Чат: когда не в эфире",
            );
            ui.selectable_value(
                &mut profile.chat,
                Some(ChatPresence::Always),
                "Чат: всегда",
            );
        });

    egui::CollapsingHeader::new("Автоставки: стратегия, фильтры и задержка")
        .id_salt(("bet_settings", id_key))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label("Стратегия выбора исхода");
                egui::ComboBox::from_id_salt(("bet_strategy", id_key))
                    .selected_text(strategy_label(profile.bet.strategy))
                    .show_ui(ui, |ui| {
                        for strategy in [
                            Strategy::Smart,
                            Strategy::MostVoted,
                            Strategy::HighOdds,
                            Strategy::Percentage,
                            Strategy::SmartMoney,
                            Strategy::Number1,
                            Strategy::Number2,
                            Strategy::Number3,
                            Strategy::Number4,
                            Strategy::Number5,
                            Strategy::Number6,
                            Strategy::Number7,
                            Strategy::Number8,
                        ] {
                            ui.selectable_value(
                                &mut profile.bet.strategy,
                                strategy,
                                strategy_label(strategy),
                            );
                        }
                    });
            });
            ui.small(strategy_help(profile.bet.strategy));
            ui.add(
                egui::Slider::new(&mut profile.bet.percentage, 1..=100)
                    .text("Размер ставки: % баланса"),
            );
            ui.add(
                egui::Slider::new(&mut profile.bet.max_points, 0..=250_000)
                    .text("Максимум баллов на ставку"),
            );
            ui.add(
                egui::Slider::new(&mut profile.bet.minimum_points, 0..=100_000)
                    .text("Минимальный баланс для участия"),
            );
            if profile.bet.strategy == Strategy::Smart {
                ui.add(
                    egui::Slider::new(&mut profile.bet.percentage_gap, 0..=100)
                        .text("Порог Smart: разница долей зрителей, п.п."),
                );
            }
            ui.checkbox(
                &mut profile.bet.stealth_mode,
                "Stealth-режим: ставить чуть ниже крупнейшей чужой ставки (TOP_POINTS)",
            )
            .on_hover_text(
                "Если рассчитанная ставка превышает топ-ставку выбранного исхода, уменьшает её на 1–4 балла, чтобы ваш ник не светился на 1-м месте в чате.",
            );

            let mut filter_enabled = profile.bet.filter_condition.is_some();
            ui.checkbox(&mut filter_enabled, "Фильтр условия ставки (FilterCondition)");
            if filter_enabled {
                let filter = profile.bet.filter_condition.get_or_insert(FilterCondition {
                    metric: FilterMetric::TotalUsers,
                    comparison: Comparison::GreaterOrEqual,
                    value: 20.0,
                });
                egui::ComboBox::from_id_salt(("filter_metric", id_key))
                    .selected_text(metric_label(filter.metric))
                    .show_ui(ui, |ui| {
                        for metric in [
                            FilterMetric::UsersPercentage,
                            FilterMetric::OddsPercentage,
                            FilterMetric::Odds,
                            FilterMetric::DecisionUsers,
                            FilterMetric::DecisionPoints,
                            FilterMetric::TopPoints,
                            FilterMetric::TotalUsers,
                            FilterMetric::TotalPoints,
                        ] {
                            ui.selectable_value(&mut filter.metric, metric, metric_label(metric));
                        }
                    });
                ui.horizontal(|ui| {
                    egui::ComboBox::from_id_salt(("filter_comparison", id_key))
                        .selected_text(comparison_label(filter.comparison))
                        .show_ui(ui, |ui| {
                            for comparison in [
                                Comparison::GreaterThan,
                                Comparison::LessThan,
                                Comparison::GreaterOrEqual,
                                Comparison::LessOrEqual,
                            ] {
                                ui.selectable_value(
                                    &mut filter.comparison,
                                    comparison,
                                    comparison_label(comparison),
                                );
                            }
                        });
                    ui.add(egui::DragValue::new(&mut filter.value).speed(0.5));
                });
            } else {
                profile.bet.filter_condition = None;
            }

            ui.horizontal(|ui| {
                ui.label("Момент ставки");
                egui::ComboBox::from_id_salt(("delay_mode", id_key))
                    .selected_text(delay_label(profile.bet.delay_mode))
                    .show_ui(ui, |ui| {
                        ui.selectable_value(
                            &mut profile.bet.delay_mode,
                            DelayMode::FromStart,
                            "Через N сек после начала (FROM_START)",
                        );
                        ui.selectable_value(
                            &mut profile.bet.delay_mode,
                            DelayMode::FromEnd,
                            "За N сек до закрытия (FROM_END)",
                        );
                        ui.selectable_value(
                            &mut profile.bet.delay_mode,
                            DelayMode::Percentage,
                            "Доля таймера (PERCENTAGE)",
                        );
                    });
                let range = if profile.bet.delay_mode == DelayMode::Percentage {
                    0.0..=1.0
                } else {
                    0.0..=120.0
                };
                ui.add(egui::Slider::new(&mut profile.bet.delay, range).text(
                    if profile.bet.delay_mode == DelayMode::Percentage {
                        "доля"
                    } else {
                        "сек"
                    },
                ));
            });
        });
}

fn render_event_filter_ui(ui: &mut egui::Ui, id_salt: &str, events: &mut Vec<MinerEvent>) {
    egui::CollapsingHeader::new(format!("Фильтр событий ({}/{})", events.len(), MinerEvent::ALL.len()))
        .id_salt(id_salt)
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                if ui.small_button("Выбрать все").clicked() {
                    *events = MinerEvent::all_vec();
                }
                if ui.small_button("Снять все").clicked() {
                    events.clear();
                }
            });
            ui.horizontal_wrapped(|ui| {
                for ev in MinerEvent::ALL {
                    let mut checked = events.contains(&ev);
                    if ui.checkbox(&mut checked, ev.as_str()).changed() {
                        if checked {
                            if !events.contains(&ev) {
                                events.push(ev);
                            }
                        } else {
                            events.retain(|x| *x != ev);
                        }
                    }
                }
            });
        });
}

fn color_label(c: LogColor) -> &'static str {
    match c {
        LogColor::Reset => "По умолчанию (RESET)",
        LogColor::Black => "Чёрный",
        LogColor::Red => "Красный",
        LogColor::Green => "Зелёный",
        LogColor::Yellow => "Жёлтый",
        LogColor::Blue => "Синий",
        LogColor::Magenta => "Пурпурный",
        LogColor::Cyan => "Голубой",
        LogColor::White => "Белый",
    }
}

fn priority_label(priority: Priority) -> &'static str {
    match priority {
        Priority::Streak => "Серия просмотра (Watch Streak)",
        Priority::Drops => "Активные кампании Drops",
        Priority::Subscribed => "Подписка / множитель очков (Subscribed)",
        Priority::Order => "Ручной список каналов (Order)",
        Priority::PointsAscending => "Сначала меньше баллов (Points Ascending)",
        Priority::PointsDescending => "Сначала больше баллов (Points Descending)",
    }
}

fn priority_help(priority: Priority) -> &'static str {
    match priority {
        Priority::Streak => "Удерживает недавно начавшиеся стримы (до 7 минут или получения бонуса серии), если перерыв между эфирами >30 минут.",
        Priority::Drops => "Выбирает каналы с подтверждёнными активными кампаниями Twitch Drops для текущей игры.",
        Priority::Subscribed => "Выбирает каналы с активным множителем очков (подписка), сортируя по величине множителя по убыванию.",
        Priority::Order => "Выбирает каналы сверху вниз по ручному списку, затем по загруженным подпискам.",
        Priority::PointsAscending => "Сначала канал с наименьшим балансом баллов.",
        Priority::PointsDescending => "Сначала канал с наибольшим балансом баллов.",
    }
}

fn chat_label(mode: ChatPresence) -> &'static str {
    match mode {
        ChatPresence::Never => "Никогда",
        ChatPresence::Online => "Когда в эфире",
        ChatPresence::Offline => "Когда не в эфире",
        ChatPresence::Always => "Всегда",
    }
}

fn forward_output<R: Read + Send + 'static>(output: Option<R>, tx: mpsc::Sender<String>) {
    if let Some(output) = output {
        std::thread::spawn(move || {
            for line in BufReader::new(output).lines().map_while(Result::ok) {
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
    }
}

fn strategy_label(strategy: Strategy) -> &'static str {
    match strategy {
        Strategy::Smart => "Smart: большинство зрителей или высокий кэф",
        Strategy::MostVoted => "Most Voted: больше всего участников",
        Strategy::HighOdds => "High Odds: наибольший коэффициент",
        Strategy::Percentage => "Percentage: наибольший % вероятности Twitch",
        Strategy::SmartMoney => "Smart Money: крупнейшая топ-ставка (TOP_POINTS)",
        Strategy::Number1 => "Всегда вариант №1 (BLUE)",
        Strategy::Number2 => "Всегда вариант №2 (PINK)",
        Strategy::Number3 => "Всегда вариант №3",
        Strategy::Number4 => "Всегда вариант №4",
        Strategy::Number5 => "Всегда вариант №5",
        Strategy::Number6 => "Всегда вариант №6",
        Strategy::Number7 => "Всегда вариант №7",
        Strategy::Number8 => "Всегда вариант №8",
    }
}

fn strategy_help(strategy: Strategy) -> &'static str {
    match strategy {
        Strategy::Smart => "Если разница долей участников >= порога percentage_gap — ставит за большинство, иначе выбирает наибольший коэффициент.",
        Strategy::MostVoted => "Выбирает исход с наибольшим количеством проголосовавших зрителей.",
        Strategy::HighOdds => "Выбирает исход с максимальным коэффициентом выплаты.",
        Strategy::Percentage => "Выбирает исход с максимальным процентом по коэффициенту (ODDS_PERCENTAGE).",
        Strategy::SmartMoney => "Выбирает исход с самой крупной одиночной ставкой среди top_predictors.",
        Strategy::Number1 => "Всегда выбирает 1-й исход.",
        Strategy::Number2 => "Всегда выбирает 2-й исход.",
        Strategy::Number3 => "Выбирает 3-й исход (или 1-й, если вариантов меньше).",
        Strategy::Number4 => "Выбирает 4-й исход (или 1-й, если вариантов меньше).",
        Strategy::Number5 => "Выбирает 5-й исход (или 1-й, если вариантов меньше).",
        Strategy::Number6 => "Выбирает 6-й исход (или 1-й, если вариантов меньше).",
        Strategy::Number7 => "Выбирает 7-й исход (или 1-й, если вариантов меньше).",
        Strategy::Number8 => "Выбирает 8-й исход (или 1-й, если вариантов меньше).",
    }
}

fn metric_label(metric: FilterMetric) -> &'static str {
    match metric {
        FilterMetric::UsersPercentage => "PERCENTAGE_USERS (% участников исхода)",
        FilterMetric::OddsPercentage => "ODDS_PERCENTAGE (% по коэффициенту)",
        FilterMetric::Odds => "ODDS (коэффициент выбранного исхода)",
        FilterMetric::DecisionUsers | FilterMetric::OutcomeUsers => {
            "DECISION_USERS (участники выбранного исхода)"
        }
        FilterMetric::DecisionPoints | FilterMetric::OutcomePoints => {
            "DECISION_POINTS (баллы на выбранном исходе)"
        }
        FilterMetric::TopPoints => "TOP_POINTS (крупнейшая ставка на выбранном исходе)",
        FilterMetric::TotalUsers => "TOTAL_USERS (всего участников прогноза)",
        FilterMetric::TotalPoints => "TOTAL_POINTS (всего баллов в банке)",
    }
}

fn comparison_label(comparison: Comparison) -> &'static str {
    match comparison {
        Comparison::GreaterThan => "> (GT)",
        Comparison::LessThan => "< (LT)",
        Comparison::GreaterOrEqual => "≥ (GTE)",
        Comparison::LessOrEqual => "≤ (LTE)",
    }
}

fn delay_label(mode: DelayMode) -> &'static str {
    match mode {
        DelayMode::FromStart => "После старта",
        DelayMode::FromEnd => "До закрытия",
        DelayMode::Percentage => "Доля таймера",
    }
}

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([680.0, 760.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Twitch Channel Points Miner",
        options,
        Box::new(|_cc| Ok(Box::new(MinerApp::new()))),
    )
}
