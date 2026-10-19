//! The assistant panel: state and drawing only. It returns [`Action`]s; the Android host runs
//! the agent and talks to the platform. Compiles on every target so it can be rendered and
//! tested off the device (`cargo run -p <app>-android --example assistant_preview`).
#![forbid(unsafe_code)]

use crate::{
    attach::Attachment,
    protocol::{self, MODELS, Provider},
};
use egui::{
    Align, Align2, Color32, Context, CornerRadius, Frame, Key, Layout, Margin, Pos2, Rect, RichText, Sense, Shape, Stroke, TextureHandle, Ui, Vec2,
    pos2, vec2,
};
use serde::{Deserialize, Serialize};
use zeroize::Zeroize;

/// What differs between the apps.
pub struct Branding {
    pub name: &'static str,
    /// How the composer names the open file ("el documento").
    pub noun: &'static str,
    pub welcome: &'static str,
    /// (button, prompt) suggestions shown before the first message.
    pub suggestions: &'static [(&'static str, &'static str)],
}

#[derive(Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// AI access: off guarantees no request leaves the device.
    pub ai: bool,
    /// Empty: the recommended model of whichever provider has a key.
    pub model: String,
    pub effort: String,
}
impl Default for Settings {
    fn default() -> Self {
        Self { ai: true, model: String::new(), effort: "medium".into() }
    }
}

/// Interface sizes the settings step through (zoom over the system's own scale).
pub const SCALES: [f32; 9] = [0.75, 0.8, 0.9, 1.0, 1.1, 1.2, 1.3, 1.4, 1.5];

/// A sent file as the conversation shows it (its bytes went to the provider).
#[derive(Clone)]
pub struct Chip {
    pub name: String,
    pub detail: String,
    pub tag: String,
    pub texture: Option<TextureHandle>,
}
impl From<&Attachment> for Chip {
    fn from(file: &Attachment) -> Self {
        Self { name: file.name.clone(), detail: file.detail.clone(), tag: file.tag(), texture: file.texture.clone() }
    }
}

pub struct Step {
    pub label: String,
    pub ok: bool,
    pub detail: String,
}

pub enum Entry {
    User { text: String, files: Vec<Chip> },
    Assistant(String),
    Steps(Vec<Step>),
    Error { text: String, retry: bool, key: bool },
    Notice(String),
}

#[derive(PartialEq, Eq, Clone, Copy, Debug, Default)]
pub enum View {
    #[default]
    Chat,
    Settings,
}

#[derive(Debug, PartialEq)]
pub enum Action {
    Send(String),
    Stop,
    Retry,
    NewChat,
    SaveSettings,
    SetKey(Provider, String),
    DeleteKey(Provider),
    /// Open Android's file picker for an attachment.
    Attach,
    /// Attach the image on the clipboard.
    PasteImage,
    RemoveAttachment(usize),
    /// The interface size, as a zoom over the system's scale.
    SetScale(f32),
}

/// Work in progress, as the host reports it each frame.
#[derive(Clone, Copy)]
pub struct Busy {
    pub seconds: u64,
    pub editing: bool,
}

#[derive(Default)]
pub struct Assistant {
    pub settings: Settings,
    /// The editor's interface size, as the host reads it each frame.
    pub ui_scale: f32,
    /// Files for the next message.
    pub attachments: Vec<Attachment>,
    /// A file is being picked or prepared.
    pub attaching: bool,
    pub attach_error: Option<String>,
    pub key_edits: [String; 2],
    /// Saved keys: [OpenAI, Anthropic], from the Keystore's flags.
    pub keys: [bool; 2],
    pub open: bool,
    pub view: View,
    pub prompt: String,
    pub entries: Vec<Entry>,
    pub busy: Option<Busy>,
    actions: Vec<Action>,
    composer_height: f32,
}

pub const COMPOSER_ID: &str = "assistant_composer";

fn key_index(provider: Provider) -> usize {
    match provider {
        Provider::OpenAi => 0,
        Provider::Anthropic => 1,
    }
}

/// Colours derived from the editor's own theme so the panel belongs to the app.
struct Palette {
    bg: Color32,
    card: Color32,
    border: Color32,
    text: Color32,
    strong: Color32,
    weak: Color32,
    accent: Color32,
    on_accent: Color32,
    error: Color32,
}
impl Palette {
    fn of(ui: &Ui) -> Self {
        let v = ui.visuals();
        // The theme's link colour is its strong accent (selection fills are pale tints).
        let accent = v.hyperlink_color;
        let lum = |c: Color32| 0.299 * f32::from(c.r()) + 0.587 * f32::from(c.g()) + 0.114 * f32::from(c.b());
        Self {
            bg: v.panel_fill,
            card: v.extreme_bg_color,
            border: v.widgets.noninteractive.bg_stroke.color,
            text: v.text_color(),
            strong: v.strong_text_color(),
            weak: v.weak_text_color(),
            accent,
            on_accent: if lum(accent) > 150.0 { Color32::BLACK } else { Color32::WHITE },
            error: v.error_fg_color,
        }
    }
}

#[derive(Clone, Copy)]
enum Icon {
    Plus,
    Close,
    Gear,
    Back,
    Sparkle,
    Send,
    Stop,
    Chevron,
    Attach,
}

fn paint_icon(painter: &egui::Painter, icon: Icon, rect: Rect, color: Color32) {
    let c = rect.center();
    let r = rect.width().min(rect.height()) / 2.0;
    let stroke = Stroke::new((r * 0.16).max(1.4), color);
    match icon {
        Icon::Plus => {
            painter.line_segment([c - vec2(r * 0.6, 0.0), c + vec2(r * 0.6, 0.0)], stroke);
            painter.line_segment([c - vec2(0.0, r * 0.6), c + vec2(0.0, r * 0.6)], stroke);
        }
        Icon::Close => {
            let d = r * 0.48;
            painter.line_segment([c - vec2(d, d), c + vec2(d, d)], stroke);
            painter.line_segment([c + vec2(-d, d), c + vec2(d, -d)], stroke);
        }
        Icon::Back => {
            let d = r * 0.42;
            painter.line_segment([c + vec2(d * 0.5, -d), c - vec2(d * 0.5, 0.0)], stroke);
            painter.line_segment([c - vec2(d * 0.5, 0.0), c + vec2(d * 0.5, d)], stroke);
        }
        Icon::Chevron => {
            let d = r * 0.4;
            painter.line_segment([c + vec2(-d, -d * 0.4), c + vec2(0.0, d * 0.5)], stroke);
            painter.line_segment([c + vec2(0.0, d * 0.5), c + vec2(d, -d * 0.4)], stroke);
        }
        Icon::Gear => {
            for i in 0..8 {
                let a = i as f32 * std::f32::consts::TAU / 8.0;
                let dir = vec2(a.cos(), a.sin());
                painter.line_segment([c + dir * r * 0.5, c + dir * r * 0.78], Stroke::new(stroke.width * 1.6, color));
            }
            painter.circle_stroke(c, r * 0.5, stroke);
            painter.circle_stroke(c, r * 0.18, stroke);
        }
        Icon::Sparkle => {
            let star = |center: Pos2, size: f32| {
                let k = size * 0.26;
                vec![
                    center + vec2(0.0, -size),
                    center + vec2(k, -k),
                    center + vec2(size, 0.0),
                    center + vec2(k, k),
                    center + vec2(0.0, size),
                    center + vec2(-k, k),
                    center + vec2(-size, 0.0),
                    center + vec2(-k, -k),
                ]
            };
            // Concave stars: fan them from the centre as triangles.
            for (center, size) in [(c + vec2(-r * 0.18, r * 0.12), r * 0.66), (c + vec2(r * 0.55, -r * 0.58), r * 0.32)] {
                let points = star(center, size);
                for i in 0..points.len() {
                    let next = points[(i + 1) % points.len()];
                    painter.add(Shape::convex_polygon(vec![center, points[i], next], color, Stroke::NONE));
                }
            }
        }
        Icon::Send => {
            let d = r * 0.5;
            painter.line_segment([c + vec2(0.0, d), c - vec2(0.0, d)], stroke);
            painter.line_segment([c - vec2(0.0, d), c + vec2(-d * 0.75, -d * 0.2)], stroke);
            painter.line_segment([c - vec2(0.0, d), c + vec2(d * 0.75, -d * 0.2)], stroke);
        }
        Icon::Stop => {
            painter.rect_filled(Rect::from_center_size(c, Vec2::splat(r * 0.75)), 2.0, color);
        }
        Icon::Attach => {
            // A paper clip: three straight runs joined by two half turns, tilted.
            let mut points = Vec::new();
            let mut arc = |center: Vec2, radius: f32, from: f32, to: f32| {
                for i in 0..=10 {
                    let a = from + (to - from) * i as f32 / 10.0;
                    points.push(center + radius * vec2(a.cos(), a.sin()));
                }
            };
            use std::f32::consts::PI;
            arc(vec2(-0.06, 0.38), 0.18, 0.0, PI);
            arc(vec2(0.03, -0.5), 0.27, PI, 2.0 * PI);
            let mut path = vec![vec2(0.12, -0.3)];
            path.extend(points);
            path.push(vec2(0.30, 0.22));
            let (sin, cos) = 0.7f32.sin_cos();
            let points = path.into_iter().map(|p| c + r * 1.35 * vec2(p.x * cos - p.y * sin, p.x * sin + p.y * cos)).collect();
            painter.add(Shape::line(points, stroke));
        }
    }
}

/// A round icon button with a touch-sized hit area.
fn icon_button(ui: &mut Ui, icon: Icon, hint: &str, palette: &Palette) -> egui::Response {
    sized_icon_button(ui, icon, hint, palette, 40.0)
}

fn sized_icon_button(ui: &mut Ui, icon: Icon, hint: &str, palette: &Palette, side: f32) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(side), Sense::click());
    let enabled = ui.is_enabled();
    if enabled && (response.hovered() || response.is_pointer_button_down_on()) {
        ui.painter().circle_filled(rect.center(), side / 2.0 - 1.0, palette.border.gamma_multiply(0.5));
    }
    let color = if enabled { palette.text } else { palette.weak.gamma_multiply(0.6) };
    paint_icon(ui.painter(), icon, rect.shrink(side * 0.275), color);
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, enabled, hint));
    response.on_hover_text(hint)
}

/// Simple Markdown: headings, bullets, numbered items, **bold** and `code`.
fn markdown(ui: &mut Ui, text: &str, palette: &Palette) {
    let size = ui.style().text_styles.get(&egui::TextStyle::Body).map_or(14.0, |f| f.size).max(14.0);
    let mut in_code = false;
    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            in_code = !in_code;
            continue;
        }
        if in_code {
            ui.add(egui::Label::new(RichText::new(line).monospace().size(size * 0.9).color(palette.text)).wrap());
            continue;
        }
        let trimmed = line.trim_start();
        if trimmed.is_empty() {
            ui.add_space(size * 0.45);
            continue;
        }
        let indent = (line.len() - trimmed.len()).min(8) as f32 * 4.0;
        let (body, heading, bullet) =
            if let Some(rest) = trimmed.strip_prefix("### ").or_else(|| trimmed.strip_prefix("## ")).or_else(|| trimmed.strip_prefix("# ")) {
                (rest, true, None)
            } else if let Some(rest) = trimmed.strip_prefix("- ").or_else(|| trimmed.strip_prefix("* ")).or_else(|| trimmed.strip_prefix("• ")) {
                (rest, false, Some("•".to_owned()))
            } else if let Some((n, rest)) =
                trimmed.split_once(". ").filter(|(n, _)| !n.is_empty() && n.len() <= 3 && n.bytes().all(|b| b.is_ascii_digit()))
            {
                (rest, false, Some(format!("{n}.")))
            } else {
                (trimmed, false, None)
            };
        let mut job = egui::text::LayoutJob::default();
        let font = egui::FontId::proportional(if heading { size * 1.15 } else { size });
        let mut bold = heading;
        for (i, chunk) in body.split("**").enumerate() {
            if i > 0 {
                bold = !bold;
            }
            for (j, piece) in chunk.split('`').enumerate() {
                let code = j % 2 == 1;
                let format = egui::TextFormat {
                    font_id: if code { egui::FontId::monospace(size * 0.9) } else { font.clone() },
                    color: if bold { palette.strong } else { palette.text },
                    background: if code { palette.border.gamma_multiply(0.4) } else { Color32::TRANSPARENT },
                    line_height: Some(size * 1.45),
                    ..Default::default()
                };
                job.append(piece, 0.0, format);
            }
        }
        if heading {
            ui.add_space(size * 0.35);
        }
        ui.horizontal_top(|ui| {
            ui.add_space(indent);
            if let Some(mark) = &bullet {
                ui.add_sized(vec2(16.0, size * 1.45), egui::Label::new(RichText::new(mark).size(size).color(palette.weak)));
            }
            job.wrap.max_width = ui.available_width();
            ui.add(egui::Label::new(job).wrap().selectable(true));
        });
    }
}

/// An attached file: its thumbnail or type, name and size; returns whether × was pressed.
/// Drawn at a fixed size so it sits the same in left- and right-aligned rows.
fn file_chip(ui: &mut Ui, chip: &Chip, palette: &Palette, removable: bool) -> bool {
    let line = |text: &str, size: f32, color: Color32| {
        let mut job = egui::text::LayoutJob::simple_singleline(text.to_owned(), egui::FontId::proportional(size), color);
        job.wrap = egui::text::TextWrapping::truncate_at_width(170.0);
        ui.painter().layout_job(job)
    };
    let name = line(&chip.name, 13.5, palette.text);
    let detail = line(&chip.detail, 12.0, palette.weak);
    let text_width = name.size().x.max(detail.size().x);
    let size = vec2(6.0 + 38.0 + 10.0 + text_width + if removable { 38.0 } else { 14.0 }, 50.0);
    let (rect, response) = ui.allocate_exact_size(size, Sense::hover());
    let painter = ui.painter();
    painter.rect(rect, CornerRadius::same(12), palette.bg, Stroke::new(1.0, palette.border), egui::StrokeKind::Inside);
    let thumb = Rect::from_min_size(rect.min + vec2(6.0, 6.0), Vec2::splat(38.0));
    if let Some(texture) = &chip.texture {
        // Fill the square, cropping the longer side.
        let size = texture.size_vec2();
        let (w, h) = (size.x.max(1.0), size.y.max(1.0));
        let uv = if w > h {
            let k = (1.0 - h / w) / 2.0;
            Rect::from_min_max(pos2(k, 0.0), pos2(1.0 - k, 1.0))
        } else {
            let k = (1.0 - w / h) / 2.0;
            Rect::from_min_max(pos2(0.0, k), pos2(1.0, 1.0 - k))
        };
        painter.image(texture.id(), thumb, uv, Color32::WHITE);
        painter.rect_stroke(thumb, 6, Stroke::new(1.0, palette.border), egui::StrokeKind::Inside);
    } else {
        painter.rect_filled(thumb, 8, palette.accent.gamma_multiply(0.14));
        painter.text(thumb.center(), Align2::CENTER_CENTER, &chip.tag, egui::FontId::proportional(11.0), palette.accent);
    }
    let x = thumb.max.x + 10.0;
    let top = rect.center().y - (name.size().y + detail.size().y + 1.0) / 2.0;
    let below = top + name.size().y + 1.0;
    painter.galley(pos2(x, top), name, palette.text);
    painter.galley(pos2(x, below), detail, palette.weak);
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, true, &chip.name));
    if !removable {
        return false;
    }
    let close = Rect::from_center_size(pos2(rect.max.x - 21.0, rect.center().y), Vec2::splat(32.0));
    let button = ui.interact(close, response.id.with("remove"), Sense::click());
    if button.hovered() || button.is_pointer_button_down_on() {
        ui.painter().circle_filled(close.center(), 15.0, palette.border.gamma_multiply(0.5));
    }
    paint_icon(ui.painter(), Icon::Close, close.shrink(9.0), palette.text);
    button.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "Quitar el archivo"));
    button.on_hover_text("Quitar el archivo").clicked()
}

/// The panel's text uses egui's own families: an editor may name a family of its own that is
/// empty without its optional font pack, and laying text out with it would fail.
fn own_fonts(ui: &mut Ui) {
    for font in ui.style_mut().text_styles.values_mut() {
        if font.family != egui::FontFamily::Monospace {
            font.family = egui::FontFamily::Proportional;
        }
    }
}

fn provider_title(provider: Provider) -> &'static str {
    match provider {
        Provider::Anthropic => "Claude (Anthropic)",
        Provider::OpenAi => "OpenAI",
    }
}

impl Assistant {
    pub fn new(settings: Settings) -> Self {
        Self { settings, ui_scale: 1.0, ..Default::default() }
    }

    pub fn has_key(&self, provider: Provider) -> bool {
        self.keys[key_index(provider)]
    }

    pub fn set_key_flags(&mut self, flags: &serde_json::Value) {
        for provider in Provider::ALL {
            self.keys[key_index(provider)] = flags[provider.id()].as_bool().unwrap_or(false);
        }
    }

    /// The model the next message goes to.
    pub fn model(&self) -> String {
        if !self.settings.model.is_empty() {
            return self.settings.model.clone();
        }
        let provider = if self.has_key(Provider::Anthropic) || !self.has_key(Provider::OpenAi) { Provider::Anthropic } else { Provider::OpenAi };
        provider.default_model().into()
    }

    pub fn zeroize_keys(&mut self) {
        for key in &mut self.key_edits {
            key.zeroize();
        }
    }

    /// Back: settings → chat → closed. Returns whether Back was used here.
    pub fn back(&mut self) -> bool {
        if self.open && self.view == View::Settings {
            self.view = View::Chat;
            self.actions.push(Action::SaveSettings);
            true
        } else if self.open {
            self.open = false;
            true
        } else {
            false
        }
    }

    pub fn take_actions(&mut self) -> Vec<Action> {
        std::mem::take(&mut self.actions)
    }

    fn header(&mut self, ui: &mut Ui, palette: &Palette) {
        ui.horizontal(|ui| {
            ui.set_min_height(48.0);
            if self.view == View::Settings {
                if icon_button(ui, Icon::Back, "Volver", palette).clicked() {
                    self.view = View::Chat;
                    self.actions.push(Action::SaveSettings);
                }
                ui.label(RichText::new("Ajustes del asistente").strong().size(16.0).color(palette.strong));
            } else {
                self.model_chip(ui, palette);
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if icon_button(ui, Icon::Close, "Cerrar el asistente", palette).clicked() {
                    self.open = false;
                }
                if self.view == View::Chat {
                    if icon_button(ui, Icon::Gear, "Ajustes y claves de API", palette).clicked() {
                        self.view = View::Settings;
                    }
                    let new = ui
                        .add_enabled_ui(self.busy.is_none() && !self.entries.is_empty(), |ui| {
                            icon_button(ui, Icon::Plus, "Nueva conversación", palette)
                        })
                        .inner;
                    if new.clicked() {
                        self.entries.clear();
                        self.prompt.clear();
                        self.actions.push(Action::NewChat);
                    }
                }
            });
        });
    }

    fn model_chip(&mut self, ui: &mut Ui, palette: &Palette) {
        let model = self.model();
        let galley = ui.painter().layout_no_wrap(protocol::label_of(&model), egui::FontId::proportional(14.5), palette.text);
        let size = vec2(galley.size().x + 46.0, 38.0);
        let (rect, response) = ui.allocate_exact_size(size, Sense::click());
        let fill = if response.hovered() { palette.border.gamma_multiply(0.35) } else { palette.card };
        ui.painter().rect(rect, CornerRadius::same(19), fill, Stroke::new(1.0, palette.border), egui::StrokeKind::Inside);
        ui.painter().galley(pos2(rect.min.x + 15.0, rect.center().y - galley.size().y / 2.0), galley, palette.text);
        paint_icon(ui.painter(), Icon::Chevron, Rect::from_center_size(pos2(rect.max.x - 19.0, rect.center().y), Vec2::splat(16.0)), palette.weak);
        response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::ComboBox, true, "Modelo"));
        egui::Popup::menu(&response).show(|ui| {
            own_fonts(ui);
            ui.set_min_width(300.0);
            ui.spacing_mut().button_padding = vec2(10.0, 8.0);
            for provider in [Provider::Anthropic, Provider::OpenAi] {
                ui.add_space(4.0);
                let note = if self.has_key(provider) { "" } else { "  ·  sin clave" };
                ui.label(RichText::new(format!("{}{note}", provider_title(provider))).small().color(palette.weak));
                for m in MODELS.iter().filter(|m| m.provider == provider) {
                    let row = ui.add_enabled_ui(self.busy.is_none(), |ui| {
                        ui.add(egui::Button::selectable(m.id == model, RichText::new(m.label).size(14.5)).min_size(vec2(ui.available_width(), 34.0)))
                    });
                    let row = row.inner;
                    ui.painter().text(
                        pos2(row.rect.max.x - 10.0, row.rect.center().y),
                        Align2::RIGHT_CENTER,
                        m.hint,
                        egui::FontId::proportional(12.0),
                        palette.weak,
                    );
                    if row.clicked() {
                        self.settings.model = m.id.into();
                        self.actions.push(Action::SaveSettings);
                        ui.close();
                    }
                }
            }
            ui.separator();
            if ui.add(egui::Button::new("Claves de API y ajustes…").min_size(vec2(ui.available_width(), 34.0))).clicked() {
                self.view = View::Settings;
                ui.close();
            }
        });
    }

    fn settings_view(&mut self, ui: &mut Ui, palette: &Palette) {
        let section = |ui: &mut Ui, title: &str| {
            ui.add_space(16.0);
            ui.label(RichText::new(title).strong().size(15.0).color(palette.strong));
            ui.add_space(6.0);
        };
        let note = |ui: &mut Ui, text: &str| {
            ui.add(egui::Label::new(RichText::new(text).color(palette.weak)).wrap());
        };
        egui::ScrollArea::vertical().id_salt("assistant_settings").auto_shrink([false; 2]).show(ui, |ui| {
            ui.set_width(ui.available_width() - 6.0);
            section(ui, "Acceso a la IA");
            let before = self.settings.ai;
            egui::ComboBox::from_id_salt("ai_access").width(ui.available_width()).selected_text(if self.settings.ai { "Activado" } else { "Desactivado" }).show_ui(ui, |ui| {
                ui.selectable_value(&mut self.settings.ai, true, "Activado");
                ui.selectable_value(&mut self.settings.ai, false, "Desactivado");
            });
            if before != self.settings.ai {
                if !self.settings.ai {
                    self.actions.push(Action::Stop);
                }
                self.actions.push(Action::SaveSettings);
            }
            ui.add_space(4.0);
            note(ui, "Cada mensaje envía tu petición, el contenido del documento abierto y los archivos que adjuntes al proveedor elegido; el asistente lo lee y lo edita con las mismas órdenes que la línea de comandos. «Desactivado» garantiza que ninguna petición sale del dispositivo.");

            section(ui, "Claves de API");
            note(ui, "Fija un límite de gasto en tu cuenta: esa misma página muestra lo que has usado. Las claves se cifran con el Android Keystore y solo viajan a su proveedor.");
            for provider in [Provider::OpenAi, Provider::Anthropic] {
                let index = key_index(provider);
                ui.add_space(12.0);
                ui.label(RichText::new(provider_title(provider)).color(palette.text));
                ui.add_space(2.0);
                let saved = self.has_key(provider);
                let mut save = false;
                let mut delete = false;
                Frame::new().fill(palette.card).stroke(Stroke::new(1.0, palette.border)).corner_radius(12).inner_margin(Margin::symmetric(12, 6)).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        let edit = &mut self.key_edits[index];
                        let hint = if saved { "•••••••• (guardada)" } else { "Pega tu clave" };
                        let has_text = !edit.trim().is_empty();
                        let field = ui.add(egui::TextEdit::singleline(edit).password(true).frame(Frame::NONE).hint_text(hint).desired_width((ui.available_width() - 96.0).max(60.0)).margin(vec2(0.0, 6.0)));
                        let submit = field.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if has_text {
                                save = ui.button("Guardar").clicked() || submit;
                            } else if saved {
                                delete = ui.button("Eliminar").clicked();
                            }
                        });
                    });
                });
                if save {
                    let key = std::mem::take(&mut self.key_edits[index]);
                    self.actions.push(Action::SetKey(provider, key));
                }
                if delete {
                    self.actions.push(Action::DeleteKey(provider));
                }
                ui.horizontal_wrapped(|ui| {
                    ui.hyperlink_to("Crear una clave", provider.keys_url());
                    ui.label(RichText::new("·").color(palette.weak));
                    ui.hyperlink_to("Fijar un límite de gasto", provider.limits_url());
                });
            }

            section(ui, "Modelo y razonamiento");
            let model = self.model();
            egui::ComboBox::from_id_salt("assistant_model").width(ui.available_width()).selected_text(protocol::label_of(&model)).show_ui(ui, |ui| {
                for m in MODELS {
                    if ui.selectable_label(m.id == model, format!("{} — {}", m.label, m.hint)).clicked() {
                        self.settings.model = m.id.into();
                        self.actions.push(Action::SaveSettings);
                    }
                }
            });
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                for (value, label) in [("low", "Bajo"), ("medium", "Medio"), ("high", "Alto")] {
                    let selected = self.settings.effort == value;
                    let button = egui::Button::selectable(selected, label).min_size(vec2(72.0, 34.0)).corner_radius(10);
                    if ui.add(button).clicked() {
                        self.settings.effort = value.into();
                        self.actions.push(Action::SaveSettings);
                    }
                }
            });
            note(ui, "Más razonamiento da respuestas más cuidadosas, pero más lentas y caras.");

            section(ui, "Pantalla y gestos");
            // Steps, not a slider: the whole interface rescales under the finger as it changes.
            let current = self.ui_scale;
            let smaller = SCALES.iter().rev().copied().find(|s| *s < current - 0.01);
            let larger = SCALES.iter().copied().find(|s| *s > current + 0.01);
            ui.horizontal(|ui| {
                ui.label(RichText::new("Tamaño de la interfaz").color(palette.text));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let step = |ui: &mut Ui, label: &str, to: Option<f32>| {
                        let button = egui::Button::new(RichText::new(label).size(20.0)).corner_radius(10).min_size(vec2(46.0, 40.0));
                        ui.add_enabled(to.is_some(), button).clicked().then_some(to).flatten()
                    };
                    let mut chosen = step(ui, "+", larger);
                    ui.add_sized(vec2(64.0, 40.0), egui::Label::new(RichText::new(format!("{:.0} %", current * 100.0)).size(15.0).strong().color(palette.strong)));
                    chosen = chosen.or(step(ui, "−", smaller));
                    if let Some(scale) = chosen {
                        self.actions.push(Action::SetScale(scale));
                    }
                });
            });
            if (current - 1.0).abs() > 0.01 && ui.button("Restablecer (100 %)").clicked() {
                self.actions.push(Action::SetScale(1.0));
            }
            ui.add_space(4.0);
            note(ui, "Un dedo desplaza la página · tocar = clic · mantener = menú contextual · mantener y arrastrar = seleccionar · dos dedos = zoom. Ratón, lápiz y trackpad funcionan como en el escritorio; Atrás cierra paneles y menús.");
            ui.add_space(18.0);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let done = egui::Button::new(RichText::new("Listo").color(palette.on_accent).strong()).fill(palette.accent).corner_radius(12).min_size(vec2(104.0, 42.0));
                if ui.add(done).clicked() {
                    self.view = View::Chat;
                    self.actions.push(Action::SaveSettings);
                }
            });
            ui.add_space(12.0);
        });
    }

    fn composer(&mut self, ui: &mut Ui, palette: &Palette, brand: &Branding) {
        let id = egui::Id::new(COMPOSER_ID);
        // A hardware Enter sends; Shift+Enter and the on-screen keyboard's Enter add a line.
        let send_key = ui.memory(|m| m.has_focus(id))
            && ui.input_mut(|i| {
                let before = i.events.len();
                i.events.retain(|e| !matches!(e, egui::Event::Key { key: Key::Enter, physical_key: Some(_), modifiers, .. } if !modifiers.shift));
                before != i.events.len()
            });
        let frame = Frame::new()
            .fill(palette.card)
            .stroke(Stroke::new(1.0, palette.border))
            .corner_radius(22)
            .inner_margin(Margin { left: 16, right: 8, top: 12, bottom: 8 })
            .show(ui, |ui| {
                self.pending_files(ui, palette);
                ui.add(
                    egui::TextEdit::multiline(&mut self.prompt)
                        .id(id)
                        .frame(Frame::NONE)
                        .hint_text(format!("Pregunta o pide un cambio en {}…", brand.noun))
                        .desired_rows(2)
                        .desired_width(f32::INFINITY)
                        .font(egui::FontId::proportional(15.0)),
                );
                ui.horizontal(|ui| {
                    let attach = ui
                        .add_enabled_ui(!self.attaching, |ui| sized_icon_button(ui, Icon::Attach, "Adjuntar imagen o archivo", palette, 40.0))
                        .inner;
                    egui::Popup::menu(&attach).show(|ui| {
                        own_fonts(ui);
                        ui.set_min_width(250.0);
                        let row = |ui: &mut Ui, label: &str| {
                            ui.add(egui::Button::new(RichText::new(label).size(14.5)).frame(false).min_size(vec2(ui.available_width(), 38.0)))
                        };
                        if row(ui, "Imagen o archivo…").clicked() {
                            self.actions.push(Action::Attach);
                            ui.close();
                        }
                        if row(ui, "Pegar imagen copiada").clicked() {
                            self.actions.push(Action::PasteImage);
                            ui.close();
                        }
                        ui.add(
                            egui::Label::new(
                                RichText::new("Imágenes, PDF, Word, Excel, PowerPoint, OpenDocument y texto").small().color(palette.weak),
                            )
                            .wrap(),
                        );
                    });
                    if let Some(busy) = self.busy {
                        ui.spinner();
                        let doing = if busy.editing { "Trabajando en el documento" } else { "Pensando" };
                        ui.label(RichText::new(format!("{doing}… {} s", busy.seconds)).color(palette.weak));
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let (rect, response) = ui.allocate_exact_size(Vec2::splat(40.0), Sense::click());
                        let running = self.busy.is_some();
                        let ready = (!self.prompt.trim().is_empty() || !self.attachments.is_empty()) && !self.attaching;
                        let fill = if running || ready { palette.accent } else { palette.border.gamma_multiply(0.8) };
                        ui.painter().circle_filled(rect.center(), 20.0, fill);
                        paint_icon(
                            ui.painter(),
                            if running { Icon::Stop } else { Icon::Send },
                            rect.shrink(10.0),
                            if running || ready { palette.on_accent } else { palette.card },
                        );
                        let hint = if running { "Detener" } else { "Enviar" };
                        response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, hint));
                        if response.clicked() {
                            if running {
                                self.actions.push(Action::Stop);
                            } else if ready {
                                self.actions.push(Action::Send(std::mem::take(&mut self.prompt)));
                            }
                        }
                    });
                });
            });
        self.composer_height = frame.response.rect.height();
        if send_key && self.busy.is_none() && !self.attaching && (!self.prompt.trim().is_empty() || !self.attachments.is_empty()) {
            self.actions.push(Action::Send(std::mem::take(&mut self.prompt)));
        }
    }

    /// The files waiting for the next message, above the text field.
    fn pending_files(&mut self, ui: &mut Ui, palette: &Palette) {
        if self.attachments.is_empty() && !self.attaching && self.attach_error.is_none() {
            return;
        }
        let mut remove = None;
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
            for (index, file) in self.attachments.iter().enumerate() {
                if file_chip(ui, &Chip::from(file), palette, true) {
                    remove = Some(index);
                }
            }
            if self.attaching {
                Frame::new()
                    .fill(palette.bg)
                    .stroke(Stroke::new(1.0, palette.border))
                    .corner_radius(12)
                    .inner_margin(Margin::symmetric(12, 15))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label(RichText::new("Preparando el archivo…").size(13.5).color(palette.weak));
                        });
                    });
            }
        });
        if let Some(error) = &self.attach_error {
            ui.add(egui::Label::new(RichText::new(error).size(13.5).color(palette.error)).wrap());
        }
        if let Some(index) = remove {
            self.actions.push(Action::RemoveAttachment(index));
        }
        ui.add_space(6.0);
    }

    fn chat_view(&mut self, ui: &mut Ui, palette: &Palette, brand: &Branding) {
        if self.entries.is_empty() {
            egui::ScrollArea::vertical().id_salt("assistant_empty").auto_shrink([false; 2]).show(ui, |ui| {
                ui.set_width(ui.available_width() - 6.0);
                ui.add_space((ui.available_height() * 0.1).clamp(16.0, 72.0));
                ui.vertical_centered(|ui| {
                    let (rect, _) = ui.allocate_exact_size(Vec2::splat(44.0), Sense::hover());
                    paint_icon(ui.painter(), Icon::Sparkle, rect, palette.accent);
                    ui.add_space(12.0);
                    ui.add(egui::Label::new(RichText::new(brand.welcome).color(palette.weak).size(15.5)).wrap().halign(Align::Center));
                });
                ui.add_space(22.0);
                self.composer(ui, palette, brand);
                ui.add_space(16.0);
                for (i, (label, prompt)) in brand.suggestions.iter().enumerate() {
                    let fill = if i == 0 { palette.accent.gamma_multiply(0.07) } else { palette.card };
                    let stroke = if i == 0 { palette.accent.gamma_multiply(0.45) } else { palette.border };
                    let button = egui::Button::new(RichText::new(*label).size(15.0).color(palette.text))
                        .fill(fill)
                        .stroke(Stroke::new(1.0, stroke))
                        .corner_radius(14)
                        .min_size(vec2(ui.available_width(), 48.0));
                    if ui.add_enabled(self.busy.is_none(), button).clicked() {
                        self.actions.push(Action::Send((*prompt).to_owned()));
                    }
                    ui.add_space(8.0);
                }
            });
            return;
        }
        // The composer grows with its files; last frame's height leaves it room.
        let composer_height = self.composer_height.max(112.0) + 12.0;
        let list_height = (ui.available_height() - composer_height).max(80.0);
        let mut retry = false;
        let mut settings = false;
        let count = self.entries.len();
        egui::ScrollArea::vertical().id_salt("assistant_conversation").max_height(list_height).auto_shrink([false; 2]).stick_to_bottom(true).show(
            ui,
            |ui| {
                let width = ui.available_width() - 6.0;
                ui.set_width(width);
                for (index, entry) in self.entries.iter().enumerate() {
                    ui.add_space(10.0);
                    match entry {
                        Entry::User { text, files } => {
                            if !files.is_empty() {
                                ui.with_layout(Layout::right_to_left(Align::Min).with_main_wrap(true), |ui| {
                                    ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
                                    for file in files {
                                        file_chip(ui, file, palette, false);
                                    }
                                });
                            }
                            if !text.is_empty() {
                                if !files.is_empty() {
                                    ui.add_space(4.0);
                                }
                                ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                                    Frame::new()
                                        .fill(palette.accent.gamma_multiply(0.13))
                                        .corner_radius(18)
                                        .inner_margin(Margin::symmetric(14, 10))
                                        .show(ui, |ui| {
                                            ui.set_max_width(width * 0.82);
                                            ui.add(egui::Label::new(RichText::new(text).size(15.0).color(palette.text)).wrap().selectable(true));
                                        });
                                });
                            }
                        }
                        Entry::Assistant(text) => markdown(ui, text, palette),
                        Entry::Steps(steps) => {
                            let failed = steps.iter().filter(|s| !s.ok).count();
                            let title = if failed == 0 {
                                format!("✓  {} {} en el documento", steps.len(), if steps.len() == 1 { "acción" } else { "acciones" })
                            } else {
                                format!("{} acciones · {failed} con error", steps.len())
                            };
                            Frame::new()
                                .fill(palette.card)
                                .stroke(Stroke::new(1.0, palette.border))
                                .corner_radius(12)
                                .inner_margin(Margin::symmetric(10, 4))
                                .show(ui, |ui| {
                                    ui.set_width(width - 22.0);
                                    egui::CollapsingHeader::new(RichText::new(title).color(palette.weak)).id_salt(("steps", index)).show(ui, |ui| {
                                        for step in steps {
                                            ui.horizontal_wrapped(|ui| {
                                                ui.label(RichText::new(if step.ok { "✓" } else { "✗" }).color(if step.ok {
                                                    palette.accent
                                                } else {
                                                    palette.error
                                                }));
                                                ui.label(RichText::new(&step.label).small().color(palette.text));
                                                if !step.detail.is_empty() {
                                                    ui.label(RichText::new(&step.detail).small().color(palette.weak));
                                                }
                                            });
                                        }
                                    });
                                });
                        }
                        Entry::Error { text, retry: can_retry, key } => {
                            Frame::new()
                                .fill(palette.error.gamma_multiply(0.07))
                                .stroke(Stroke::new(1.0, palette.error.gamma_multiply(0.45)))
                                .corner_radius(12)
                                .inner_margin(Margin::same(12))
                                .show(ui, |ui| {
                                    ui.set_width(width - 26.0);
                                    ui.add(egui::Label::new(RichText::new(text).color(palette.text)).wrap().selectable(true));
                                    if index + 1 == count && (*can_retry || *key) {
                                        ui.add_space(6.0);
                                        ui.horizontal(|ui| {
                                            if *key && ui.button("Añadir clave de API").clicked() {
                                                settings = true;
                                            }
                                            if *can_retry && ui.button("Reintentar").clicked() {
                                                retry = true;
                                            }
                                        });
                                    }
                                });
                        }
                        Entry::Notice(text) => {
                            ui.vertical_centered(|ui| ui.add(egui::Label::new(RichText::new(text).small().color(palette.weak)).wrap()));
                        }
                    }
                }
                ui.add_space(10.0);
            },
        );
        if settings {
            self.view = View::Settings;
        }
        if retry {
            self.actions.push(Action::Retry);
        }
        ui.add_space(6.0);
        self.composer(ui, palette, brand);
    }

    fn contents(&mut self, ui: &mut Ui, palette: &Palette, brand: &Branding) {
        own_fonts(ui);
        ui.spacing_mut().interact_size.y = ui.spacing().interact_size.y.max(30.0);
        ui.spacing_mut().button_padding = vec2(12.0, 7.0);
        self.header(ui, palette);
        ui.add_space(4.0);
        match self.view {
            View::Chat => self.chat_view(ui, palette, brand),
            View::Settings => self.settings_view(ui, palette),
        }
    }

    /// The side panel; on a narrow screen a full-screen layer over the editor.
    /// Call before the editor's own UI.
    pub fn panel(&mut self, ui: &mut Ui, brand: &Branding) {
        let screen = ui.ctx().content_rect();
        let palette = Palette::of(ui);
        let margin = Margin { left: 14, right: 10, top: 6, bottom: 12 };
        if screen.width() < 720.0 {
            egui::Area::new(egui::Id::new("assistant_fullscreen")).order(egui::Order::Foreground).fixed_pos(screen.min).show(ui.ctx(), |ui| {
                Frame::new().fill(palette.bg).inner_margin(margin).show(ui, |ui| {
                    let size = screen.size() - vec2(f32::from(margin.left + margin.right), f32::from(margin.top + margin.bottom));
                    ui.set_min_size(size);
                    ui.set_max_size(size);
                    self.contents(ui, &palette, brand);
                });
            });
            return;
        }
        let frame = Frame::new().fill(palette.bg).inner_margin(margin).stroke(Stroke::new(1.0, palette.border));
        egui::Panel::right("android_assistant")
            .frame(frame)
            .show_separator_line(false)
            .default_size(400.0)
            .min_size(330.0)
            .max_size((screen.width() * 0.5).max(340.0))
            .resizable(true)
            .show(ui, |ui| self.contents(ui, &palette, brand));
    }

    /// The floating button that opens the assistant from anywhere (drawn while it is closed).
    pub fn launcher(&mut self, ctx: &Context) {
        if self.open {
            return;
        }
        let screen = ctx.content_rect();
        let center = pos2(screen.max.x - 58.0, screen.max.y - 88.0);
        egui::Area::new(egui::Id::new("assistant_launcher")).order(egui::Order::Foreground).fixed_pos(center - Vec2::splat(28.0)).show(ctx, |ui| {
            let palette = Palette::of(ui);
            let (rect, response) = ui.allocate_exact_size(Vec2::splat(56.0), Sense::click());
            let painter = ui.painter();
            painter.circle_filled(rect.center() + vec2(0.0, 2.5), 28.5, Color32::from_black_alpha(45));
            painter.circle_filled(rect.center(), 28.0, if response.hovered() { palette.accent.gamma_multiply(0.92) } else { palette.accent });
            paint_icon(painter, Icon::Sparkle, rect.shrink(16.0), palette.on_accent);
            response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "Asistente"));
            if response.on_hover_text("Asistente").clicked() {
                self.open = true;
                self.view = View::Chat;
            }
        });
    }
}
