//! Renders the Android assistant panel beside the real editor, off screen (wgpu), so its
//! design can be checked without a tablet: closed (floating button), empty, a conversation,
//! an error, the settings, attached files, and a narrow phone-sized window.
//!
//! `cargo run -p wordcraft-android --example assistant_preview -- /abs/out/dir`
use wordcraft_android::assistant::{Assistant, Busy, Chip, Entry, Step, View};
use wordcraft_android::attach;
use wordcraft_engine::Session;
use wordcraft_ui_egui::{Services, WordApp};

struct State {
    app: WordApp,
    assistant: Assistant,
}

fn scenario(name: &str) -> Assistant {
    let mut a = Assistant::new(Default::default());
    a.open = name != "closed";
    a.keys = [true, false];
    a.settings.model = "gpt-6-luna".into();
    match name {
        "conversation" | "phone" => {
            a.entries = vec![
                Entry::User { text: "¿De qué trata este documento? Luego pon el título en negrita.".into(), files: vec![] },
                Entry::Assistant("## Resumen\nEl documento presenta el **estudio** de diseño y sus servicios:\n- Identidad visual y tipografía\n- Maquetación de publicaciones\n1. Propuesta\n2. Calendario\n\nAhora aplico la negrita al título.".into()),
                Entry::Steps(vec![
                    Step { label: "Buscó órdenes: «bold»".into(), ok: true, detail: String::new() },
                    Step { label: "Bold (format.bold)".into(), ok: true, detail: String::new() },
                ]),
                Entry::Assistant("Listo: el título «Studio» ya está en negrita. Puedes deshacerlo con Ctrl+Z.".into()),
                Entry::User { text: "Ahora corrige la ortografía de todo el texto.".into(), files: vec![] },
            ];
            a.busy = Some(Busy { seconds: 7, editing: true });
        }
        "error" => {
            a.settings.model = "claude-opus-5-5".into();
            a.entries = vec![
                Entry::User { text: "Resume el documento".into(), files: vec![] },
                Entry::Error { text: "Falta la clave de API de Anthropic.".into(), retry: false, key: true },
            ];
        }
        "settings" => {
            a.view = View::Settings;
            a.ui_scale = 1.2;
        }
        "attachments" => {
            let chip = |name: &str, detail: &str| Chip {
                name: name.into(),
                detail: detail.into(),
                tag: attach::tag_of(name, attach::Kind::Text),
                texture: None,
            };
            a.entries = vec![
                Entry::User {
                    text: "Resume estos apuntes y añade al documento una tabla con las fechas clave.".into(),
                    files: vec![chip("Historia del Perú – tema 4.pdf", "PDF · 2,4 MB"), chip("cronograma.xlsx", "Hoja de cálculo · 3 120 caracteres")],
                },
                Entry::Assistant("Los apuntes cubren la **Independencia** (1821–1824):\n- Proclamación en Lima\n- Batallas de Junín y Ayacucho\n\nAñado la tabla al final del documento.".into()),
                Entry::User { text: String::new(), files: vec![chip("pizarra.jpg", "Imagen · 1568 × 1176")] },
            ];
            let mut png = Vec::new();
            let img = image::RgbImage::from_fn(1200, 800, |x, y| image::Rgb([(40 + x / 8) as u8, (90 + y / 6) as u8, 170]));
            let _ = image::DynamicImage::ImageRgb8(img).write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png);
            a.attachments = [attach::prepare("foto de la pizarra.png", &png), attach::prepare("lectura.pdf", b"%PDF-1.7 apuntes")]
                .into_iter()
                .flatten()
                .collect();
            a.attach_error = Some("«video.mp4» no es una imagen, un PDF ni un documento de texto que se pueda leer.".into());
        }
        _ => {}
    }
    a
}

fn main() {
    let out = std::path::PathBuf::from(std::env::args().nth(1).unwrap_or_else(|| "target/assistant-preview".into()));
    let _ = std::fs::create_dir_all(&out);
    for name in ["closed", "empty", "conversation", "error", "settings", "attachments", "phone"] {
        let size = if name == "phone" { egui::vec2(700.0, 1100.0) } else { egui::vec2(1440.0, 900.0) };
        let services = Services { host_toolbar: Some(Default::default()), ..Default::default() };
        let app = WordApp::new(Session::new(wordcraft_engine::sample::sample_document()), services);
        let state = State { app, assistant: scenario(name) };
        let mut harness = egui_kittest::Harness::builder().with_size(size).with_pixels_per_point(2.0).wgpu().build_ui_state(
            |ui, s: &mut State| {
                let ctx = ui.ctx().clone();
                for file in &mut s.assistant.attachments {
                    if let Some(thumb) = file.thumb.take() {
                        file.texture = Some(ctx.load_texture(file.name.clone(), thumb, egui::TextureOptions::LINEAR));
                    }
                }
                s.app.logic(&ctx);
                if s.assistant.open {
                    s.assistant.panel(ui, &wordcraft_android::BRANDING);
                }
                s.app.ui(ui);
                s.assistant.launcher(&ctx);
            },
            state,
        );
        harness.input_mut().max_texture_side = Some(8192);
        for _ in 0..8 {
            harness.step();
        }
        match harness.render() {
            Ok(image) => {
                let path = out.join(format!("{name}.png"));
                if image.save(&path).is_ok() {
                    println!("{}", path.display());
                }
            }
            Err(e) => eprintln!("{name}: render failed: {e}"),
        }
    }
}
