//! Translate Android's InputConnection and hardware keys to the editor's input events.
//! Winit 0.30 does not forward GameActivity text-state events, so Java supplies this queue.
use egui::{Event, ImeEvent, Key, Modifiers, RawInput};
use serde_json::Value;

#[derive(Default)]
pub struct AndroidInput {
    composing: bool,
    modifiers: Modifiers,
}

impl AndroidInput {
    pub fn reset_composition(&mut self) {
        self.composing = false;
    }

    pub fn append(&mut self, source: &Value, raw: &mut RawInput) {
        // GameActivity still wakes winit for native keys. They are represented once by this
        // queue, with Android's Unicode mapping and modifier state, rather than twice.
        raw.events.retain(|e| !matches!(e, Event::Key { .. } | Event::Text(_) | Event::Ime(_) | Event::Copy | Event::Cut | Event::Paste(_)));
        for event in source.as_array().into_iter().flatten().take(1024) {
            match event["type"].as_str().unwrap_or("") {
                "preedit" => {
                    self.composing = true;
                    raw.events.push(Event::Ime(ImeEvent::Preedit { text: text(event), active_range_chars: None }));
                }
                "commit" => {
                    let text = text(event);
                    if self.composing {
                        raw.events.push(Event::Ime(ImeEvent::Commit(text)));
                        self.composing = false;
                    } else {
                        append_text(&mut raw.events, &text);
                    }
                }
                "delete" => raw.events.push(Event::Ime(ImeEvent::DeleteSurrounding {
                    before_chars: event["before"].as_u64().unwrap_or(0).min(8192) as usize,
                    after_chars: event["after"].as_u64().unwrap_or(0).min(8192) as usize,
                })),
                "cancel" => {
                    raw.events.push(Event::Ime(ImeEvent::Preedit { text: String::new(), active_range_chars: None }));
                    self.composing = false;
                }
                "key" => {
                    let meta = event["meta"].as_u64().unwrap_or(0);
                    self.modifiers = Modifiers {
                        shift: meta & 0xc1 != 0,
                        alt: meta & 0x32 != 0,
                        ctrl: meta & 0x7000 != 0,
                        mac_cmd: meta & 0x70000 != 0,
                        command: meta & 0x77000 != 0,
                    };
                    let down = event["down"].as_bool().unwrap_or(false);
                    let text = text(event);
                    // AltGr is text input, including @ and accented letters on external layouts.
                    if event["altgr"] == true {
                        self.modifiers.alt = false;
                        self.modifiers.ctrl = false;
                        self.modifiers.command = self.modifiers.mac_cmd;
                    }
                    raw.events.push(Event::ModifiersChanged(self.modifiers));
                    if let Some(key) = event["code"].as_u64().and_then(key) {
                        if down && self.modifiers.command && matches!(key, Key::C | Key::X) {
                            raw.events.push(if key == Key::C { Event::Copy } else { Event::Cut });
                        } else {
                            raw.events.push(Event::Key {
                                key,
                                physical_key: Some(key),
                                pressed: down,
                                repeat: event["repeat"].as_bool().unwrap_or(false),
                                modifiers: self.modifiers,
                            });
                        }
                    }
                    if down && !self.modifiers.command && !text.is_empty() && text.chars().all(|c| !c.is_control()) {
                        raw.events.push(Event::Text(text));
                    }
                }
                _ => {}
            }
        }
    }
}

fn text(event: &Value) -> String {
    event["text"].as_str().unwrap_or("").chars().take(8192).collect()
}

fn append_text(events: &mut Vec<Event>, text: &str) {
    let mut chunk = String::new();
    for ch in text.chars() {
        let key = match ch {
            '\n' => Some(Key::Enter),
            '\t' => Some(Key::Tab),
            '\r' => continue,
            _ => None,
        };
        if let Some(key) = key {
            if !chunk.is_empty() {
                events.push(Event::Text(std::mem::take(&mut chunk)));
            }
            for pressed in [true, false] {
                events.push(Event::Key { key, physical_key: None, pressed, repeat: false, modifiers: Modifiers::NONE });
            }
        } else if !ch.is_control() {
            chunk.push(ch);
        }
    }
    if !chunk.is_empty() {
        events.push(Event::Text(chunk));
    }
}

fn key(code: u64) -> Option<Key> {
    match code {
        7..=16 => Key::from_name(&(code - 7).to_string()),
        29..=54 => char::from_u32(u32::try_from(code - 29 + u64::from(b'A')).ok()?).and_then(|c| Key::from_name(&c.to_string())),
        144..=153 => Key::from_name(&(code - 144).to_string()),
        55 | 159 => Some(Key::Comma),
        56 | 158 => Some(Key::Period),
        68 => Some(Key::Backtick),
        69 | 156 => Some(Key::Minus),
        70 | 161 => Some(Key::Equals),
        71 => Some(Key::OpenBracket),
        72 => Some(Key::CloseBracket),
        73 => Some(Key::Backslash),
        74 => Some(Key::Semicolon),
        75 => Some(Key::Quote),
        76 | 154 => Some(Key::Slash),
        81 | 157 => Some(Key::Plus),
        131..=142 => Key::from_name(&format!("F{}", code - 130)),
        19 => Some(Key::ArrowUp),
        20 => Some(Key::ArrowDown),
        21 => Some(Key::ArrowLeft),
        22 => Some(Key::ArrowRight),
        61 => Some(Key::Tab),
        62 => Some(Key::Space),
        66 | 160 => Some(Key::Enter),
        67 => Some(Key::Backspace),
        92 => Some(Key::PageUp),
        93 => Some(Key::PageDown),
        111 => Some(Key::Escape),
        112 => Some(Key::Delete),
        122 => Some(Key::Home),
        123 => Some(Key::End),
        124 => Some(Key::Insert),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn hardware_spanish_altgr_and_shortcuts_arrive_once() {
        let mut bridge = AndroidInput::default();
        let mut raw = RawInput { events: vec![Event::Text("duplicate".into())], ..Default::default() };
        bridge.append(
            &json!([
                {"type":"key","code":42,"down":true,"text":"ñ","meta":0},
                {"type":"key","code":45,"down":true,"text":"@","meta":4098,"altgr":true},
                {"type":"key","code":31,"down":true,"text":"c","meta":4096}
            ]),
            &mut raw,
        );
        let texts: Vec<_> = raw.events.iter().filter_map(|e| if let Event::Text(t) = e { Some(t.as_str()) } else { None }).collect();
        assert_eq!(texts, ["ñ", "@"]);
        assert_eq!(raw.events.iter().filter(|e| matches!(e, Event::Copy)).count(), 1);
    }

    #[test]
    fn software_composition_commit_delete_and_enter_are_ordered() {
        let mut bridge = AndroidInput::default();
        let mut raw = RawInput::default();
        bridge.append(
            &json!([
                {"type":"preedit","text":"espa"}, {"type":"preedit","text":"español"},
                {"type":"commit","text":"español"}, {"type":"delete","before":1,"after":0},
                {"type":"commit","text":"\nHola 🙂"}
            ]),
            &mut raw,
        );
        assert!(matches!(raw.events.first(), Some(Event::Ime(ImeEvent::Preedit { .. }))));
        assert_eq!(raw.events.iter().filter(|e| matches!(e, Event::Ime(ImeEvent::Commit(t)) if t == "español")).count(), 1);
        assert!(raw.events.iter().any(|e| matches!(e, Event::Ime(ImeEvent::DeleteSurrounding { before_chars: 1, after_chars: 0 }))));
        assert!(raw.events.iter().any(|e| matches!(e, Event::Key { key: Key::Enter, pressed: true, .. })));
        assert!(raw.events.iter().any(|e| matches!(e, Event::Text(t) if t == "Hola 🙂")));
    }
}
