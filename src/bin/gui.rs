use eframe::egui;
use std::io::{BufRead, BufReader, Read};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use twitch_points_miner::bet::{Comparison, DelayMode, FilterCondition, FilterMetric, Strategy};
use twitch_points_miner::chat::ChatPresence;
use twitch_points_miner::config::{AccountConfig, AppConfig, FollowersOrder};
use twitch_points_miner::entities::StreamerSettings;
use twitch_points_miner::notify::{DiscordConfig, GotifyConfig, TelegramConfig, WebhookConfig};
use twitch_points_miner::settings::Priority;

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
                forward_output(child.stderr.take(), log_tx.clone());
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
            if self.log_lines.len() > 500 {
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
            ui.heading("Twitch Points Miner");
            ui.label("Управляйте каналами и запускайте майнер из одного окна.");
            ui.add_space(12.0);

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
                        .selected_text(match self.config.chat {
                            ChatPresence::Never => "Никогда",
                            ChatPresence::Online => "Когда канал в эфире",
                            ChatPresence::Offline => "Когда канал не в эфире",
                            ChatPresence::Always => "Всегда",
                        })
                        .show_ui(ui, |ui| {
                            ui.selectable_value(
                                &mut self.config.chat,
                                ChatPresence::Never,
                                "Никогда",
                            );
                            ui.selectable_value(
                                &mut self.config.chat,
                                ChatPresence::Online,
                                "Когда канал в эфире",
                            );
                            ui.selectable_value(
                                &mut self.config.chat,
                                ChatPresence::Offline,
                                "Когда канал не в эфире",
                            );
                            ui.selectable_value(
                                &mut self.config.chat,
                                ChatPresence::Always,
                                "Всегда",
                            );
                        });
                    ui.end_row();
                });

            ui.add_space(14.0);
            ui.horizontal(|ui| {
            ui.heading("Каналы — ручной список");
                ui.add_space(8.0);
                let input = ui.add(
                    egui::TextEdit::singleline(&mut self.streamer_input).hint_text("имя канала"),
                );
                if input.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    self.add_streamer();
                }
                if ui.button("Добавить").clicked() {
                    self.add_streamer();
                }
            });
            ui.small("Кнопки ↑/↓ меняют порядок списка. Он используется правилом «Порядок списка» ниже.");
            egui::ScrollArea::vertical()
                .max_height(220.0)
                .show(ui, |ui| {
                    let channels = self.config.streamers.clone();
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
                                        .or_insert_with(StreamerSettings::default);
                                    ui.checkbox(
                                        &mut profile.make_predictions,
                                        "Автоставки на прогнозы",
                                    );
                                    ui.checkbox(&mut profile.follow_raid, "Переходить по рейдам");
                                    ui.checkbox(
                                        &mut profile.claim_drops,
                                        "Учитывать канал при сборе Drops",
                                    );
                                    ui.checkbox(&mut profile.claim_moments, "Забирать Moments");
                                    ui.checkbox(
                                        &mut profile.watch_streak,
                                        "Приоритет серии просмотра",
                                    );
                                    ui.checkbox(
                                        &mut profile.community_goals,
                                        "Вносить баллы в цели сообщества",
                                    );
                                    egui::ComboBox::from_id_salt(("streamer_chat", channel))
                                        .selected_text(
                                            profile
                                                .chat
                                                .map(chat_label)
                                                .unwrap_or("Общая настройка"),
                                        )
                                        .show_ui(ui, |ui| {
                                            ui.selectable_value(
                                                &mut profile.chat,
                                                None,
                                                "Общая настройка",
                                            );
                                            ui.selectable_value(
                                                &mut profile.chat,
                                                Some(ChatPresence::Never),
                                                "Чат: никогда",
                                            );
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
                                    egui::CollapsingHeader::new("Автоставки: стратегия и сумма")
                                        .id_salt(("bet_settings", channel))
                                        .show(ui, |ui| {
                                            ui.label("Эти правила применяются только к прогнозам этого канала.");
                                            ui.horizontal(|ui| {
                                                ui.label("Как выбрать сторону");
                                                egui::ComboBox::from_id_salt(("bet_strategy", channel))
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
                                                            ui.selectable_value(&mut profile.bet.strategy, strategy, strategy_label(strategy));
                                                        }
                                                    });
                                            });
                                            ui.label(strategy_help(profile.bet.strategy));
                                            ui.add(egui::Slider::new(&mut profile.bet.percentage, 1..=100)
                                                .text("Размер ставки: % баланса"));
                                            ui.add(egui::Slider::new(&mut profile.bet.max_points, 0..=100_000)
                                                .text("Лимит баллов на одну ставку"));
                                            ui.add(egui::Slider::new(&mut profile.bet.minimum_points, 0..=10_000)
                                                .text("Минимальный баланс для ставки"));
                                            ui.small("Ставка = процент баланса, но не выше лимита. Лимит 0 запрещает ставки; если баланс ниже минимума, ставка пропускается.");
                                            if profile.bet.strategy == Strategy::Smart {
                                                ui.add(egui::Slider::new(&mut profile.bet.percentage_gap, 0..=100)
                                                    .text("Порог Smart: разница долей зрителей, п.п."));
                                                ui.small("Если разрыв долей голосов больше порога — выбирается самая популярная сторона; иначе сторона с наибольшим коэффициентом.");
                                            }
                                            ui.checkbox(&mut profile.bet.stealth_mode, "Оставлять случайный запас (1–14 баллов)")
                                                .on_hover_text("Если ставка больше 10 баллов, майнер немного уменьшит её, чтобы не ставить весь рассчитанный максимум.");

                                            let mut filter_enabled = profile.bet.filter_condition.is_some();
                                            ui.checkbox(&mut filter_enabled, "Ставить только при выполнении фильтра");
                                            if filter_enabled {
                                                let filter = profile.bet.filter_condition.get_or_insert(FilterCondition {
                                                    metric: FilterMetric::UsersPercentage,
                                                    comparison: Comparison::GreaterOrEqual,
                                                    value: 0.0,
                                                });
                                                egui::ComboBox::from_id_salt(("filter_metric", channel))
                                                    .selected_text(metric_label(filter.metric))
                                                    .show_ui(ui, |ui| {
                                                        for metric in [FilterMetric::UsersPercentage, FilterMetric::OddsPercentage, FilterMetric::Odds, FilterMetric::DecisionUsers, FilterMetric::DecisionPoints, FilterMetric::OutcomeUsers, FilterMetric::OutcomePoints, FilterMetric::TopPoints, FilterMetric::TotalUsers, FilterMetric::TotalPoints] {
                                                            ui.selectable_value(&mut filter.metric, metric, metric_label(metric));
                                                        }
                                                    });
                                                ui.horizontal(|ui| {
                                                    egui::ComboBox::from_id_salt(("filter_comparison", channel))
                                                        .selected_text(comparison_label(filter.comparison))
                                                        .show_ui(ui, |ui| {
                                                            for comparison in [Comparison::GreaterThan, Comparison::LessThan, Comparison::GreaterOrEqual, Comparison::LessOrEqual] {
                                                                ui.selectable_value(&mut filter.comparison, comparison, comparison_label(comparison));
                                                            }
                                                        });
                                                    ui.add(egui::DragValue::new(&mut filter.value).speed(0.5));
                                                });
                                                ui.small("Фильтр сравнивает выбранную метрику с указанным числом. Если условие не выполнено, ставка пропускается.");
                                            } else {
                                                profile.bet.filter_condition = None;
                                            }

                                            ui.horizontal(|ui| {
                                                ui.label("Когда ставить");
                                                egui::ComboBox::from_id_salt(("delay_mode", channel))
                                                    .selected_text(delay_label(profile.bet.delay_mode))
                                                    .show_ui(ui, |ui| {
                                                        ui.selectable_value(&mut profile.bet.delay_mode, DelayMode::FromStart, "Через N секунд после старта");
                                                        ui.selectable_value(&mut profile.bet.delay_mode, DelayMode::FromEnd, "За N секунд до конца");
                                                        ui.selectable_value(&mut profile.bet.delay_mode, DelayMode::Percentage, "В указанной доле таймера");
                                                    });
                                                let range = if profile.bet.delay_mode == DelayMode::Percentage { 0.0..=1.0 } else { 0.0..=120.0 };
                                                ui.add(egui::Slider::new(&mut profile.bet.delay, range).text(if profile.bet.delay_mode == DelayMode::Percentage { "доля" } else { "секунд" }));
                                            });
                                            if profile.bet.delay_mode == DelayMode::Percentage {
                                                ui.small(format!("Например, 0,5 — середина таймера; сейчас: {:.0}% от начала.", profile.bet.delay * 100.0));
                                            }
                                        });
                                });
                            if ui.small_button("Удалить").clicked() {
                                remove = Some(index);
                            }
                            if ui.add_enabled(index > 0, egui::Button::new("↑")).on_hover_text("Поднять канал в ручном списке").clicked() {
                                move_channel = Some((index, index - 1));
                            }
                            if ui.add_enabled(index + 1 < channels.len(), egui::Button::new("↓")).on_hover_text("Опустить канал в ручном списке").clicked() {
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

            ui.add_space(14.0);
            ui.heading("Сбор и приоритеты");
            ui.checkbox(
                &mut self.config.followers,
                "Добавлять каналы из подписок Twitch",
            );
            ui.horizontal(|ui| {
                ui.label("Сортировка загруженных подписок");
                egui::ComboBox::from_id_salt("followers_order")
                    .selected_text(match self.config.followers_order {
                        FollowersOrder::Ascending => "Сначала старые",
                        FollowersOrder::Descending => "Сначала новые",
                    })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(
                            &mut self.config.followers_order,
                            FollowersOrder::Ascending,
                            "Сначала старые",
                        );
                        ui.selectable_value(
                            &mut self.config.followers_order,
                            FollowersOrder::Descending,
                            "Сначала новые",
                        );
                    });
            });
            ui.horizontal(|ui| {
                ui.label("Исключить каналы (через запятую)");
                ui.text_edit_singleline(&mut self.blacklist_input);
            });
            ui.checkbox(
                &mut self.config.claim_drops,
                "Забирать готовые Drops при запуске",
            );
            ui.checkbox(&mut self.config.analytics, "Включить аналитику");
            ui.horizontal(|ui| {
                ui.label("Адрес аналитики");
                ui.text_edit_singleline(&mut self.config.analytics_bind);
            });
            ui.horizontal(|ui| {
                ui.label("Сколько каналов смотреть одновременно");
                ui.add(egui::Slider::new(
                    &mut self.config.miner.max_watch_concurrent,
                    1..=2,
                ));
            });
            ui.horizontal(|ui| {
                ui.label("Пауза между минутами просмотра, сек");
                ui.add(egui::Slider::new(
                    &mut self.config.miner.watch_interval_secs,
                    10..=120,
                ));
            });
            ui.horizontal(|ui| {
                ui.label("Обновлять статус канала, сек");
                ui.add(egui::Slider::new(
                    &mut self.config.miner.status_refresh_secs,
                    30..=600,
                ));
            });
            ui.horizontal(|ui| {
                ui.label("Проверять Drops каждые, сек");
                ui.add(egui::Slider::new(
                    &mut self.config.miner.drops_claim_interval_secs,
                    60..=7200,
                ));
            });
            ui.heading("Очередь выбора каналов");
            ui.label("Майнер проверяет правила сверху вниз и добирает каналы до указанного лимита (не более двух). Переместите ↑/↓, чтобы задать важность правил.");
            ui.label("«Порядок списка» берёт первые каналы из ручного списка; если включены подписки, затем учитывается выбранная сортировка подписок.");
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
            egui::CollapsingHeader::new("Уведомления").show(ui, |ui| {
                let notifications = &mut self.config.notifications;
                let mut telegram_enabled = notifications.telegram.is_some();
                ui.checkbox(&mut telegram_enabled, "Telegram");
                if telegram_enabled {
                    let tg = notifications
                        .telegram
                        .get_or_insert_with(|| TelegramConfig {
                            bot_token: String::new(),
                            chat_id: String::new(),
                        });
                    ui.horizontal(|ui| {
                        ui.label("Токен");
                        ui.add(egui::TextEdit::singleline(&mut tg.bot_token).password(true));
                    });
                    ui.horizontal(|ui| {
                        ui.label("Chat ID");
                        ui.text_edit_singleline(&mut tg.chat_id);
                    });
                } else {
                    notifications.telegram = None;
                }

                let mut discord_enabled = notifications.discord.is_some();
                ui.checkbox(&mut discord_enabled, "Discord webhook");
                if discord_enabled {
                    let dc = notifications.discord.get_or_insert_with(|| DiscordConfig {
                        webhook_url: String::new(),
                    });
                    ui.text_edit_singleline(&mut dc.webhook_url);
                } else {
                    notifications.discord = None;
                }

                let mut webhook_enabled = notifications.webhook.is_some();
                ui.checkbox(&mut webhook_enabled, "HTTP webhook");
                if webhook_enabled {
                    let wh = notifications
                        .webhook
                        .get_or_insert_with(|| WebhookConfig { url: String::new() });
                    ui.text_edit_singleline(&mut wh.url);
                } else {
                    notifications.webhook = None;
                }

                let mut gotify_enabled = notifications.gotify.is_some();
                ui.checkbox(&mut gotify_enabled, "Gotify");
                if gotify_enabled {
                    let gotify = notifications.gotify.get_or_insert_with(|| GotifyConfig {
                        url: String::new(),
                        token: String::new(),
                    });
                    ui.horizontal(|ui| {
                        ui.label("URL");
                        ui.text_edit_singleline(&mut gotify.url);
                    });
                    ui.horizontal(|ui| {
                        ui.label("Token");
                        ui.add(egui::TextEdit::singleline(&mut gotify.token).password(true));
                    });
                } else {
                    notifications.gotify = None;
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
                        .max_height(145.0)
                        .stick_to_bottom(true)
                        .show(ui, |ui| {
                            for line in &self.log_lines {
                                ui.label(egui::RichText::new(line).monospace().size(11.0));
                            }
                        });
                });
        });
        ctx.request_repaint_after(std::time::Duration::from_millis(500));
    }
}

fn priority_label(priority: Priority) -> &'static str {
    match priority {
        Priority::Streak => "Активная серия просмотра",
        Priority::Drops => "Есть доступные Drops",
        Priority::Subscribed => "Подписка / множитель (не определяется)",
        Priority::Order => "Ручной список каналов",
        Priority::PointsAscending => "Сначала меньше баллов",
        Priority::PointsDescending => "Сначала больше баллов",
    }
}

fn priority_help(priority: Priority) -> &'static str {
    match priority {
        Priority::Streak => "Сначала выбираются каналы с активной серией просмотра.",
        Priority::Drops => "Затем выбираются эфиры с игрой и включённой опцией сбора Drops.",
        Priority::Subscribed => "В этой сборке нет данных об уровне подписки Twitch; это старое правило из конфига не может выбрать подписанные каналы отдельно.",
        Priority::Order => "Выбирает каналы в ручном порядке сверху вниз, затем — подписки в выбранной сортировке.",
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
        Strategy::Smart => "Smart: популярность или коэффициент",
        Strategy::MostVoted => "Самая популярная (по числу голосов)",
        Strategy::HighOdds => "Наибольший коэффициент",
        Strategy::Percentage => "Наибольшая доля баллов",
        Strategy::SmartMoney => "Больше всего баллов поставлено",
        Strategy::Number1 => "Всегда вариант №1",
        Strategy::Number2 => "Всегда вариант №2",
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
        Strategy::Smart => "Сравнивает доли голосов двух лидирующих сторон: при разнице выше порога выбирает лидера, иначе — сторону с наибольшим коэффициентом.",
        Strategy::MostVoted => "Выбирает сторону, за которую проголосовало больше людей.",
        Strategy::HighOdds => "Выбирает сторону с наибольшим потенциальным коэффициентом (обычно там меньше поставленных баллов).",
        Strategy::Percentage => "Выбирает сторону с наибольшей долей уже поставленных баллов.",
        Strategy::SmartMoney => "Выбирает сторону, на которую поставили больше всего баллов.",
        Strategy::Number1 => "Всегда ставит на первый вариант прогноза.",
        Strategy::Number2 => "Всегда ставит на второй вариант прогноза.",
        Strategy::Number3 => "Всегда ставит на третий вариант; если такого нет, ставку пропустит.",
        Strategy::Number4 => "Всегда ставит на четвёртый вариант; если такого нет, ставку пропустит.",
        Strategy::Number5 => "Всегда ставит на пятый вариант; если такого нет, ставку пропустит.",
        Strategy::Number6 => "Всегда ставит на шестой вариант; если такого нет, ставку пропустит.",
        Strategy::Number7 => "Всегда ставит на седьмой вариант; если такого нет, ставку пропустит.",
        Strategy::Number8 => "Всегда ставит на восьмой вариант; если такого нет, ставку пропустит.",
    }
}

fn metric_label(metric: FilterMetric) -> &'static str {
    match metric {
        FilterMetric::UsersPercentage => "% участников, выбравших сторону",
        FilterMetric::OddsPercentage => "Вероятность по коэффициенту, %",
        FilterMetric::Odds => "Коэффициент стороны",
        FilterMetric::DecisionUsers => "Участники выбранной стороны",
        FilterMetric::DecisionPoints => "Баллы выбранной стороны",
        FilterMetric::OutcomeUsers => "Участники стороны",
        FilterMetric::OutcomePoints => "Баллы стороны",
        FilterMetric::TopPoints => "Баллы лидирующей стороны",
        FilterMetric::TotalUsers => "Всего участников",
        FilterMetric::TotalPoints => "Всего баллов",
    }
}

fn comparison_label(comparison: Comparison) -> &'static str {
    match comparison {
        Comparison::GreaterThan => ">",
        Comparison::LessThan => "<",
        Comparison::GreaterOrEqual => "≥",
        Comparison::LessOrEqual => "≤",
    }
}

fn delay_label(mode: DelayMode) -> &'static str {
    match mode {
        DelayMode::FromStart => "После старта",
        DelayMode::FromEnd => "Перед концом",
        DelayMode::Percentage => "Доля таймера",
    }
}

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([620.0, 690.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Twitch Points Miner",
        options,
        Box::new(|_cc| Ok(Box::new(MinerApp::new()))),
    )
}
