//! The desktop host of the assistant panel (Windows, macOS, Linux).
//!
//! Each message goes one of two ways, per provider, as the user chose in the settings:
//! - **API key**: this module's own agent loop, the same as Android's, over HTTPS ([`http`]).
//! - **Plan**: the user's own Codex or Claude Code ([`plan`]), which calls the document tools
//!   back through [`bridge`].
//!
//! The app's `main` calls [`maybe_serve_mcp`] first, then per frame [`Desktop::frame`],
//! [`Desktop::panel`] (before the editor's own UI) and [`Desktop::finish`] (after it).
#![forbid(unsafe_code)]

pub mod bridge;
pub mod http;
pub mod plan;
pub mod secrets;

use crate::{
    agent::{self, Editor},
    assistant::{Action, Assistant, Branding, Busy, COMPOSER_ID, Chip, Entry, Host, PlanInfo, Settings, Step, View, plan_index},
    attach::{self, Attachment},
    protocol::{self, Degrade, Provider, Thread},
    update::{self, Release},
};
use egui::Context;
use serde_json::Value;
use std::{
    path::PathBuf,
    sync::mpsc::{self, Receiver, Sender},
    time::{Duration, Instant},
};
use zeroize::Zeroize;

/// If this process was started as the assistant's MCP server (by Codex or Claude Code), serve
/// it and return true: the caller then exits without opening a window.
pub fn maybe_serve_mcp() -> bool {
    if std::env::args_os().nth(1).is_some_and(|a| a == bridge::MCP_FLAG) {
        bridge::serve_stdio();
        return true;
    }
    false
}

// The assistant's clocks only time a run and its checks.
#[allow(clippy::disallowed_methods)]
fn now() -> Instant {
    Instant::now()
}

type Prepared = Result<Attachment, String>;

/// One user message on the API path: model rounds and tool calls until the model answers.
struct ApiRun {
    ticket: u64,
    rounds: usize,
    calls: usize,
    started: Instant,
    degrade: Degrade,
}

/// One user message on the plan path.
struct PlanRun {
    run: plan::Run,
    provider: Provider,
    model: String,
    calls: usize,
    started: Instant,
}

/// The last message sent through a plan, to send again after a failure.
struct PlanMessage {
    text: String,
    files: Vec<Attachment>,
}

pub struct Desktop {
    pub assistant: Assistant,
    brand: &'static Branding,
    app: &'static str,
    dir: PathBuf,
    secrets: secrets::Secrets,
    thread: Option<Thread>,
    run: Option<ApiRun>,
    plan: Option<PlanRun>,
    /// The plan tools' own conversations: (model, session id) per provider.
    sessions: [Option<(String, String)>; 2],
    last_plan: Option<PlanMessage>,
    bridge: Option<bridge::Bridge>,
    next_ticket: u64,
    replies: (Sender<(u64, Value)>, Receiver<(u64, Value)>),
    prepared: (Sender<Prepared>, Receiver<Prepared>),
    checks: (Sender<(Provider, PlanInfo)>, Receiver<(Provider, PlanInfo)>),
    updates: (Sender<Result<Option<Release>, String>>, Receiver<Result<Option<Release>, String>>),
    /// The system's display scale (native pixels per point).
    native: f32,
    was_settings: bool,
    /// The plan tools' programs, when not found where they install ([Codex, Claude Code]).
    pub plan_programs: [Option<PathBuf>; 2],
}

impl Desktop {
    /// `dir` is the app's settings folder (assistant.json, the bridge file).
    pub fn new(app: &'static str, brand: &'static Branding, dir: PathBuf) -> Self {
        let _ = std::fs::create_dir_all(&dir);
        let settings = std::fs::read(dir.join("assistant.json")).ok().and_then(|b| serde_json::from_slice::<Settings>(&b).ok()).unwrap_or_default();
        let mut assistant = Assistant::new(settings);
        assistant.host = Host::Desktop;
        let secrets = secrets::Secrets::new(app, dir.clone());
        assistant.set_key_flags(&secrets.flags());
        let mut desktop = Self {
            assistant,
            brand,
            app,
            dir,
            secrets,
            thread: None,
            run: None,
            plan: None,
            sessions: [None, None],
            last_plan: None,
            bridge: None,
            next_ticket: 1,
            replies: mpsc::channel(),
            prepared: mpsc::channel(),
            checks: mpsc::channel(),
            updates: mpsc::channel(),
            native: 1.0,
            was_settings: false,
            plan_programs: [None, None],
        };
        for provider in Provider::ALL {
            if desktop.assistant.settings.plan(provider) {
                desktop.check(provider, Duration::ZERO);
            }
        }
        desktop
    }

    pub fn toggle(&mut self) {
        self.assistant.open = !self.assistant.open;
        self.assistant.view = View::Chat;
    }

    pub fn open_settings(&mut self) {
        self.assistant.open = true;
        self.assistant.view = View::Settings;
    }

    pub fn busy(&self) -> bool {
        self.run.is_some() || self.plan.is_some()
    }

    /// A text field of the panel has the keyboard: the editor must not take its keys.
    pub fn typing(&self, ctx: &Context) -> bool {
        self.assistant.open && ctx.memory(|m| m.focused()).is_some_and(|id| id == egui::Id::new(COMPOSER_ID) || self.assistant.view == View::Settings)
    }

    /// Check a plan tool off the UI thread; with `watch`, again every few seconds until it is
    /// installed and signed in or `watch` has passed (after the user opened sign-in or install).
    fn check(&mut self, provider: Provider, watch: Duration) {
        let tx = self.checks.0.clone();
        let _ = std::thread::Builder::new().name("craft-plan-check".into()).spawn(move || {
            let started = now();
            loop {
                let info = plan::check(provider);
                let ready = info.installed && info.logged_in;
                if tx.send((provider, info)).is_err() || ready || started.elapsed() >= watch {
                    return;
                }
                std::thread::sleep(Duration::from_secs(4));
            }
        });
    }

    /// Look for a newer published build off the UI thread.
    fn check_update(&mut self) {
        let Some(url) = update::api_url(update::repo(crate::RELEASES)) else { return };
        self.assistant.update_checking = true;
        self.assistant.settings.update_checked = unix_now();
        self.save_settings();
        let tx = self.updates.0.clone();
        let spawned = std::thread::Builder::new().name("craft-update-check".into()).spawn(move || {
            let result =
                http::releases(&url).map(|reply| update::latest(&reply, update::repo(crate::RELEASES), update::Target::Windows, update::build()));
            let _ = tx.send(result);
        });
        if spawned.is_err() {
            self.assistant.update_result(Err(update::failure(None)));
        }
    }

    fn save_settings(&mut self) {
        let result = serde_json::to_vec_pretty(&self.assistant.settings).map_err(|e| e.to_string()).and_then(|bytes| {
            let tmp = self.dir.join("assistant.json.tmp");
            std::fs::write(&tmp, bytes).and_then(|()| std::fs::rename(tmp, self.dir.join("assistant.json"))).map_err(|e| e.to_string())
        });
        if let Err(e) = result {
            self.assistant.settings_error = Some(format!("No se pudieron guardar los ajustes: {e}"));
        }
    }

    /// Per frame, before the UI: replies, plan events, tool calls from the bridge, prepared
    /// files and plan checks.
    pub fn frame(&mut self, ctx: &Context, editor: &mut dyn Editor) {
        if !self.assistant.settings.ai {
            self.stop();
        }
        self.native = ctx.native_pixels_per_point().filter(|n| n.is_finite() && *n > 0.0).unwrap_or(1.0);
        self.assistant.ui_scale = editor.interface_scale(self.native);
        while let Ok((ticket, event)) = self.replies.1.try_recv() {
            if self.run.as_ref().is_some_and(|r| r.ticket == ticket) {
                self.reply(&event, editor);
            }
        }
        self.plan_events(editor);
        if let Some(bridge) = &self.bridge {
            for call in bridge.calls() {
                match &mut self.plan {
                    Some(plan) => {
                        plan.calls += 1;
                        let (label, result) = agent::run_tool(editor, &call.name, &call.args, plan.calls > protocol::MAX_CALLS);
                        let ok = result.is_ok();
                        let (content, detail) = agent::result_content(&result);
                        push_steps(&mut self.assistant.entries, vec![Step { label, ok, detail }]);
                        call.answer(content, !ok);
                    }
                    None => call.answer(serde_json::json!({"error":"No request is in progress in the editor"}).to_string(), true),
                }
            }
        }
        while let Ok(result) = self.prepared.1.try_recv() {
            self.assistant.attaching = false;
            match result {
                Ok(mut file) if self.assistant.attachments.len() < attach::MAX_FILES => {
                    if let Some(thumb) = file.thumb.take() {
                        file.texture = Some(ctx.load_texture(format!("attachment-{}", self.next_ticket), thumb, egui::TextureOptions::LINEAR));
                        self.next_ticket = self.next_ticket.saturating_add(1);
                    }
                    self.assistant.attachments.push(file);
                }
                Ok(_) => self.assistant.attach_error = Some(format!("Como máximo {} archivos por mensaje.", attach::MAX_FILES)),
                // The dialog was closed without a file.
                Err(e) if e.is_empty() => {}
                Err(e) => self.assistant.attach_error = Some(e),
            }
        }
        while let Ok((provider, info)) = self.checks.1.try_recv() {
            self.assistant.plans[plan_index(provider)] = info;
        }
        while let Ok(result) = self.updates.1.try_recv() {
            self.assistant.update_result(result);
        }
        if self.assistant.update_due(unix_now()) {
            self.check_update();
        }
        // Opening the settings checks the chosen plans again (the user may have signed in).
        let settings = self.assistant.open && self.assistant.view == View::Settings;
        if settings && !self.was_settings {
            for provider in Provider::ALL {
                if self.assistant.settings.plan(provider) {
                    self.check(provider, Duration::ZERO);
                }
            }
        }
        self.was_settings = settings;
        let started = self.run.as_ref().map(|r| (r.started, r.calls)).or_else(|| self.plan.as_ref().map(|p| (p.started, p.calls)));
        self.assistant.busy = started.map(|(started, calls)| Busy { seconds: started.elapsed().as_secs(), editing: calls > 0 });
        if self.busy() || self.assistant.attaching {
            // Tool calls from the bridge wait for this thread: keep it turning.
            ctx.request_repaint_after(Duration::from_millis(80));
        }
    }

    /// The panel, before the editor's own UI (it takes the right side of the window).
    pub fn panel(&mut self, ui: &mut egui::Ui) {
        if self.assistant.open {
            self.assistant.panel(ui, self.brand);
        }
    }

    /// After the editor's UI: what the panel asked for. (The ribbon's Asistente tab opens it; the
    /// floating button is for touch screens.)
    pub fn finish(&mut self, ctx: &Context, editor: &mut dyn Editor) {
        self.assistant.update_notice(ctx, self.brand);
        let actions = self.assistant.take_actions();
        self.act(actions, ctx, editor);
        if !self.assistant.open || self.assistant.view != View::Settings {
            self.assistant.zeroize_keys();
        }
    }

    pub fn on_exit(&mut self) {
        self.stop();
        self.save_settings();
        self.assistant.zeroize_keys();
    }

    fn stop(&mut self) {
        self.run = None;
        if let Some(thread) = &mut self.thread {
            thread.close_pending("Cancelled by the user.");
        }
        if let Some(mut plan) = self.plan.take() {
            plan.run.cancel();
        }
    }

    fn act(&mut self, actions: Vec<Action>, ctx: &Context, editor: &mut dyn Editor) {
        for action in actions {
            match action {
                Action::Send(text) => self.send(text, editor),
                Action::Stop => {
                    if self.busy() {
                        self.stop();
                        self.assistant.entries.push(Entry::Notice("Detenido. Los cambios ya aplicados se pueden deshacer.".into()));
                    }
                }
                Action::Retry => self.retry(editor),
                Action::NewChat => {
                    self.stop();
                    self.thread = None;
                    self.sessions = [None, None];
                    self.last_plan = None;
                }
                Action::SaveSettings => self.save_settings(),
                Action::SetKey(provider, mut key) => {
                    match self.secrets.set(provider, key.trim()) {
                        Ok(()) => {
                            self.assistant.settings_error = None;
                            self.assistant.set_key_flags(&self.secrets.flags());
                            let current = protocol::provider_of(&self.assistant.model());
                            if self.assistant.settings.model.is_empty() || current != provider && !self.assistant.ready(current) {
                                self.assistant.settings.model = provider.default_model().into();
                                self.save_settings();
                            }
                        }
                        Err(e) => self.assistant.settings_error = Some(e),
                    }
                    key.zeroize();
                }
                Action::DeleteKey(provider) => match self.secrets.delete(provider) {
                    Ok(()) => self.assistant.set_key_flags(&self.secrets.flags()),
                    Err(e) => self.assistant.settings_error = Some(e),
                },
                Action::SetPlan(provider, on) => {
                    self.assistant.settings.set_plan(provider, on);
                    // The other route keeps nothing of this provider's conversation.
                    self.sessions[plan_index(provider)] = None;
                    if self.thread.as_ref().is_some_and(|t| t.provider == provider) {
                        self.thread = None;
                    }
                    let model = self.assistant.settings.model.clone();
                    if on && (model.is_empty() || protocol::provider_of(&model) != provider && !self.assistant.ready(protocol::provider_of(&model))) {
                        self.assistant.settings.model =
                            if provider == Provider::OpenAi { protocol::CODEX_DEFAULT.into() } else { provider.default_model().into() };
                    }
                    if !on && model == protocol::CODEX_DEFAULT {
                        self.assistant.settings.model = Provider::OpenAi.default_model().into();
                    }
                    if on {
                        self.check(provider, Duration::ZERO);
                    }
                    self.save_settings();
                }
                Action::PlanLogin(provider) => match plan::open_login(provider) {
                    Ok(()) => {
                        self.assistant.settings_error = None;
                        self.check(provider, Duration::from_secs(300));
                    }
                    Err(e) => self.assistant.settings_error = Some(e),
                },
                Action::PlanInstall(provider) => match plan::open_install(provider) {
                    Ok(()) => {
                        self.assistant.settings_error = None;
                        self.check(provider, Duration::from_secs(900));
                    }
                    Err(e) => self.assistant.settings_error = Some(e),
                },
                Action::PlanRefresh => {
                    for provider in Provider::ALL {
                        if self.assistant.settings.plan(provider) {
                            self.check(provider, Duration::ZERO);
                        }
                    }
                }
                Action::CheckUpdate => {
                    self.assistant.settings.update_skipped = 0;
                    if !self.assistant.update_checking {
                        self.check_update();
                    }
                }
                Action::OpenUpdate => {
                    if let Some(release) = &self.assistant.update {
                        let build = release.build;
                        if let Err(e) = open::that_detached(&release.download) {
                            self.assistant.update_status = Some(format!("No se pudo abrir el navegador ({e}). Descárgala desde {}", release.page));
                        }
                        self.assistant.settings.update_skipped = build;
                        self.save_settings();
                    }
                }
                Action::SkipUpdate => {
                    if let Some(release) = &self.assistant.update {
                        self.assistant.settings.update_skipped = release.build;
                        self.save_settings();
                    }
                }
                Action::Attach => self.attach(),
                Action::PasteImage => self.paste_image(),
                Action::RemoveAttachment(index) => {
                    if index < self.assistant.attachments.len() {
                        self.assistant.attachments.remove(index);
                    }
                    self.assistant.attach_error = None;
                }
                Action::SetScale(scale) => {
                    editor.set_interface_scale(self.native, scale);
                    self.assistant.ui_scale = editor.interface_scale(self.native);
                    ctx.request_repaint();
                }
            }
        }
    }

    fn room_for_file(&mut self) -> bool {
        self.assistant.attach_error = None;
        if self.assistant.attachments.len() >= attach::MAX_FILES {
            self.assistant.attach_error = Some(format!("Como máximo {} archivos por mensaje.", attach::MAX_FILES));
            return false;
        }
        true
    }

    /// Pick a file with the system dialog and prepare it, both off the UI thread.
    fn attach(&mut self) {
        if self.assistant.attaching || !self.room_for_file() {
            return;
        }
        let tx = self.prepared.0.clone();
        self.assistant.attaching = true;
        let spawned = std::thread::Builder::new().name("craft-attach".into()).spawn(move || {
            let picked = pollster::block_on(
                rfd::AsyncFileDialog::new()
                    .set_title("Adjuntar al asistente")
                    .add_filter(
                        "Imágenes, PDF, documentos y texto",
                        &[
                            "png", "jpg", "jpeg", "gif", "webp", "bmp", "pdf", "docx", "xlsx", "pptx", "odt", "ods", "odp", "txt", "md", "csv",
                            "json", "html", "xml", "rtf",
                        ],
                    )
                    .add_filter("Todos los archivos", &["*"])
                    .pick_file(),
            );
            let Some(file) = picked else {
                let _ = tx.send(Err(String::new()));
                return;
            };
            let path = file.path().to_owned();
            let name = path.file_name().map_or_else(|| "archivo".to_owned(), |n| n.to_string_lossy().into_owned());
            let _ = tx.send(read_and_prepare(&path, &name));
        });
        if spawned.is_err() {
            self.assistant.attaching = false;
            self.assistant.attach_error = Some("No se pudo abrir el selector de archivos.".into());
        }
    }

    fn paste_image(&mut self) {
        if self.assistant.attaching || !self.room_for_file() {
            return;
        }
        let image = arboard::Clipboard::new().and_then(|mut c| c.get_image());
        let Ok(image) = image else {
            self.assistant.attach_error = Some("No hay ninguna imagen copiada en el portapapeles.".into());
            return;
        };
        let (width, height, bytes) = (image.width, image.height, image.bytes.into_owned());
        let tx = self.prepared.0.clone();
        self.assistant.attaching = true;
        let spawned = std::thread::Builder::new().name("craft-attach".into()).spawn(move || {
            let result = u32::try_from(width)
                .ok()
                .zip(u32::try_from(height).ok())
                .and_then(|(w, h)| image::RgbaImage::from_raw(w, h, bytes))
                .ok_or_else(|| "La imagen del portapapeles no se pudo leer.".to_owned())
                .and_then(|rgba| {
                    let mut png = Vec::new();
                    image::DynamicImage::ImageRgba8(rgba)
                        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
                        .map_err(|_| "La imagen del portapapeles no se pudo leer.".to_owned())?;
                    attach::prepare("Imagen pegada.png", &png)
                });
            let _ = tx.send(result);
        });
        if spawned.is_err() {
            self.assistant.attaching = false;
        }
    }

    fn send(&mut self, text: String, editor: &mut dyn Editor) {
        let text = text.trim().to_owned();
        if (text.is_empty() && self.assistant.attachments.is_empty()) || self.busy() || self.assistant.attaching {
            return;
        }
        let chips: Vec<Chip> = self.assistant.attachments.iter().map(Chip::from).collect();
        if !self.assistant.settings.ai {
            self.assistant.entries.push(Entry::Notice("El acceso a la IA está desactivado en Ajustes.".into()));
            self.assistant.prompt = text;
            return;
        }
        let model = self.assistant.model();
        let provider = protocol::provider_of(&model);
        if self.assistant.uses_plan(provider) {
            let files = std::mem::take(&mut self.assistant.attachments);
            self.assistant.attach_error = None;
            self.assistant.entries.push(Entry::User { text: text.clone(), files: chips });
            self.start_plan(PlanMessage { text, files }, editor);
            return;
        }
        if !self.assistant.has_key(provider) {
            self.assistant.entries.push(Entry::User { text: text.clone(), files: chips });
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
        // Files are checked against the request limit before they join the history for good.
        let weight: usize = self.assistant.attachments.iter().map(Attachment::weight).sum();
        if weight > 0 {
            let history = self.thread.as_ref().map_or(0, |t| t.request("", "low", Degrade::default())["body"].to_string().len());
            if history + weight + protocol::MAX_CONTEXT > protocol::MAX_REQUEST {
                self.assistant.attach_error = Some(too_large(history + weight));
                self.assistant.prompt = text;
                return;
            }
        }
        let files = std::mem::take(&mut self.assistant.attachments);
        self.assistant.attach_error = None;
        let context = editor.context();
        let ask = if text.is_empty() { "Revisa los archivos adjuntos." } else { text.as_str() };
        self.thread.get_or_insert_with(|| Thread::new(&model)).push_user(protocol::user_message(ask, &context), &files);
        self.assistant.entries.push(Entry::User { text, files: chips });
        self.run = Some(ApiRun { ticket: 0, rounds: 0, calls: 0, started: now(), degrade: Degrade::default() });
        self.request();
    }

    /// Send the conversation as it is now (API path).
    fn request(&mut self) {
        let (Some(run), Some(thread)) = (&mut self.run, &self.thread) else { return };
        self.next_ticket = self.next_ticket.saturating_add(1);
        run.ticket = self.next_ticket;
        run.rounds += 1;
        let request = thread.request(&protocol::desktop_system_prompt(self.app), &self.assistant.settings.effort, run.degrade);
        let size = request["body"].to_string().len();
        if size > protocol::MAX_REQUEST {
            self.run = None;
            if let Some(thread) = &mut self.thread {
                thread.close_pending("The request grew too large.");
            }
            self.assistant.entries.push(Entry::Error { text: too_large(size), retry: false, key: false });
            return;
        }
        let key = self.secrets.get(thread.provider).unwrap_or_default();
        let (ticket, tx) = (run.ticket, self.replies.0.clone());
        let spawned = std::thread::Builder::new().name("craft-assistant-http".into()).spawn(move || {
            let mut key = key;
            let event = http::send(&request, &key);
            key.zeroize();
            let _ = tx.send((ticket, event));
        });
        if spawned.is_err() {
            self.run = None;
            self.assistant.entries.push(Entry::Error { text: "No se pudo enviar la petición.".into(), retry: true, key: false });
        }
    }

    fn retry(&mut self, editor: &mut dyn Editor) {
        if self.busy() {
            return;
        }
        if matches!(self.assistant.entries.last(), Some(Entry::Error { .. })) {
            self.assistant.entries.pop();
        }
        let provider = protocol::provider_of(&self.assistant.model());
        if self.assistant.uses_plan(provider) {
            if let Some(message) = self.last_plan.take() {
                self.start_plan(message, editor);
            }
            return;
        }
        if self.thread.is_some() {
            self.run = Some(ApiRun { ticket: 0, rounds: 0, calls: 0, started: now(), degrade: Degrade::default() });
            self.request();
        }
    }

    fn reply(&mut self, event: &Value, editor: &mut dyn Editor) {
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
            let (label, result) = agent::run_tool(editor, &call.name, &call.args, run.calls > protocol::MAX_CALLS);
            let ok = result.is_ok();
            let (content, detail) = agent::result_content(&result);
            steps.push(Step { label, ok, detail });
            results.push((call.id, content, !ok));
        }
        push_steps(&mut self.assistant.entries, steps);
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

    /// Send one message through the provider's plan tool.
    fn start_plan(&mut self, message: PlanMessage, editor: &mut dyn Editor) {
        let model = self.assistant.model();
        let provider = protocol::provider_of(&model);
        let tool = plan::tool(provider);
        let Some(exe) = self.plan_programs[plan_index(provider)].clone().or_else(|| plan::locate(provider)) else {
            let (plan_name, _) = crate::assistant::plan_title(provider);
            self.assistant.entries.push(Entry::Error {
                text: format!(
                    "Para usar tu {} hace falta {tool} en este equipo. Instálalo desde Ajustes del asistente.",
                    crate::assistant::lower_first(plan_name)
                ),
                retry: false,
                key: true,
            });
            self.last_plan = Some(message);
            return;
        };
        if self.bridge.is_none() {
            match bridge::Bridge::start(&self.dir) {
                Ok(bridge) => self.bridge = Some(bridge),
                Err(e) => {
                    self.assistant.entries.push(Entry::Error {
                        text: format!("No se pudo preparar la conexión con {tool}: {e}"),
                        retry: true,
                        key: false,
                    });
                    self.last_plan = Some(message);
                    return;
                }
            }
        }
        let Some(bridge) = &self.bridge else { return };
        let Ok(app_exe) = std::env::current_exe() else {
            self.assistant.entries.push(Entry::Error { text: "No se encontró el programa de la app.".into(), retry: false, key: false });
            return;
        };
        let index = plan_index(provider);
        let session = match &self.sessions[index] {
            Some((m, id)) if *m == model => Some(id.clone()),
            Some(_) => {
                self.assistant
                    .entries
                    .push(Entry::Notice(format!("Ahora con {}: la conversación anterior no se le envía.", protocol::label_of(&model))));
                None
            }
            None => None,
        };
        let context = editor.context();
        let ask = if message.text.is_empty() { "Revisa los archivos adjuntos." } else { message.text.as_str() };
        let text = protocol::user_message(ask, &context);
        let system = protocol::desktop_system_prompt(self.app);
        let work = self.dir.join("assistant-cli");
        let request = plan::Request {
            provider,
            exe: &exe,
            model: (model != protocol::CODEX_DEFAULT).then_some(model.as_str()),
            effort: &self.assistant.settings.effort,
            system: &system,
            text: &text,
            files: &message.files,
            session: session.as_deref(),
            app: &app_exe,
            bridge: bridge.file(),
            work: &work,
        };
        match plan::start(&request) {
            Ok(run) => {
                self.plan = Some(PlanRun { run, provider, model, calls: 0, started: now() });
                self.last_plan = Some(message);
            }
            Err(e) => {
                let retry = !e.contains("PDF") && !e.contains("adjuntos");
                self.assistant.entries.push(Entry::Error { text: e, retry, key: false });
                self.last_plan = retry.then_some(message);
            }
        }
    }

    fn plan_events(&mut self, _editor: &mut dyn Editor) {
        let Some(plan) = &mut self.plan else { return };
        let mut ended = None;
        while let Ok(event) = plan.run.events.try_recv() {
            match event {
                plan::Event::Session(id) => self.sessions[plan_index(plan.provider)] = Some((plan.model.clone(), id)),
                plan::Event::Text(text) => self.assistant.entries.push(Entry::Assistant(text.trim().to_owned())),
                plan::Event::Done(error) => {
                    ended = Some(error);
                    break;
                }
            }
        }
        let Some(error) = ended else { return };
        self.plan = None;
        match error {
            None => self.last_plan = None,
            Some(text) => {
                let key = text.contains("sesión");
                self.assistant.entries.push(Entry::Error { text, retry: !key, key });
            }
        }
    }
}

// Only when the last update check was (desktop: never wasm).
#[allow(clippy::disallowed_methods)]
fn unix_now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn push_steps(entries: &mut Vec<Entry>, steps: Vec<Step>) {
    if let Some(Entry::Steps(previous)) = entries.last_mut() {
        previous.extend(steps);
    } else {
        entries.push(Entry::Steps(steps));
    }
}

fn read_and_prepare(path: &std::path::Path, name: &str) -> Prepared {
    match std::fs::metadata(path) {
        Ok(meta) if meta.len() > attach::MAX_FILE => {
            Err(format!("«{name}» pesa {}; el máximo es {}.", attach::size(meta.len() as usize), attach::size(attach::MAX_FILE as usize)))
        }
        Ok(_) => std::fs::read(path).map_err(|_| format!("No se pudo leer «{name}».")).and_then(|bytes| attach::prepare(name, &bytes)),
        Err(_) => Err(format!("No se pudo leer «{name}».")),
    }
}

fn too_large(size: usize) -> String {
    format!(
        "Con los adjuntos, la conversación ocuparía {} y el máximo por petición es {}. Quita algún archivo o empieza una conversación nueva (+).",
        attach::size(size),
        attach::size(protocol::MAX_REQUEST)
    )
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use serde_json::json;

    /// A stand-in for Claude Code: calls a document tool through the bridge, as the real one does
    /// through the MCP server, then reports in Claude Code's stream-json events.
    const FAKE_CLAUDE: &str = r#"#!/usr/bin/env python3
import json, socket, sys
args = sys.argv[1:]
open(sys.argv[0] + ".args", "a").write(json.dumps(args) + "\n")
config = json.load(open(args[args.index("--mcp-config") + 1]))
server = config["mcpServers"]["craft"]
bridge = json.load(open(server["env"]["CRAFT_ASSISTANT_BRIDGE"]))
message = json.loads(sys.stdin.readline())
text = message["message"]["content"][-1]["text"]
assert "<document_snapshot>" in text, text
s = socket.create_connection(("127.0.0.1", bridge["port"]))
f = s.makefile("rwb")
call = {"token": bridge["token"], "name": "execute_command", "args": {"command": "text.insert", "params": {"text": "Hola"}}}
f.write((json.dumps(call) + "\n").encode()); f.flush()
reply = json.loads(f.readline())
print(json.dumps({"type": "system", "subtype": "init", "session_id": "ses-1"}), flush=True)
print(json.dumps({"type": "assistant", "message": {"content": [{"type": "text", "text": "Listo: " + reply["content"]}]}}), flush=True)
print(json.dumps({"type": "result", "subtype": "success", "is_error": False, "result": "Listo", "session_id": "ses-1"}), flush=True)
"#;

    struct Fake {
        ran: Vec<(String, Value)>,
    }
    impl Editor for Fake {
        fn catalog(&self) -> Value {
            json!([{"id":"text.insert","label":"Insert text"}])
        }
        fn run(&mut self, id: &str, params: Value) -> Result<Value, String> {
            self.ran.push((id.into(), params));
            Ok(json!({"inserted":true}))
        }
        fn save(&mut self) -> Result<Value, String> {
            Err("no".into())
        }
        fn context(&mut self) -> Value {
            json!({"blocks":["Hola"]})
        }
    }

    fn wait(desktop: &mut Desktop, ctx: &Context, editor: &mut Fake) {
        for _ in 0..400 {
            desktop.frame(ctx, editor);
            if desktop.plan.is_none() {
                break;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        assert!(desktop.plan.is_none(), "the plan run did not end");
    }

    #[test]
    fn a_message_through_the_plan_edits_the_document_and_continues_the_session() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("craft-plan-e2e-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join("claude");
        std::fs::write(&script, FAKE_CLAUDE).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        let mut desktop = Desktop::new("WordCraft", &crate::BRANDING, dir.clone());
        desktop.assistant.settings.anthropic_plan = true;
        desktop.assistant.settings.model = "claude-sonnet-5-5".into();
        desktop.plan_programs[1] = Some(script.clone());
        let ctx = Context::default();
        let mut editor = Fake { ran: vec![] };

        desktop.send("Escribe Hola".into(), &mut editor);
        wait(&mut desktop, &ctx, &mut editor);
        assert_eq!(editor.ran, [("text.insert".to_owned(), json!({"text":"Hola"}))]);
        let entries = &desktop.assistant.entries;
        assert!(matches!(&entries[0], Entry::User { text, .. } if text == "Escribe Hola"));
        assert!(matches!(&entries[1], Entry::Steps(steps) if steps.len() == 1 && steps[0].ok), "{}", entries.len());
        assert!(matches!(&entries[2], Entry::Assistant(text) if text.contains("inserted")));
        assert_eq!(entries.len(), 3, "no error after a successful run");

        // The next message continues Claude Code's own conversation.
        desktop.send("Otra vez".into(), &mut editor);
        wait(&mut desktop, &ctx, &mut editor);
        let runs: Vec<Vec<String>> =
            std::fs::read_to_string(dir.join("claude.args")).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        assert_eq!(runs.len(), 2);
        assert!(!runs[0].contains(&"--resume".to_owned()));
        let resume = runs[1].iter().position(|a| a == "--resume").unwrap();
        assert_eq!(runs[1][resume + 1], "ses-1");
        assert!(runs[1].windows(2).any(|w| w == ["--model", "claude-sonnet-5-5"]));

        // A new conversation starts a new session.
        desktop.act(vec![Action::NewChat], &ctx, &mut editor);
        assert!(desktop.sessions.iter().all(Option::is_none));
        drop(desktop);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
