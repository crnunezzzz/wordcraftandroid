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

struct AssistantHost(Desktop);

thread_local! {
    /// The assistant beside this process's window (eframe runs every frame on one thread). Held
    /// here rather than in `main.rs`'s `App`, so merging the original project's `main.rs` only
    /// meets the four calls below.
    static HOST: std::cell::RefCell<Option<AssistantHost>> = const { std::cell::RefCell::new(None) };
}

fn with(f: impl FnOnce(&mut AssistantHost)) {
    HOST.with(|cell| {
        if let Ok(mut host) = cell.try_borrow_mut() {
            f(host.get_or_insert_with(AssistantHost::new));
        }
    });
}

/// On a Wayland session the window opens natively, like the official app's (its Flatpak offers
/// X11 only as a fallback), not through XWayland: there the desktop draws its own taller title
/// bar, with the document name, above the editor's title row. `WORDCRAFT_X11=1` keeps XWayland.
#[cfg(all(unix, not(target_os = "macos")))]
pub fn native_wayland() -> bool {
    wordcraft_assistant::desktop::native_wayland("WORDCRAFT_X11")
}

/// Before the editor's frame logic.
pub fn frame(ctx: &egui::Context, app: &mut WordApp) {
    // This build updates itself from this fork's releases (`wordcraft_assistant::update`): the
    // original project's updater would install the official WordCraft over it, without the
    // assistant. Turned off before the editor's first logic tick, which would start its check.
    app.set_startup_update_check(false);
    with(|host| host.frame(ctx, app));
}

/// Before the editor's UI: the panel takes the right side of the window.
pub fn panel(ui: &mut egui::Ui, app: &mut WordApp) {
    with(|host| host.panel(ui, app));
}

/// After the editor's UI: the ribbon's Asistente tab, the floating button, the panel's actions.
pub fn finish(ctx: &egui::Context, app: &mut WordApp) {
    with(|host| host.finish(ctx, app));
}

/// Only an assistant that was started has anything to save.
pub fn on_exit() {
    HOST.with(|cell| {
        if let Ok(mut host) = cell.try_borrow_mut()
            && let Some(host) = host.as_mut()
        {
            host.on_exit();
        }
    });
}

impl AssistantHost {
    fn new() -> Self {
        let dir = wordcraft_control_key::settings_dir().unwrap_or_else(|| std::env::temp_dir().join("WordCraft"));
        Self(Desktop::new("WordCraft", &BRANDING, dir))
    }

    fn frame(&mut self, ctx: &egui::Context, app: &mut WordApp) {
        self.0.frame(ctx, &mut Driven(app));
    }

    fn panel(&mut self, ui: &mut egui::Ui, app: &mut WordApp) {
        if let Some(host) = &mut app.services.host_toolbar {
            host.assistant_open = self.0.assistant.open;
            host.busy = self.0.busy();
        }
        self.0.panel(ui);
    }

    fn finish(&mut self, ctx: &egui::Context, app: &mut WordApp) {
        match app.services.host_toolbar.as_mut().and_then(|h| h.action.take()) {
            Some("assistant") => self.0.toggle(),
            Some("settings") => self.0.open_settings(),
            // Help › Check for Updates in this build: this fork's releases, not the original's.
            Some("check_update") => self.0.check_now(),
            _ => {}
        }
        self.0.finish(ctx, &mut Driven(app));
    }

    fn on_exit(&mut self) {
        self.0.on_exit();
    }
}
