#![forbid(unsafe_code)]

use crate::platform::Platform;
use egui::{Context, Ui};
use serde_json::{Value, json};
use std::{
    cell::RefCell,
    collections::HashMap,
    path::{Path, PathBuf},
    rc::Rc,
    sync::mpsc::{self, Sender},
};
use wordcraft_engine::Session;
use wordcraft_ui_egui::{FileDialogAnswer, FileDialogRequest, Services, WordApp};

type Pending = Rc<RefCell<HashMap<u64, Sender<FileDialogAnswer>>>>;
pub struct Editor {
    platform: Platform,
    app: WordApp,
    pending: Pending,
    export_path: PathBuf,
}
impl Editor {
    pub const NAME: &'static str = "WordCraft";
    /// The canvas coasts after a flick by itself (wheel phases), so the host must not.
    pub const OWN_MOMENTUM: bool = true;
    pub fn new(dir: &Path, platform: Platform, ctx: &Context) -> Self {
        let _ = wordcraft_fonts::FontDb::global().load_user_fonts(&dir.join("fonts"));
        let pending: Pending = Rc::new(RefCell::new(HashMap::new()));
        let queue = pending.clone();
        let serial = Rc::new(RefCell::new(0u64));
        let dialog_platform = platform.clone();
        let hook = Box::new(move |request: FileDialogRequest| {
            let (tx, rx) = mpsc::channel();
            let mut serial = serial.borrow_mut();
            *serial = serial.saturating_add(1);
            let ticket = *serial;
            let request = match request {
                FileDialogRequest::Open { purpose } | FileDialogRequest::OpenMany { purpose } => {
                    json!({"op":"open","ticket":ticket,"purpose":purpose})
                }
                FileDialogRequest::Save { name } => json!({"op":"save_picker","ticket":ticket,"name":name}),
            };
            match dialog_platform.exchange(request) {
                Ok(_) => {
                    queue.borrow_mut().insert(ticket, tx);
                }
                Err(e) => {
                    let _ = tx.send(Err(e));
                }
            }
            rx
        });
        let clipboard = platform.clone();
        let html_clipboard = platform.clone();
        let copy_clipboard = platform.clone();
        let picture_clipboard = platform.clone();
        let print = platform.clone();
        let print_path = dir.join("print.pdf");
        let mut session = Session::new(wordcraft_doc::Document::new());
        let dictionary = dir.join("user-dictionary.txt");
        wordcraft_proof::load_user_dictionary(&dictionary);
        session.user_dict_path = Some(dictionary);
        let recovery = dir.join("recovery");
        let repaint = ctx.clone();
        let _ = session.enable_recovery(recovery, std::sync::Arc::new(move || repaint.request_repaint()));
        let mut app = WordApp::new(
            session,
            Services {
                host_toolbar: Some(Default::default()),
                file_dialog: Some(hook),
                clipboard_html: Some(Box::new(move || html_clipboard.clipboard("html"))),
                set_clipboard_html: Some(Box::new(move |html, text, _mathml| copy_clipboard.copy_html(html, text).is_ok())),
                clipboard_picture: Some(Box::new(move || {
                    picture_clipboard.clipboard_image().map(wordcraft_ui_egui::paste_picture::ClipboardPicture::Encoded)
                })),
                clipboard_text: Some(Box::new(move || {
                    clipboard.exchange(json!({"op":"clipboard_get"})).ok()?.get("text")?.as_str().map(str::to_owned)
                })),
                print: Some(Box::new(move |bytes| {
                    std::fs::write(&print_path, bytes).map_err(|e| e.to_string())?;
                    print.exchange(json!({"op":"print","path":print_path})).map(|_| ())
                })),
                ..Default::default()
            },
        );
        if let Ok(bytes) = std::fs::read(dir.join("editor.json"))
            && let Some(prefs) = wordcraft_ui_egui::UiState::from_json(&bytes)
        {
            app.apply_prefs(prefs);
        }
        app.start_screen();
        Self { app, pending, platform, export_path: dir.join("export.docx") }
    }
    pub fn logic(&mut self, ctx: &Context) {
        self.app.logic(ctx);
    }
    pub fn raw_input(&mut self, raw: &mut egui::RawInput) {
        self.app.raw_input_hook(raw);
    }
    pub fn ui(&mut self, ui: &mut Ui) {
        self.app.ui(ui);
    }
    pub fn host_state(&mut self, open: bool, busy: bool, status: &str) {
        if let Some(host) = &mut self.app.services.host_toolbar {
            host.assistant_open = open;
            host.busy = busy;
            host.status = status.into();
        }
    }
    pub fn host_action(&mut self) -> Option<&'static str> {
        self.app.services.host_toolbar.as_mut()?.action.take()
    }
    pub fn run(&mut self, id: &str, params: Value) -> Result<Value, String> {
        self.app.run(id, params)
    }
    pub fn agent_save(&mut self) -> Result<Value, String> {
        let path = if let Some(path) = &self.app.session.path {
            path.to_string_lossy().into_owned()
        } else {
            self.platform.exchange(json!({"op":"local_save","name":"document.docx"}))?["path"]
                .as_str()
                .ok_or("No se pudo crear el archivo local")?
                .to_owned()
        };
        self.app.run("file.save", json!({"path":path}))?;
        if self.app.session.dirty || !Path::new(&path).is_file() {
            return Err("El guardado necesita completarse en el editor".into());
        }
        Ok(json!({"saved":true,"scope":"local_copy","name":Path::new(&path).file_name().and_then(|n| n.to_str())}))
    }
    pub fn catalog(&self) -> Value {
        self.app.session.registry.describe()
    }
    pub fn context(&mut self) -> Value {
        if !self.app.has_document()
            && let Err(e) = self.app.run("file.new", json!({}))
        {
            return json!({"error":e});
        }
        self.app.session.run("document.inspect", &json!({})).unwrap_or_else(|e| json!({"error":e.to_string()}))
    }
    pub fn file_event(&mut self, event: &Value) {
        if event["external"] == true
            && let Some(path) = event["path"].as_str()
        {
            if let Err(error) = self.app.run("file.open", json!({"path":path})) {
                self.app.status(error);
            }
            return;
        }
        let ticket = event["ticket"].as_u64().unwrap_or(0);
        if let Some(tx) = self.pending.borrow_mut().remove(&ticket) {
            let answer = if let Some(error) = event["error"].as_str() {
                Err(error.into())
            } else if event["cancelled"] == true {
                Ok(vec![])
            } else {
                Ok(event["path"].as_str().into_iter().map(str::to_owned).collect())
            };
            let _ = tx.send(answer);
        }
    }
    pub fn persist(&self, dir: &Path) {
        if let Ok(bytes) = serde_json::to_vec(&self.app.prefs()) {
            let _ = std::fs::write(dir.join("editor.json"), bytes);
        }
    }
    pub fn recover(&mut self, _dir: &Path) {
        self.app.session.recovery_tick();
    }
    pub fn export(&mut self, platform: &Platform) -> Result<(), String> {
        wordcraft_engine::io::save_path_with(
            &self.export_path,
            &self.app.session.doc,
            self.app.session.password.as_ref().map(|p| p.as_str()),
            &Default::default(),
        )?;
        platform.exchange(json!({"op":"export","path":self.export_path,"name":"document.docx","ticket":9000}))?;
        Ok(())
    }
}
