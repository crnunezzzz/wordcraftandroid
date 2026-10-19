#![forbid(unsafe_code)]

use crate::{
    adapter::Editor,
    assistant::{Action, Assistant, Busy, COMPOSER_ID, Entry, Settings, Step, View},
    platform::Platform,
    protocol::{self, Degrade, Thread},
};
use egui::{Color32, Context, Key, Ui};
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};
use zeroize::Zeroize;

// This module is compiled only for Android. GridCraft's global clock lint guards WASM.
#[allow(clippy::disallowed_methods)]
fn now() -> Instant {
    Instant::now()
}

/// One user message being worked on: model rounds and tool calls until the model answers.
struct Run {
    ticket: u64,
    rounds: usize,
    calls: usize,
    started: Instant,
    degrade: Degrade,
}

pub struct Mobile {
    input: crate::input::AndroidInput,
    input_focus: Option<(Option<egui::Id>, bool)>,
    editor: Editor,
    platform: Platform,
    dir: PathBuf,
    assistant: Assistant,
    thread: Option<Thread>,
    run: Option<Run>,
    next_ticket: u64,
    status: String,
    clock: Instant,
    last_poll: Instant,
    last_recovery: Instant,
    last_back: Option<Instant>,
    chrome: Option<(Color32, Color32, bool)>,
    zoom: f32,
}

pub fn start(app: android_activity::AndroidApp) {
    let Some(dir) = app.internal_data_path() else { return };
    let workspace = dir.join("documents");
    if std::fs::create_dir_all(&workspace).is_err() || std::env::set_current_dir(&workspace).is_err() {
        return;
    }
    let platform = match Platform::new() {
        Ok(p) => p,
        Err(e) => {
            let _ = std::io::Write::write_fmt(&mut std::io::stderr(), format_args!("{e}\n"));
            return;
        }
    };
    let options = eframe::NativeOptions { android_app: Some(app), ..Default::default() };
    let result = eframe::run_native(
        Editor::NAME,
        options,
        Box::new(move |cc| {
            let mut assistant = Assistant::new(
                std::fs::read(dir.join("assistant.json")).ok().and_then(|b| serde_json::from_slice::<Settings>(&b).ok()).unwrap_or_default(),
            );
            assistant.set_key_flags(&platform.exchange(json!({"op":"keys"})).unwrap_or(json!({})));
            let editor = Editor::new(&dir, platform.clone(), &cc.egui_ctx);
            // Without the waker the editor still repaints on its timer; input just lags.
            let _ = platform.spawn_waker(cc.egui_ctx.clone());
            let mut input = crate::input::AndroidInput::default();
            input.synth_fling = !Editor::OWN_MOMENTUM;
            Ok(Box::new(Mobile {
                input,
                input_focus: None,
                editor,
                platform,
                dir,
                assistant,
                thread: None,
                run: None,
                next_ticket: 10_000,
                status: String::new(),
                clock: now(),
                last_poll: now(),
                last_recovery: now(),
                last_back: None,
                chrome: None,
                zoom: 0.0,
            }))
        }),
    );
    if let Err(e) = result {
        let _ = std::io::Write::write_fmt(&mut std::io::stderr(), format_args!("Android: {e}\n"));
    }
}

impl Mobile {
    fn save_settings(&mut self) {
        let result = serde_json::to_vec(&self.assistant.settings).map_err(|e| e.to_string()).and_then(|bytes| {
            let tmp = self.dir.join("assistant.json.tmp");
            std::fs::write(&tmp, bytes).and_then(|()| std::fs::rename(tmp, self.dir.join("assistant.json"))).map_err(|e| e.to_string())
        });
        if let Err(e) = result {
            self.status = e;
        }
        self.editor.persist(&self.dir);
    }

    fn stop(&mut self) {
        if let Some(run) = self.run.take() {
            let _ = self.platform.exchange(json!({"op":"cancel_http","ticket":run.ticket}));
            if let Some(thread) = &mut self.thread {
                thread.close_pending("Cancelled by the user.");
            }
        }
    }

    fn poll(&mut self, ctx: &Context) {
        if self.last_poll.elapsed() < Duration::from_millis(50) {
            return;
        }
        self.last_poll = now();
        match self.platform.exchange(json!({"op":"poll"})) {
            Ok(events) => {
                for event in events.as_array().into_iter().flatten() {
                    if event["kind"] == "http" {
                        if self.run.as_ref().is_some_and(|r| Some(r.ticket) == event["ticket"].as_u64()) {
                            self.reply(event);
                            ctx.request_repaint();
                        }
                    } else if event["exported"] == true {
                        self.status = "Archivo exportado al destino Android".into();
                    } else if let Some(error) = event["error"].as_str() {
                        self.status = error.into();
                        self.editor.file_event(event);
                    } else {
                        self.editor.file_event(event);
                    }
                }
            }
            Err(e) => self.status = e,
        }
    }

    /// Send the conversation as it is now.
    fn request(&mut self) {
        let Some(run) = &mut self.run else { return };
        let Some(thread) = &self.thread else { return };
        self.next_ticket = self.next_ticket.saturating_add(1);
        run.ticket = self.next_ticket;
        run.rounds += 1;
        let mut request = thread.request(&protocol::system_prompt(Editor::NAME), &self.assistant.settings.effort, run.degrade);
        request["ticket"] = json!(run.ticket);
        if let Err(e) = self.platform.exchange(request) {
            self.run = None;
            self.assistant.entries.push(Entry::Error { text: e, retry: true, key: false });
        }
    }

    fn send(&mut self, text: String) {
        let text = text.trim().to_owned();
        if text.is_empty() || self.run.is_some() {
            return;
        }
        if !self.assistant.settings.ai {
            self.assistant.entries.push(Entry::Notice("El acceso a la IA está desactivado en Ajustes.".into()));
            self.assistant.prompt = text;
            return;
        }
        let model = self.assistant.model();
        let provider = protocol::provider_of(&model);
        if !self.assistant.has_key(provider) {
            self.assistant.entries.push(Entry::User(text.clone()));
            self.assistant.entries.push(Entry::Error { text: format!("Falta la clave de API de {}.", provider.name()), retry: false, key: true });
            self.assistant.prompt = text;
            return;
        }
        if self.thread.as_ref().is_some_and(|t| t.model != model) {
            if self.thread.as_ref().is_some_and(|t| !t.is_empty()) {
                self.assistant
                    .entries
                    .push(Entry::Notice(format!("Ahora con {}: la conversación anterior no se le envía.", protocol::label_of(&model))));
            }
            self.thread = None;
        }
        let context = self.editor.context();
        self.thread.get_or_insert_with(|| Thread::new(&model)).push_user(protocol::user_message(&text, &context));
        self.assistant.entries.push(Entry::User(text));
        self.run = Some(Run { ticket: 0, rounds: 0, calls: 0, started: now(), degrade: Degrade::default() });
        self.request();
    }

    fn retry(&mut self) {
        if self.run.is_some() || self.thread.is_none() {
            return;
        }
        if matches!(self.assistant.entries.last(), Some(Entry::Error { .. })) {
            self.assistant.entries.pop();
        }
        self.run = Some(Run { ticket: 0, rounds: 0, calls: 0, started: now(), degrade: Degrade::default() });
        self.request();
    }

    fn reply(&mut self, event: &Value) {
        let Some(mut run) = self.run.take() else { return };
        let Some(thread) = &mut self.thread else { return };
        let ok = event["status"].as_u64().is_some_and(|s| (200..300).contains(&s)) && event.get("body").is_some();
        if !ok {
            if event["status"] == 400
                && let Some(next) = run.degrade.after(thread.provider, &protocol::provider_detail(event))
            {
                // The model refused an optional parameter: ask again without it.
                run.degrade = next;
                run.rounds = run.rounds.saturating_sub(1);
                self.run = Some(run);
                self.request();
                return;
            }
            if event["error"] == "cancelled" {
                return;
            }
            let text = protocol::describe_failure(thread.provider, &thread.model, event);
            let key = event["error"] == "missing_key" || event["status"] == 401;
            self.assistant.entries.push(Entry::Error { text, retry: !key, key });
            return;
        }
        let turn = match thread.absorb(&event["body"]) {
            Ok(turn) => turn,
            Err(text) => {
                self.assistant.entries.push(Entry::Error { text, retry: true, key: false });
                return;
            }
        };
        if !turn.text.trim().is_empty() {
            self.assistant.entries.push(Entry::Assistant(turn.text.trim().to_owned()));
        }
        if turn.calls.is_empty() {
            if turn.resume && run.rounds < protocol::MAX_ROUNDS {
                self.run = Some(run);
                self.request();
            }
            return;
        }
        let mut steps = Vec::new();
        let mut results = Vec::new();
        for call in turn.calls {
            run.calls += 1;
            let (label, result) = self.tool(&call, run.calls > protocol::MAX_CALLS);
            let ok = result.is_ok();
            let (content, detail) = match result {
                Ok(v) if v.to_string().len() <= protocol::MAX_RESULT => (v.to_string(), String::new()),
                Ok(_) => (json!({"error":"Result too large; inspect a smaller range"}).to_string(), String::new()),
                Err(e) => (json!({"error": e}).to_string(), e),
            };
            steps.push(Step { label, ok, detail });
            results.push((call.id, content, !ok));
        }
        if let Some(Entry::Steps(previous)) = self.assistant.entries.last_mut() {
            previous.extend(steps);
        } else {
            self.assistant.entries.push(Entry::Steps(steps));
        }
        if let Some(thread) = &mut self.thread {
            thread.push_results(results);
        }
        if run.rounds >= protocol::MAX_ROUNDS || run.calls > protocol::MAX_CALLS {
            if let Some(thread) = &mut self.thread {
                thread.close_pending("Work limit reached.");
            }
            self.assistant
                .entries
                .push(Entry::Notice("Se alcanzó el límite de trabajo de esta petición. Los cambios aplicados se pueden deshacer.".into()));
            return;
        }
        self.run = Some(run);
        self.request();
    }

    /// Run one tool call; returns the step's label and its result.
    fn tool(&mut self, call: &protocol::Call, over_limit: bool) -> (String, Result<Value, String>) {
        if over_limit {
            return ("Límite de acciones".into(), Err("Tool call limit reached for this request".into()));
        }
        if call.args.get("__invalid_json").is_some() {
            return ("Instrucción ilegible".into(), Err("Arguments were not valid JSON; send a JSON object".into()));
        }
        match call.name.as_str() {
            "list_commands" => {
                let search = call.args["search"].as_str().unwrap_or("").to_lowercase();
                let offset = call.args["offset"].as_u64().unwrap_or(0).min(100_000) as usize;
                let matching: Vec<Value> = self
                    .editor
                    .catalog()
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|c| c["id"].as_str().is_some_and(protocol::command_allowed) && c.to_string().to_lowercase().contains(&search))
                    .cloned()
                    .map(|mut command| {
                        if command["id"] == "file.save" {
                            command["params"] = json!("{}: save the current local document; no path or other params allowed");
                        }
                        command
                    })
                    .collect();
                let label =
                    if search.is_empty() { "Consultó las órdenes disponibles".to_owned() } else { format!("Buscó órdenes: «{search}»") };
                (label, Ok(json!({"total":matching.len(),"offset":offset,"commands":matching.into_iter().skip(offset).take(30).collect::<Vec<_>>()})))
            }
            "execute_command" => {
                let id = call.args["command"].as_str().unwrap_or("").to_owned();
                let params = call.args.get("params").cloned().unwrap_or(json!({}));
                let catalog = self.editor.catalog();
                let entry = catalog.as_array().into_iter().flatten().find(|c| c["id"] == id.as_str());
                let label = entry.and_then(|c| c["label"].as_str()).map_or_else(|| id.clone(), |l| format!("{l} ({id})"));
                let result = if entry.is_none() || !protocol::command_allowed(&id) || !params.is_object() {
                    Err(format!("Command not available: {id}. Use list_commands to find the exact id."))
                } else if let Err(e) = protocol::validate_params(&params, 0) {
                    Err(e)
                } else if id == "file.save" {
                    if params.as_object().is_some_and(|p| p.is_empty()) { self.editor.agent_save() } else { Err("file.save only accepts {}".into()) }
                } else {
                    self.editor.run(&id, params)
                };
                (label, result)
            }
            other => (other.to_owned(), Err(format!("Unknown tool {other}"))),
        }
    }

    fn act(&mut self, actions: Vec<Action>) {
        for action in actions {
            match action {
                Action::Send(text) => self.send(text),
                Action::Stop => {
                    if self.run.is_some() {
                        self.stop();
                        self.assistant.entries.push(Entry::Notice("Detenido. Los cambios ya aplicados se pueden deshacer.".into()));
                    }
                }
                Action::Retry => self.retry(),
                Action::NewChat => {
                    self.stop();
                    self.thread = None;
                }
                Action::SaveSettings => self.save_settings(),
                Action::SetKey(provider, mut key) => {
                    match self.platform.exchange(json!({"op":"set_key","provider":provider.id(),"key":key.trim()})) {
                        Ok(flags) => {
                            self.assistant.set_key_flags(&flags);
                            let current = protocol::provider_of(&self.assistant.model());
                            if self.assistant.settings.model.is_empty() || current != provider && !self.assistant.has_key(current) {
                                self.assistant.settings.model = provider.default_model().into();
                                self.save_settings();
                            }
                        }
                        Err(e) => self.status = e,
                    }
                    key.zeroize();
                }
                Action::DeleteKey(provider) => match self.platform.exchange(json!({"op":"delete_key","provider":provider.id()})) {
                    Ok(flags) => self.assistant.set_key_flags(&flags),
                    Err(e) => self.status = e,
                },
            }
        }
    }

    /// Android Back: close what is open, one layer at a time; at the editor, a second Back
    /// within two seconds sends the app to the background (the document stays open).
    fn back(&mut self, ctx: &Context, raw: &mut egui::RawInput) {
        let escape = |raw: &mut egui::RawInput| {
            for pressed in [true, false] {
                raw.events.push(egui::Event::Key {
                    key: Key::Escape,
                    physical_key: Some(Key::Escape),
                    pressed,
                    repeat: false,
                    modifiers: egui::Modifiers::NONE,
                });
            }
        };
        if self.assistant.back() {
            let actions = self.assistant.take_actions();
            self.act(actions);
        } else if egui::Popup::is_any_open(ctx) {
            escape(raw);
        } else if self.last_back.is_some_and(|t| t.elapsed() < Duration::from_secs(2)) {
            self.last_back = None;
            let _ = self.platform.exchange(json!({"op":"minimize"}));
        } else {
            self.last_back = Some(now());
            escape(raw);
        }
    }

    /// Paint the status and navigation bar areas in the editor's own colours.
    fn chrome(&mut self, ctx: &Context) {
        let visuals = ctx.global_style().visuals.clone();
        let colors = (visuals.panel_fill, visuals.panel_fill, visuals.dark_mode);
        if self.chrome != Some(colors) {
            self.chrome = Some(colors);
            let rgb = |c: Color32| (i64::from(c.r()) << 16) | (i64::from(c.g()) << 8) | i64::from(c.b());
            let _ = self.platform.exchange(json!({"op":"chrome","top":rgb(colors.0),"bottom":rgb(colors.1),"dark":colors.2}));
        }
    }

    /// A text field of the assistant has the keyboard: keep its keys from the editor.
    fn typing_in_panel(&self, ctx: &Context) -> bool {
        self.assistant.open && ctx.memory(|m| m.focused()).is_some_and(|id| id == egui::Id::new(COMPOSER_ID) || self.assistant.view == View::Settings)
    }
}

impl eframe::App for Mobile {
    fn logic(&mut self, ctx: &Context, _frame: &mut eframe::Frame) {
        if !self.assistant.settings.ai {
            self.stop();
        }
        let scale = self.assistant.settings.ui_scale.clamp(0.8, 1.5);
        if (scale - self.zoom).abs() > 1e-3 {
            self.zoom = scale;
            ctx.set_zoom_factor(scale);
        }
        // egui's own Ctrl +/- zoom would fight the editor's document zoom shortcuts.
        ctx.options_mut(|o| o.zoom_with_keyboard = false);
        self.poll(ctx);
        self.editor.logic(ctx);
        self.chrome(ctx);
        if self.last_recovery.elapsed() >= Duration::from_secs(20) {
            self.editor.recover(&self.dir);
            self.save_settings();
            self.last_recovery = now();
        }
        self.assistant.busy = self.run.as_ref().map(|run| Busy { seconds: run.started.elapsed().as_secs(), editing: run.calls > 0 });
        if self.input.active() {
            ctx.request_repaint();
        } else {
            ctx.request_repaint_after(Duration::from_millis(if self.run.is_some() { 250 } else { 500 }));
        }
    }

    fn raw_input_hook(&mut self, ctx: &Context, raw: &mut egui::RawInput) {
        let events = self.platform.exchange(json!({"op":"input"})).unwrap_or(json!([]));
        let elapsed = self.clock.elapsed().as_secs_f64();
        self.input.append(&events, raw, ctx.pixels_per_point(), elapsed);
        if std::mem::take(&mut self.input.back) {
            self.back(ctx, raw);
        }
        let paste = raw
            .events
            .iter()
            .any(|e| matches!(e, egui::Event::Key { key: Key::V, pressed: true, modifiers, .. } if modifiers.ctrl || modifiers.mac_cmd));
        if paste
            && !raw.events.iter().any(|e| matches!(e, egui::Event::Paste(_)))
            && let Ok(clipboard) = self.platform.exchange(json!({"op":"clipboard_get"}))
            && let Some(text) = clipboard["text"].as_str()
        {
            raw.events.push(egui::Event::Paste(text.into()));
        }
        if !self.typing_in_panel(ctx) {
            self.editor.raw_input(raw);
        }
    }

    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        self.editor.host_state(self.assistant.open, self.run.is_some(), &self.status);
        if self.assistant.open {
            self.assistant.panel(ui, &crate::BRANDING);
        }
        self.editor.ui(ui);
        self.assistant.launcher(ui.ctx());
        let actions = self.assistant.take_actions();
        self.act(actions);
        match self.editor.host_action() {
            Some("assistant") => {
                self.assistant.open = !self.assistant.open;
                self.assistant.view = View::Chat;
            }
            Some("settings") => {
                self.assistant.open = true;
                self.assistant.view = View::Settings;
            }
            Some("export") => self.status = self.editor.export(&self.platform).err().unwrap_or_else(|| "Elige dónde guardar la copia".into()),
            _ => {}
        }
        let (copies, links) = ui.ctx().output(|out| {
            let mut copies = Vec::new();
            let mut links = Vec::new();
            for command in &out.commands {
                match command {
                    egui::OutputCommand::CopyText(text) => copies.push(text.clone()),
                    egui::OutputCommand::OpenUrl(link) => links.push(link.url.clone()),
                    _ => {}
                }
            }
            (copies, links)
        });
        for text in copies {
            let _ = self.platform.exchange(json!({"op":"clipboard_set","text":text}));
        }
        for url in links {
            let _ = self.platform.exchange(json!({"op":"open_url","url":url}));
        }
        let ime = ui.ctx().output(|out| out.ime);
        let focus = ime.map(|ime| (ui.ctx().memory(|m| m.focused()), ime.purpose == egui::IMEPurpose::Password));
        if focus != self.input_focus || ime.is_some_and(|ime| ime.should_interrupt_composition) {
            self.input.reset_composition();
            let _ = self.platform.exchange(json!({"op":"input_context", "password":focus.is_some_and(|(_, password)| password)}));
            self.input_focus = focus;
        }
        let _ = self.platform.exchange(json!({"op":"frame_done"}));
        if ui.ctx().output(|out| out.ime.is_some()) {
            ui.ctx().request_repaint_after(Duration::from_millis(33));
        }
        if !self.assistant.open || self.assistant.view != View::Settings {
            self.assistant.zeroize_keys();
        }
    }

    fn on_exit(&mut self) {
        self.stop();
        self.editor.recover(&self.dir);
        self.save_settings();
        self.assistant.zeroize_keys();
    }
}
