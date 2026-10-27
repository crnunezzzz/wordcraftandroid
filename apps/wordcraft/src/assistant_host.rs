//! The AI assistant panel in the desktop app (`wordcraft-assistant`): the editor it drives and
//! the ribbon's Asistente tab. Kept in this file so `main.rs` only wires it in.
#![forbid(unsafe_code)]

use serde_json::{Value, json};
use wordcraft_assistant::{BRANDING, agent::Editor, desktop::Desktop};
use wordcraft_ui_egui::WordApp;

/// The editor as the assistant drives it.
struct Driven<'a>(&'a mut WordApp);

impl Editor for Driven<'_> {
    fn catalog(&self) -> Value {
        self.0.session.registry.describe()
    }
    fn run(&mut self, id: &str, params: Value) -> Result<Value, String> {
        self.0.run(id, params)
    }
    fn save(&mut self) -> Result<Value, String> {
        let Some(path) = self.0.session.path.clone() else {
            return Err("The document has no file yet: ask the user to save it once with File › Save As".into());
        };
        self.0.run("file.save", json!({}))?;
        if self.0.session.dirty {
            return Err("The save did not complete".into());
        }
        Ok(json!({"saved":true,"name":path.file_name().map(|n| n.to_string_lossy().into_owned())}))
    }
    fn context(&mut self) -> Value {
        if !self.0.has_document()
            && let Err(e) = self.0.run("file.new", json!({}))
        {
            return json!({"error":e});
        }
        self.0.session.run("document.inspect", &json!({})).unwrap_or_else(|e| json!({"error":e.to_string()}))
    }
    fn interface_scale(&self, native: f32) -> f32 {
        self.0.ui.interface_size.zoom_factor(native)
    }
    fn set_interface_scale(&mut self, native: f32, scale: f32) {
        use wordcraft_ui_egui::theme::InterfaceSize;
        self.0.ui.interface_size = if (scale - 1.0).abs() < 0.01 {
            InterfaceSize::AUTO
        } else {
            InterfaceSize::from_percent(f64::from(scale * native * 100.0)).unwrap_or(InterfaceSize::AUTO)
        };
    }
}

pub struct AssistantHost(Desktop);

impl AssistantHost {
    pub fn new() -> Self {
        let dir = wordcraft_control_key::settings_dir().unwrap_or_else(|| std::env::temp_dir().join("WordCraft"));
        Self(Desktop::new("WordCraft", &BRANDING, dir))
    }

    /// Before the editor's frame logic.
    pub fn frame(&mut self, ctx: &egui::Context, app: &mut WordApp) {
        self.0.frame(ctx, &mut Driven(app));
    }

    /// Before the editor's UI: the panel takes the right side of the window.
    pub fn panel(&mut self, ui: &mut egui::Ui, app: &mut WordApp) {
        if let Some(host) = &mut app.services.host_toolbar {
            host.assistant_open = self.0.assistant.open;
            host.busy = self.0.busy();
        }
        self.0.panel(ui);
    }

    /// After the editor's UI: the ribbon's Asistente tab, the floating button, the panel's actions.
    pub fn finish(&mut self, ctx: &egui::Context, app: &mut WordApp) {
        match app.services.host_toolbar.as_mut().and_then(|h| h.action.take()) {
            Some("assistant") => self.0.toggle(),
            Some("settings") => self.0.open_settings(),
            _ => {}
        }
        self.0.finish(ctx, &mut Driven(app));
    }

    pub fn on_exit(&mut self) {
        self.0.on_exit();
    }
}
