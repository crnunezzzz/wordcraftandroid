//! Files the user attaches to an assistant message: images, PDFs, and documents read as text.
//! Pure Rust, no platform calls, so it runs on a worker thread and is tested off the device.
#![forbid(unsafe_code)]

use std::io::{Cursor, Read};

/// A picked file larger than this is refused before it is read.
pub const MAX_FILE: u64 = 40 * 1024 * 1024;
/// PDFs travel whole (base64) with every request of the conversation.
pub const MAX_PDF: usize = 12 * 1024 * 1024;
/// Images are scaled so the longest side fits; larger adds cost and no detail the models use.
pub const MAX_EDGE: u32 = 1568;
/// An image the providers accept as it is, when it cannot be decoded here.
const MAX_RAW_IMAGE: usize = 3_500_000;
/// Text read from a document (bytes).
pub const MAX_TEXT: usize = 120 * 1024;
/// Attachments per message.
pub const MAX_FILES: usize = 8;
/// Pixels an image may have before decoding is refused (memory).
const MAX_PIXELS: u64 = 60_000_000;
/// One part of an Office or ODF package.
const MAX_PART: u64 = 48 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Image,
    Pdf,
    Text,
}

/// A file ready to send: image or PDF bytes, or the text read from a document.
pub struct Attachment {
    pub name: String,
    pub kind: Kind,
    pub media_type: &'static str,
    pub data: Vec<u8>,
    pub text: String,
    /// What the chip says under the name ("Imagen · 1568 × 1176").
    pub detail: String,
    pub thumb: Option<egui::ColorImage>,
    /// Loaded from `thumb` on the UI thread.
    pub texture: Option<egui::TextureHandle>,
}

impl std::fmt::Debug for Attachment {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Attachment").field("name", &self.name).field("kind", &self.kind).field("detail", &self.detail).finish_non_exhaustive()
    }
}

impl Attachment {
    fn new(name: &str, kind: Kind, media_type: &'static str, detail: String) -> Self {
        Self { name: name.to_owned(), kind, media_type, data: Vec::new(), text: String::new(), detail, thumb: None, texture: None }
    }

    /// The short tag drawn on a file without a thumbnail ("PDF", "DOCX").
    pub fn tag(&self) -> String {
        tag_of(&self.name, self.kind)
    }

    /// Bytes this attachment adds to every request of its conversation.
    pub fn weight(&self) -> usize {
        match self.kind {
            Kind::Text => self.text.len(),
            _ => self.data.len() / 3 * 4 + 4,
        }
    }
}

pub fn tag_of(name: &str, kind: Kind) -> String {
    let ext = name.rsplit_once('.').map(|(_, e)| e).filter(|e| !e.is_empty() && e.len() <= 4 && e.chars().all(|c| c.is_ascii_alphanumeric()));
    match (ext, kind) {
        (Some(e), _) => e.to_ascii_uppercase(),
        (None, Kind::Image) => "IMG".into(),
        (None, Kind::Pdf) => "PDF".into(),
        (None, Kind::Text) => "TXT".into(),
    }
}

/// Read a picked file into an attachment, or say in the user's language why it cannot be sent.
pub fn prepare(name: &str, bytes: &[u8]) -> Result<Attachment, String> {
    let name = if name.trim().is_empty() { "archivo" } else { name.trim() };
    if bytes.is_empty() {
        return Err(format!("«{name}» está vacío."));
    }
    if bytes.starts_with(b"%PDF-") {
        if bytes.len() > MAX_PDF {
            return Err(format!("«{name}» pesa {}; el máximo para un PDF es {}.", size(bytes.len()), size(MAX_PDF)));
        }
        let mut a = Attachment::new(name, Kind::Pdf, "application/pdf", format!("PDF · {}", size(bytes.len())));
        a.data = bytes.to_vec();
        return Ok(a);
    }
    if let Some(media) = image_type(bytes) {
        return image(name, bytes, media);
    }
    if bytes.len() > 12 && &bytes[4..8] == b"ftyp" && [b"heic", b"heix", b"mif1", b"heif", b"avif"].iter().any(|b| &bytes[8..12] == *b) {
        return Err(format!("«{name}» es una foto HEIC/AVIF, que no se puede leer aquí. Compártela como JPG o haz una captura."));
    }
    if bytes.starts_with(b"PK\x03\x04") {
        let (text, what) = package_text(bytes).map_err(|e| format!("No se pudo leer «{name}»: {e}"))?;
        return Ok(text_attachment(name, text, what));
    }
    match decode_text(bytes) {
        Some(text) => Ok(text_attachment(name, text, "Texto")),
        None => Err(format!("«{name}» no es una imagen, un PDF ni un documento de texto que se pueda leer.")),
    }
}

fn text_attachment(name: &str, mut text: String, what: &str) -> Attachment {
    let total = text.chars().count();
    let cut = clip(&mut text, MAX_TEXT);
    let mut detail = format!("{what} · {} caracteres", thousands(total));
    if cut {
        detail.push_str(" (recortado)");
        text.push_str("\n[… el resto del archivo se recortó por su tamaño]");
    }
    if text.trim().is_empty() {
        detail = format!("{what} · sin texto legible");
    }
    let mut a = Attachment::new(name, Kind::Text, "text/plain", detail);
    a.text = text;
    a
}

fn image_type(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("image/jpeg")
    } else if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if bytes.len() > 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        Some("image/webp")
    } else if bytes.starts_with(b"BM") && bytes.len() > 26 {
        Some("image/bmp")
    } else {
        None
    }
}

fn image(name: &str, bytes: &[u8], media: &'static str) -> Result<Attachment, String> {
    let reader = image::ImageReader::new(Cursor::new(bytes)).with_guessed_format().map_err(|e| e.to_string());
    let dims = reader.and_then(|r| r.into_dimensions().map_err(|e| e.to_string()));
    let decoded = match dims {
        Ok((w, h)) if u64::from(w) * u64::from(h) > MAX_PIXELS => {
            return Err(format!("«{name}» tiene demasiados píxeles ({w} × {h})."));
        }
        Ok(_) => image::load_from_memory(bytes).ok(),
        Err(_) => None,
    };
    let Some(img) = decoded else {
        // Not decodable here (a format this build has no decoder for): the providers still
        // read the four common formats as they are.
        if media != "image/bmp" && bytes.len() <= MAX_RAW_IMAGE {
            let mut a = Attachment::new(name, Kind::Image, media, format!("Imagen · {}", size(bytes.len())));
            a.data = bytes.to_vec();
            return Ok(a);
        }
        return Err(format!("No se pudo leer la imagen «{name}»."));
    };
    let (w, h) = (img.width(), img.height());
    if w == 0 || h == 0 {
        return Err(format!("La imagen «{name}» está vacía."));
    }
    let as_is = w.max(h) <= MAX_EDGE && bytes.len() <= 1_500_000 && matches!(media, "image/jpeg" | "image/png");
    let thumb = img.thumbnail(160, 160).to_rgba8();
    let thumb = egui::ColorImage::from_rgba_unmultiplied([thumb.width() as usize, thumb.height() as usize], thumb.as_raw());
    let (data, media, w, h) = if as_is {
        (bytes.to_vec(), media, w, h)
    } else {
        let img = if w.max(h) > MAX_EDGE { img.resize(MAX_EDGE, MAX_EDGE, image::imageops::FilterType::Triangle) } else { img };
        let mut out = Vec::new();
        let png = img.color().has_alpha() && img.write_to(&mut Cursor::new(&mut out), image::ImageFormat::Png).is_ok() && out.len() <= 2_000_000;
        if png {
            (out, "image/png", img.width(), img.height())
        } else {
            out.clear();
            image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 85)
                .encode_image(&img.to_rgb8())
                .map_err(|e| format!("No se pudo preparar «{name}»: {e}"))?;
            (out, "image/jpeg", img.width(), img.height())
        }
    };
    let mut a = Attachment::new(name, Kind::Image, media, format!("Imagen · {w} × {h}"));
    a.data = data;
    a.thumb = Some(thumb);
    Ok(a)
}

/// Text in UTF-8 or UTF-16 (with its mark), else Latin-1 when it reads as text; None for binary.
fn decode_text(bytes: &[u8]) -> Option<String> {
    let text = if let Some(rest) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        String::from_utf8_lossy(rest).into_owned()
    } else if bytes.starts_with(&[0xFF, 0xFE]) || bytes.starts_with(&[0xFE, 0xFF]) {
        let le = bytes[0] == 0xFF;
        let units: Vec<u16> = bytes[2..].as_chunks::<2>().0.iter().map(|&p| if le { u16::from_le_bytes(p) } else { u16::from_be_bytes(p) }).collect();
        String::from_utf16_lossy(&units)
    } else if let Ok(s) = std::str::from_utf8(bytes) {
        s.to_owned()
    } else {
        bytes.iter().map(|&b| char::from(b)).collect()
    };
    let sample = text.chars().take(64 * 1024);
    let (mut total, mut odd) = (0usize, 0usize);
    for c in sample {
        total += 1;
        if c == '\u{FFFD}' || (c.is_control() && !matches!(c, '\n' | '\r' | '\t' | '\u{C}')) {
            odd += 1;
        }
    }
    (total > 0 && odd * 100 <= total).then_some(text)
}

fn read_part(zip: &mut zip::ZipArchive<Cursor<&[u8]>>, name: &str) -> Option<String> {
    let file = zip.by_name(name).ok()?;
    let mut bytes = Vec::new();
    file.take(MAX_PART).read_to_end(&mut bytes).ok()?;
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

/// Text of a Word, Excel or PowerPoint file, or of an OpenDocument; with what it was.
fn package_text(bytes: &[u8]) -> Result<(String, &'static str), String> {
    let mut zip = zip::ZipArchive::new(Cursor::new(bytes)).map_err(|_| "el archivo comprimido está dañado".to_owned())?;
    let names: Vec<String> = zip.file_names().map(str::to_owned).collect();
    let has = |n: &str| names.iter().any(|x| x == n);
    let mut out = String::new();
    if has("word/document.xml") {
        for part in ["word/document.xml", "word/footnotes.xml", "word/endnotes.xml"] {
            if let Some(xml) = read_part(&mut zip, part) {
                xml_text(&xml, false, &mut out);
            }
        }
        return Ok((out, "Documento de Word"));
    }
    if has("ppt/presentation.xml") {
        for (n, part) in numbered(&names, "ppt/slides/slide") {
            if let Some(xml) = read_part(&mut zip, &part) {
                out.push_str(&format!("\n## Diapositiva {n}\n"));
                xml_text(&xml, false, &mut out);
            }
            if out.len() > MAX_TEXT {
                break;
            }
        }
        return Ok((out, "Presentación"));
    }
    if has("xl/workbook.xml") {
        let shared = read_part(&mut zip, "xl/sharedStrings.xml").map(|x| shared_strings(&x)).unwrap_or_default();
        for (title, part) in sheets(&mut zip, &names) {
            if let Some(xml) = read_part(&mut zip, &part) {
                out.push_str(&format!("\n## Hoja «{title}»\n"));
                sheet_text(&xml, &shared, &mut out);
            }
            if out.len() > MAX_TEXT {
                break;
            }
        }
        return Ok((out, "Hoja de cálculo"));
    }
    if has("content.xml") {
        if let Some(xml) = read_part(&mut zip, "content.xml") {
            xml_text(&xml, true, &mut out);
        }
        return Ok((out, "Documento OpenDocument"));
    }
    Err("no es un documento de Office ni OpenDocument".into())
}

/// Parts named `<prefix><n>.xml`, in number order.
fn numbered(names: &[String], prefix: &str) -> Vec<(u32, String)> {
    let mut parts: Vec<(u32, String)> =
        names.iter().filter_map(|n| n.strip_prefix(prefix)?.strip_suffix(".xml")?.parse::<u32>().ok().map(|i| (i, n.clone()))).collect();
    parts.sort();
    parts
}

fn local(name: &[u8]) -> &[u8] {
    name.rsplit(|&b| b == b':').next().unwrap_or(name)
}

fn attr(e: &quick_xml::events::BytesStart, key: &[u8]) -> Option<String> {
    e.attributes()
        .flatten()
        .find(|a| local(a.key.as_ref()) == key)
        .and_then(|a| a.normalized_value(quick_xml::XmlVersion::Explicit1_0).ok().map(|v| v.into_owned()))
}

/// The text a reference stands for (`&amp;`, `&#65;`).
fn reference(g: &quick_xml::events::BytesRef) -> Option<String> {
    match g.resolve_char_ref() {
        Ok(Some(c)) => Some(c.to_string()),
        Ok(None) => g.decode().ok().and_then(|n| quick_xml::escape::resolve_predefined_entity(&n).map(str::to_owned)),
        Err(_) => None,
    }
}

/// Visible text of WordprocessingML/DrawingML (inside `t`) or of ODF (inside `p` and `h`),
/// one paragraph per line.
fn xml_text(xml: &str, odf: bool, out: &mut String) {
    use quick_xml::events::Event;
    let mut reader = quick_xml::Reader::from_str(xml);
    let mut inside = 0usize;
    let starts_text = |name: &[u8]| if odf { name == b"p" || name == b"h" } else { name == b"t" };
    loop {
        if out.len() > MAX_TEXT + 1024 {
            return;
        }
        match reader.read_event() {
            Ok(Event::Start(e)) => {
                if starts_text(local(e.name().as_ref())) {
                    inside += 1;
                }
            }
            Ok(Event::Empty(e)) => match local(e.name().as_ref()) {
                b"tab" => out.push('\t'),
                b"br" | b"cr" | b"line-break" => out.push('\n'),
                b"s" if odf => out.push(' '),
                b"p" | b"h" => out.push('\n'),
                _ => {}
            },
            Ok(Event::End(e)) => {
                let qname = e.name();
                let name = local(qname.as_ref());
                if starts_text(name) {
                    inside = inside.saturating_sub(1);
                }
                if name == b"p" || name == b"h" {
                    out.push('\n');
                }
            }
            Ok(Event::Text(t)) if inside > 0 => {
                if let Ok(s) = t.decode() {
                    out.push_str(&s);
                }
            }
            Ok(Event::CData(t)) if inside > 0 => out.push_str(&String::from_utf8_lossy(&t)),
            Ok(Event::GeneralRef(g)) if inside > 0 => {
                if let Some(s) = reference(&g) {
                    out.push_str(&s);
                }
            }
            Ok(Event::Eof) | Err(_) => return,
            _ => {}
        }
    }
}

fn shared_strings(xml: &str) -> Vec<String> {
    use quick_xml::events::Event;
    let mut reader = quick_xml::Reader::from_str(xml);
    let mut strings = Vec::new();
    let (mut current, mut in_t, mut in_phonetic) = (String::new(), false, false);
    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => match local(e.name().as_ref()) {
                b"si" => current.clear(),
                b"t" => in_t = true,
                b"rPh" => in_phonetic = true,
                _ => {}
            },
            Ok(Event::Empty(e)) if local(e.name().as_ref()) == b"si" => strings.push(String::new()),
            Ok(Event::End(e)) => match local(e.name().as_ref()) {
                b"si" => strings.push(std::mem::take(&mut current)),
                b"t" => in_t = false,
                b"rPh" => in_phonetic = false,
                _ => {}
            },
            Ok(Event::Text(t)) if in_t && !in_phonetic => {
                if let Ok(s) = t.decode() {
                    current.push_str(&s);
                }
            }
            Ok(Event::GeneralRef(g)) if in_t && !in_phonetic => {
                if let Some(s) = reference(&g) {
                    current.push_str(&s);
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        if strings.len() > 1_000_000 {
            break;
        }
    }
    strings
}

/// The workbook's sheets in tab order: (name, part path).
fn sheets(zip: &mut zip::ZipArchive<Cursor<&[u8]>>, names: &[String]) -> Vec<(String, String)> {
    use quick_xml::events::Event;
    let mut targets = Vec::new();
    if let Some(rels) = read_part(zip, "xl/_rels/workbook.xml.rels") {
        let mut reader = quick_xml::Reader::from_str(&rels);
        loop {
            match reader.read_event() {
                Ok(Event::Start(e) | Event::Empty(e)) if local(e.name().as_ref()) == b"Relationship" => {
                    if let (Some(id), Some(target)) = (attr(&e, b"Id"), attr(&e, b"Target")) {
                        let path = match target.strip_prefix('/') {
                            Some(absolute) => absolute.to_owned(),
                            None => format!("xl/{target}"),
                        };
                        targets.push((id, path));
                    }
                }
                Ok(Event::Eof) | Err(_) => break,
                _ => {}
            }
        }
    }
    let mut found = Vec::new();
    if let Some(workbook) = read_part(zip, "xl/workbook.xml") {
        let mut reader = quick_xml::Reader::from_str(&workbook);
        loop {
            match reader.read_event() {
                Ok(Event::Start(e) | Event::Empty(e)) if local(e.name().as_ref()) == b"sheet" => {
                    let title = attr(&e, b"name").unwrap_or_default();
                    if let Some(id) = attr(&e, b"id")
                        && let Some((_, path)) = targets.iter().find(|(i, _)| *i == id)
                        && names.contains(path)
                    {
                        found.push((title, path.clone()));
                    }
                }
                Ok(Event::Eof) | Err(_) => break,
                _ => {}
            }
        }
    }
    if found.is_empty() {
        found = numbered(names, "xl/worksheets/sheet").into_iter().map(|(n, p)| (format!("Hoja{n}"), p)).collect();
    }
    found
}

/// One line per row: `A1: valor | B1: 12 (=SUMA(…))`.
fn sheet_text(xml: &str, shared: &[String], out: &mut String) {
    use quick_xml::events::Event;
    let mut reader = quick_xml::Reader::from_str(xml);
    let mut row: Vec<String> = Vec::new();
    let (mut cell, mut kind) = (String::new(), String::new());
    let (mut value, mut formula, mut field) = (String::new(), String::new(), 0u8);
    loop {
        if out.len() > MAX_TEXT + 1024 {
            return;
        }
        match reader.read_event() {
            Ok(Event::Start(e)) => match local(e.name().as_ref()) {
                b"row" => row.clear(),
                b"c" => {
                    cell = attr(&e, b"r").unwrap_or_default();
                    kind = attr(&e, b"t").unwrap_or_default();
                    value.clear();
                    formula.clear();
                }
                b"v" | b"t" => field = 1,
                b"f" => field = 2,
                _ => {}
            },
            Ok(Event::End(e)) => match local(e.name().as_ref()) {
                b"v" | b"t" | b"f" => field = 0,
                b"c" => {
                    let shown = match kind.as_str() {
                        "s" => value.trim().parse::<usize>().ok().and_then(|i| shared.get(i)).cloned().unwrap_or_default(),
                        "b" => (if value.trim() == "1" { "VERDADERO" } else { "FALSO" }).to_owned(),
                        _ => value.clone(),
                    };
                    if !shown.is_empty() || !formula.is_empty() {
                        let mut entry = format!("{cell}: {}", shown.replace('\n', " "));
                        if !formula.is_empty() {
                            entry.push_str(&format!(" (={formula})"));
                        }
                        row.push(entry);
                    }
                }
                b"row" if !row.is_empty() => {
                    out.push_str(&row.join(" | "));
                    out.push('\n');
                }
                _ => {}
            },
            Ok(Event::Text(t)) if field > 0 => {
                if let Ok(s) = t.decode() {
                    if field == 1 { value.push_str(&s) } else { formula.push_str(&s) }
                }
            }
            Ok(Event::GeneralRef(g)) if field > 0 => {
                if let Some(s) = reference(&g) {
                    if field == 1 { value.push_str(&s) } else { formula.push_str(&s) }
                }
            }
            Ok(Event::Eof) | Err(_) => return,
            _ => {}
        }
    }
}

/// Cut to at most `max` bytes at a character boundary; whether anything was cut.
fn clip(text: &mut String, max: usize) -> bool {
    if text.len() <= max {
        return false;
    }
    let mut cut = max;
    while !text.is_char_boundary(cut) {
        cut -= 1;
    }
    text.truncate(cut);
    true
}

pub fn size(bytes: usize) -> String {
    if bytes >= 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0)).replace('.', ",")
    } else {
        format!("{} KB", bytes.div_ceil(1024))
    }
}

fn thousands(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push('\u{202F}');
        }
        out.push(c);
    }
    out
}

/// Standard base64 with padding, for data the providers take inline.
pub fn base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [chunk[0], chunk.get(1).copied().unwrap_or(0), chunk.get(2).copied().unwrap_or(0)];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(char::from(TABLE[(n >> (18 - 6 * i) & 63) as usize]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn package(parts: &[(&str, &str)]) -> Vec<u8> {
        let mut out = Cursor::new(Vec::new());
        let mut zip = zip::ZipWriter::new(&mut out);
        for (name, body) in parts {
            zip.start_file(*name, zip::write::SimpleFileOptions::default()).unwrap();
            zip.write_all(body.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
        out.into_inner()
    }

    #[test]
    fn base64_matches_the_standard() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
        assert_eq!(base64(&[0xFF, 0xEE]), "/+4=");
    }

    #[test]
    fn large_images_are_scaled_and_get_a_thumbnail() {
        let img = image::RgbImage::from_fn(3000, 1000, |x, _| image::Rgb([(x % 255) as u8, 80, 160]));
        let mut png = Vec::new();
        image::DynamicImage::ImageRgb8(img).write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png).unwrap();
        let a = prepare("foto.png", &png).unwrap();
        assert_eq!(a.kind, Kind::Image);
        assert_eq!(a.media_type, "image/jpeg");
        let back = image::load_from_memory(&a.data).unwrap();
        assert_eq!((back.width(), back.height()), (MAX_EDGE, 523));
        assert!(a.thumb.is_some());
        assert!(a.detail.contains("1568 × 523"), "{}", a.detail);
    }

    #[test]
    fn small_images_travel_unchanged() {
        let mut png = Vec::new();
        image::DynamicImage::ImageRgba8(image::RgbaImage::new(40, 30)).write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png).unwrap();
        let a = prepare("x.png", &png).unwrap();
        assert_eq!((a.media_type, a.data.as_slice()), ("image/png", png.as_slice()));
    }

    #[test]
    fn pdfs_travel_whole_up_to_the_limit() {
        let a = prepare("apuntes.pdf", b"%PDF-1.7\n...").unwrap();
        assert_eq!((a.kind, a.media_type, a.tag().as_str()), (Kind::Pdf, "application/pdf", "PDF"));
        let mut big = b"%PDF-1.7".to_vec();
        big.resize(MAX_PDF + 1, b' ');
        assert!(prepare("libro.pdf", &big).unwrap_err().contains("máximo"));
    }

    #[test]
    fn text_files_are_read_and_binary_is_refused() {
        let a = prepare("notas.md", "# Título\nÁrbol, niño".as_bytes()).unwrap();
        assert_eq!(a.kind, Kind::Text);
        assert!(a.text.contains("Árbol"));
        let latin1 = prepare("viejo.txt", &[b'a', 0xF1, b'o']).unwrap();
        assert_eq!(latin1.text, "año");
        assert!(prepare("programa.bin", &[0u8, 1, 2, 3, 0, 0, 7, 8]).is_err());
        assert!(prepare("vacío.txt", b"").is_err());
        let mut long = "x".repeat(MAX_TEXT + 10);
        long.push('ñ');
        let a = prepare("largo.txt", long.as_bytes()).unwrap();
        assert!(a.text.len() < MAX_TEXT + 100 && a.detail.contains("recortado"));
        assert!(prepare("foto.heic", b"\0\0\0\x18ftypheic\0\0\0\0").unwrap_err().contains("HEIC"));
    }

    #[test]
    fn office_documents_become_text() {
        let docx = package(&[(
            "word/document.xml",
            r#"<w:document xmlns:w="w"><w:body><w:p><w:r><w:t>Hola &amp; </w:t></w:r><w:r><w:t xml:space="preserve">mundo</w:t></w:r></w:p><w:p><w:r><w:delText>borrado</w:delText><w:t>Segunda</w:t><w:tab/><w:t>línea</w:t></w:r></w:p></w:body></w:document>"#,
        )]);
        let a = prepare("informe.docx", &docx).unwrap();
        assert_eq!(a.text.trim(), "Hola & mundo\nSegunda\tlínea");
        assert!(a.detail.starts_with("Documento de Word"));

        let pptx = package(&[
            ("ppt/presentation.xml", "<p:presentation/>"),
            ("ppt/slides/slide10.xml", "<p:sld><a:p><a:r><a:t>Diez</a:t></a:r></a:p></p:sld>"),
            ("ppt/slides/slide2.xml", "<p:sld><a:p><a:r><a:t>Dos</a:t></a:r></a:p></p:sld>"),
        ]);
        let text = prepare("clase.pptx", &pptx).unwrap().text;
        assert!(text.find("Dos").unwrap() < text.find("Diez").unwrap(), "{text}");
        assert!(text.contains("## Diapositiva 2"));

        let xlsx = package(&[
            ("xl/workbook.xml", r#"<workbook><sheets><sheet name="Ventas" sheetId="1" r:id="rId1"/></sheets></workbook>"#),
            ("xl/_rels/workbook.xml.rels", r#"<Relationships><Relationship Id="rId1" Target="worksheets/sheet1.xml"/></Relationships>"#),
            ("xl/sharedStrings.xml", "<sst><si><t>Total</t></si><si><r><t>Re</t></r><r><t>gión</t></r></si></sst>"),
            (
                "xl/worksheets/sheet1.xml",
                r#"<worksheet><sheetData><row r="1"><c r="A1" t="s"><v>1</v></c><c r="B1"><v>12.5</v></c></row><row r="2"><c r="A2" t="s"><v>0</v></c><c r="B2"><f>SUM(B1:B1)</f><v>12.5</v></c><c r="C2" t="inlineStr"><is><t>ok</t></is></c></row></sheetData></worksheet>"#,
            ),
        ]);
        let text = prepare("ventas.xlsx", &xlsx).unwrap().text;
        assert!(text.contains("## Hoja «Ventas»"), "{text}");
        assert!(text.contains("A1: Región | B1: 12.5"), "{text}");
        assert!(text.contains("A2: Total | B2: 12.5 (=SUM(B1:B1)) | C2: ok"), "{text}");

        let odt = package(&[
            ("mimetype", "application/vnd.oasis.opendocument.text"),
            (
                "content.xml",
                "<office:document-content><text:h>Tema</text:h><text:p>Uno<text:s/>dos<text:span> tres</text:span></text:p></office:document-content>",
            ),
        ]);
        assert_eq!(prepare("a.odt", &odt).unwrap().text.trim(), "Tema\nUno dos tres");
        assert!(prepare("fotos.zip", &package(&[("a.jpg", "x")])).is_err());
        assert!(prepare("roto.docx", b"PK\x03\x04garbage").is_err());
    }
}
