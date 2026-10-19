#![forbid(unsafe_code)]

use crate::{
    adapter::Editor,
    platform::Platform,
    protocol::{self, Profile},
};
use egui::{Context, Ui};
use serde::{Deserialize, Serialize};
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

#[derive(Serialize, Deserialize)]
#[serde(default)]
struct Settings {
    profiles: Vec<Profile>,
    active: usize,
    enabled: bool,
    touch: bool,
}
impl Default for Settings {
    fn default() -> Self {
        Self { profiles: protocol::profiles(), active: 0, enabled: false, touch: true }
    }
}

struct Agent {
    messages: Vec<Value>,
    stamp: String,
    ticket: u64,
    rounds: usize,
    calls: usize,
    api: String,
    profile: Profile,
}

pub struct Mobile {
    input: crate::input::AndroidInput,
    input_focus: Option<(Option<egui::Id>, bool)>,
    editor: Editor,
    platform: Platform,
    dir: PathBuf,
    settings: Settings,
    keys: Vec<String>,
    key_flags: Value,
    show_settings: bool,
    show_agent: bool,
    prompt: String,
    transcript: String,
    status: String,
    agent: Option<Agent>,
    next_ticket: u64,
    last_poll: Instant,
    last_recovery: Instant,
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
            let settings = std::fs::read(dir.join("mobile.json")).ok().and_then(|b| serde_json::from_slice::<Settings>(&b).ok()).unwrap_or_default();
            let editor = Editor::new(&dir, platform.clone(), &cc.egui_ctx);
            let key_flags = platform.exchange(json!({"op":"keys"})).unwrap_or(json!({}));
            Ok(Box::new(Mobile {
                input: Default::default(),
                input_focus: None,
                editor,
                platform,
                dir,
                keys: (0..settings.profiles.len()).map(|_| String::new()).collect(),
                settings,
                key_flags,
                show_settings: false,
                show_agent: false,
                prompt: String::new(),
                transcript: String::new(),
                status: String::new(),
                agent: None,
                next_ticket: 10_000,
                last_poll: now(),
                last_recovery: now(),
            }))
        }),
    );
    if let Err(e) = result {
        let _ = std::io::Write::write_fmt(&mut std::io::stderr(), format_args!("Android: {e}\n"));
    }
}

impl Mobile {
    fn stop(&mut self) {
        if let Some(agent) = self.agent.take() {
            let _ = self.platform.exchange(json!({"op":"cancel_http","ticket":agent.ticket}));
        }
    }
    fn persist(&mut self) {
        let result = serde_json::to_vec(&self.settings).map_err(|e| e.to_string()).and_then(|bytes| {
            let tmp = self.dir.join("mobile.json.tmp");
            std::fs::write(&tmp, bytes).and_then(|()| std::fs::rename(tmp, self.dir.join("mobile.json"))).map_err(|e| e.to_string())
        });
        if let Err(e) = result {
            self.status = e;
        }
        self.editor.persist(&self.dir);
    }

    fn poll(&mut self, ctx: &Context) {
        if self.last_poll.elapsed() < Duration::from_millis(150) {
            return;
        }
        self.last_poll = now();
        match self.platform.exchange(json!({"op":"poll"})) {
            Ok(events) => {
                for event in events.as_array().into_iter().flatten() {
                    if event["kind"] == "http" {
                        if self.agent.as_ref().is_some_and(|a| Some(a.ticket) == event["ticket"].as_u64()) {
                            self.reply(event);
                        }
                    } else if event["exported"] == true {
                        self.status = "Archivo exportado al destino Android".into();
                    } else if let Some(error) = event["error"].as_str() {
                        self.status = error.into();
                        self.editor.file_event(event);
                    } else {
                        self.editor.file_event(event);
                        if event["path"].is_string() {
                            self.status = "Archivo abierto como copia local. Exportar a Android guarda una copia en el destino elegido.".into();
                        }
                    }
                }
            }
            Err(e) => self.status = e,
        }
        ctx.request_repaint_after(Duration::from_millis(150));
    }

    fn send(&mut self) {
        if !self.settings.enabled {
            self.stop();
            return;
        }
        let Some(agent) = &mut self.agent else { return };
        if agent.rounds >= protocol::MAX_ROUNDS || agent.calls >= protocol::MAX_CALLS {
            self.status = "Se alcanzó el límite de trabajo. Los cambios aplicados siguen disponibles para deshacer.".into();
            self.agent = None;
            return;
        }
        agent.rounds += 1;
        self.next_ticket = self.next_ticket.saturating_add(1);
        agent.ticket = self.next_ticket;
        let result = protocol::request(&agent.profile, &agent.messages).and_then(|mut request| {
            request["ticket"] = json!(agent.ticket);
            self.platform.exchange(request)
        });
        if let Err(e) = result {
            self.status = e;
            self.agent = None;
        }
    }

    fn begin(&mut self) {
        if !self.settings.enabled || self.prompt.trim().is_empty() || self.agent.is_some() {
            return;
        }
        let Some(profile) = self.settings.profiles.get(self.settings.active).cloned() else { return };
        let context = self.editor.context();
        if context.to_string().len() > 64 * 1024 {
            self.status = "El contexto es demasiado grande; selecciona un fragmento más pequeño".into();
            return;
        }
        self.transcript = format!("Tú: {}\n\n", self.prompt);
        self.agent = Some(Agent {
            messages: protocol::initial_messages(Editor::NAME, &self.prompt, context),
            stamp: self.editor.stamp(),
            ticket: 0,
            rounds: 0,
            calls: 0,
            api: profile.api.clone(),
            profile,
        });
        self.send();
    }

    fn reply(&mut self, event: &Value) {
        let Some(mut agent) = self.agent.take() else { return };
        if agent.stamp != self.editor.stamp() {
            self.status = "El documento o la selección cambió durante la petición. Vuelve a solicitar los cambios.".into();
            return;
        }
        if let Some(error) = event["error"].as_str() {
            self.status = error.into();
            return;
        }
        let reply = match protocol::parse_reply(&agent.api, &event["body"]) {
            Ok(r) => r,
            Err(e) => {
                self.status = e;
                return;
            }
        };
        if !reply.text.is_empty() {
            self.transcript.push_str(&reply.text);
            self.transcript.push_str("\n\n");
        }
        agent.messages.push(reply.message);
        if reply.calls.is_empty() {
            self.status = "Solicitud terminada. Puedes guardar o deshacer los cambios.".into();
            return;
        }
        for call in reply.calls {
            if agent.calls >= protocol::MAX_CALLS {
                self.status = "Límite de instrucciones alcanzado".into();
                return;
            }
            agent.calls += 1;
            let result = if call.name == "list_commands" {
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
                Ok(json!({"total":matching.len(),"offset":offset,"commands":matching.into_iter().skip(offset).take(30).collect::<Vec<_>>()}))
            } else {
                let id = call.args["command"].as_str().unwrap_or("");
                let params = &call.args["params"];
                let known = self.editor.catalog().as_array().into_iter().flatten().any(|c| c["id"] == id);
                if !known || !protocol::command_allowed(id) || !params.is_object() {
                    Err("Comando no permitido".into())
                } else if let Err(e) = protocol::validate_params(params, 0) {
                    Err(e)
                } else {
                    let result = if id == "file.save" {
                        if params.as_object().is_some_and(|p| p.is_empty()) {
                            self.editor.agent_save()
                        } else {
                            Err("file.save solo acepta {} y guarda la copia local actual".into())
                        }
                    } else {
                        self.editor.run(id, params.clone())
                    };
                    self.transcript.push_str(&format!("{} {id}\n", if result.is_ok() { "✓" } else { "×" }));
                    result
                }
            };
            let result = match result {
                Ok(v) if v.to_string().len() <= 48 * 1024 => v,
                Ok(_) => json!({"error":"Resultado demasiado grande; consulta un rango más pequeño"}),
                Err(e) => json!({"error":e}),
            };
            agent.messages.push(json!({"role":"tool","tool_call_id":call.id,"name":call.name,"content":result.to_string()}));
        }
        agent.stamp = self.editor.stamp();
        self.agent = Some(agent);
        self.send();
    }

    fn settings_ui(&mut self, ui: &mut Ui) {
        ui.checkbox(&mut self.settings.enabled, "Activar asistente por API");
        ui.label("Cada solicitud envía tu instrucción, el contexto del documento y los resultados de las herramientas al proveedor elegido.");
        ui.checkbox(&mut self.settings.touch, "Controles amplios para uso táctil");
        ui.separator();
        for (i, profile) in self.settings.profiles.iter_mut().enumerate() {
            ui.strong(&profile.name);
            let has_key = self.key_flags[&profile.id].as_bool().unwrap_or(false);
            ui.label(if has_key { "Clave guardada en Android Keystore" } else { "Sin clave" });
            if let Some(key) = self.keys.get_mut(i) {
                ui.add(egui::TextEdit::singleline(key).password(true).hint_text("Nueva API key (vacío conserva la actual)"));
                ui.horizontal(|ui| {
                    if ui.button("Guardar clave").clicked() && !key.trim().is_empty() {
                        match self.platform.exchange(json!({"op":"set_key","provider":profile.id,"key":key.trim()})) {
                            Ok(flags) => {
                                self.key_flags = flags;
                                self.status = "Clave guardada".into();
                            }
                            Err(e) => self.status = e,
                        }
                        key.zeroize();
                    }
                    if ui.add_enabled(has_key, egui::Button::new("Eliminar clave")).clicked() {
                        match self.platform.exchange(json!({"op":"delete_key","provider":profile.id})) {
                            Ok(flags) => self.key_flags = flags,
                            Err(e) => self.status = e,
                        }
                        key.zeroize();
                    }
                });
            }
            ui.label(if profile.api == "azure" {
                "Endpoint Azure del recurso (sin /openai)"
            } else {
                "Endpoint HTTPS (incluye /v1 cuando corresponda)"
            });
            ui.text_edit_singleline(&mut profile.endpoint);
            ui.label(if profile.api == "azure" { "Nombre del deployment" } else { "Modelo" });
            ui.text_edit_singleline(&mut profile.model);
            ui.separator();
        }
        if ui.button("Guardar configuración").clicked() {
            self.persist();
            self.show_settings = false;
        }
    }

    fn agent_ui(&mut self, ui: &mut Ui) {
        if !self.settings.enabled {
            ui.label("Activa el asistente y configura una clave en Configuración de IA.");
        }
        ui.add(egui::TextEdit::multiline(&mut self.prompt).hint_text("¿Qué quieres cambiar en este documento?").desired_rows(3));
        ui.horizontal(|ui| {
            if ui.add_enabled(self.agent.is_none() && self.settings.enabled, egui::Button::new("Ejecutar")).clicked() {
                self.begin();
            }
            if self.agent.is_some() {
                ui.spinner();
                if ui.button("Detener").clicked() {
                    self.stop();
                    self.status = "Detenido. Los cambios ya aplicados se pueden deshacer.".into();
                }
            }
        });
        ui.separator();
        egui::ScrollArea::vertical().id_salt("conversation").show(ui, |ui| {
            ui.label(&self.transcript);
        });
    }
}

impl eframe::App for Mobile {
    fn logic(&mut self, ctx: &Context, _frame: &mut eframe::Frame) {
        if !self.settings.enabled {
            self.stop();
        }
        self.poll(ctx);
        self.editor.logic(ctx);
        if self.last_recovery.elapsed() >= Duration::from_secs(20) {
            self.editor.recover(&self.dir);
            self.persist();
            self.last_recovery = now();
        }
        ctx.request_repaint_after(Duration::from_millis(150));
    }
    fn raw_input_hook(&mut self, _ctx: &Context, raw: &mut egui::RawInput) {
        if let Ok(events) = self.platform.exchange(json!({"op":"input"})) {
            self.input.append(&events, raw);
        }
        let paste = raw
            .events
            .iter()
            .any(|e| matches!(e, egui::Event::Key { key: egui::Key::V, pressed: true, modifiers, .. } if modifiers.ctrl || modifiers.mac_cmd));
        if paste
            && !raw.events.iter().any(|e| matches!(e, egui::Event::Paste(_)))
            && let Ok(clipboard) = self.platform.exchange(json!({"op":"clipboard_get"}))
            && let Some(text) = clipboard["text"].as_str()
        {
            raw.events.push(egui::Event::Paste(text.into()));
        }
        if !self.show_settings && !self.show_agent {
            self.editor.raw_input(raw);
        }
    }
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        self.editor.host_state(self.show_agent, self.agent.is_some(), &self.status);
        if self.show_agent {
            egui::Panel::right("android_assistant").default_size(340.0).min_size(260.0).max_size(520.0).resizable(true).show(ui, |ui| {
                if self.settings.touch {
                    ui.spacing_mut().interact_size.y = 32.0;
                }
                ui.horizontal(|ui| {
                    let selected = self
                        .settings
                        .profiles
                        .get(self.settings.active)
                        .map(|p| if p.model.is_empty() { p.name.clone() } else { p.model.chars().take(28).collect() })
                        .unwrap_or_else(|| "Modelo".into());
                    ui.menu_button(selected, |ui| {
                        ui.set_max_width(280.0);
                        ui.add_enabled_ui(self.agent.is_none(), |ui| {
                            for (i, profile) in self.settings.profiles.iter().enumerate() {
                                if ui.selectable_value(&mut self.settings.active, i, format!("{} · {}", profile.name, profile.model)).clicked() {
                                    ui.close();
                                }
                            }
                        });
                        ui.separator();
                        if ui.button("API keys y proveedores…").clicked() {
                            self.show_settings = true;
                            ui.close();
                        }
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("×").on_hover_text("Cerrar asistente").clicked() {
                            self.show_agent = false;
                        }
                    });
                });
                ui.separator();
                if self.show_settings {
                    if ui.button("‹ Volver a la conversación").clicked() {
                        self.show_settings = false;
                        self.persist();
                    }
                    egui::ScrollArea::vertical().id_salt("assistant_settings").show(ui, |ui| {
                        self.settings_ui(ui);
                    });
                } else {
                    ui.horizontal(|ui| {
                        if ui.button("Nueva conversación").clicked() && self.agent.is_none() {
                            self.transcript.clear();
                            self.prompt.clear();
                        }
                        ui.menu_button("⋯", |ui| {
                            if ui.button("Configurar IA").clicked() {
                                self.show_settings = true;
                                ui.close();
                            }
                            if ui.button("Exportar una copia…").clicked() {
                                self.status = self.editor.export(&self.platform).err().unwrap_or_else(|| "Elige dónde guardar la copia".into());
                                ui.close();
                            }
                        });
                    });
                    self.agent_ui(ui);
                }
                if !self.status.is_empty() {
                    ui.separator();
                    ui.add(egui::Label::new(&self.status).wrap());
                }
            });
        }
        self.editor.ui(ui);
        match self.editor.host_action() {
            Some("assistant") => self.show_agent = !self.show_agent,
            Some("settings") => {
                self.show_settings = true;
                self.show_agent = true;
            }
            Some("export") => self.status = self.editor.export(&self.platform).err().unwrap_or_else(|| "Elige dónde guardar la copia".into()),
            _ => {}
        }
        let copies: Vec<String> = ui.ctx().output(|out| {
            out.commands
                .iter()
                .filter_map(|command| match command {
                    egui::OutputCommand::CopyText(text) => Some(text.clone()),
                    _ => None,
                })
                .collect()
        });
        for text in copies {
            let _ = self.platform.exchange(json!({"op":"clipboard_set","text":text}));
        }
        let links: Vec<String> = ui.ctx().output(|out| {
            out.commands
                .iter()
                .filter_map(|command| match command {
                    egui::OutputCommand::OpenUrl(link) => Some(link.url.clone()),
                    _ => None,
                })
                .collect()
        });
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
        if !self.show_settings || !self.show_agent {
            for key in &mut self.keys {
                key.zeroize();
            }
        }
    }
    fn on_exit(&mut self) {
        self.stop();
        self.editor.recover(&self.dir);
        self.persist();
        for key in &mut self.keys {
            key.zeroize();
        }
    }
}
