//! Translate Android's InputConnection, hardware keys and pointers to the editor's input events.
//! Winit 0.30 does not forward GameActivity text-state events, so Java supplies this queue.
//!
//! Pointers are recognised here rather than handed to egui as raw touches: egui reads two
//! touches as a pinch zoom (a palm or a touchpad's two fingers zoomed the page by surprise) and a
//! finger drag as text selection. A finger pans and flicks the view, a tap clicks, a long press
//! opens the context menu and a long press then drag selects; two fingers pinch-zoom and pan.
//! Mouse, stylus and touchpad stay precise pointers with hover, buttons and wheel.
use egui::{Event, ImeEvent, Key, Modifiers, MouseWheelUnit, PointerButton, Pos2, RawInput, TouchPhase, Vec2, pos2, vec2};
use serde_json::Value;

/// Movement (points) before a finger press becomes a pan or a drag.
const SLOP: f32 = 10.0;
/// Seconds a still finger must rest before the press is a long press.
const LONG_PRESS: f64 = 0.45;
/// Flick velocity (points/s) below which a lifted pan does not coast.
const MIN_FLING: f32 = 120.0;
const STOP_FLING: f32 = 15.0;
const MAX_FLING: f32 = 7000.0;
/// Exponential decay of a coast (1/s).
const FLING_DECAY: f32 = 3.2;

// android.view.MotionEvent constants.
const DOWN: u64 = 0;
const UP: u64 = 1;
const MOVE: u64 = 2;
const CANCEL: u64 = 3;
const POINTER_DOWN: u64 = 5;
const POINTER_UP: u64 = 6;
const HOVER_MOVE: u64 = 7;
const SCROLL: u64 = 8;
const HOVER_ENTER: u64 = 9;
const BUTTON_PRESS: u64 = 11;
const BUTTON_RELEASE: u64 = 12;
const TOOL_FINGER: u64 = 1;
const TOOL_UNKNOWN: u64 = 0;
const SOURCE_TOUCHSCREEN: u64 = 0x1002;
const CLASS_TWO_FINGER_SWIPE: u64 = 3;
const CLASS_MULTI_FINGER_SWIPE: u64 = 4;
const CLASS_PINCH: u64 = 5;

#[derive(Clone, Copy, Debug, PartialEq)]
enum Finger {
    Idle,
    /// Down, not yet a tap, pan or long press.
    Pending {
        id: i64,
        start: Pos2,
        last: Pos2,
        since: f64,
    },
    Pan {
        id: i64,
        last: Pos2,
    },
    /// A long press that has not moved: lifting opens the context menu, moving drags.
    Armed {
        id: i64,
        start: Pos2,
        last: Pos2,
    },
    Drag {
        id: i64,
        last: Pos2,
    },
    Pinch {
        a: i64,
        b: i64,
        distance: f32,
        center: Pos2,
    },
    /// Ignore the rest of this gesture until every finger lifts.
    Done,
}

/// A touchpad gesture Android reports through a fake finger (Android 14+).
#[derive(Clone, Copy, Debug, PartialEq)]
enum Pad {
    Idle,
    Swipe { last: Pos2 },
    Pinch,
}

pub struct AndroidInput {
    composing: bool,
    modifiers: Modifiers,
    finger: Finger,
    pad: Pad,
    /// Mouse/stylus buttons pressed through this queue.
    pressed: Vec<PointerButton>,
    /// Events held back for the next frame (a tap's release must not land in its press's frame).
    later: Vec<Event>,
    /// Recent pan samples: (seconds, delta in points).
    samples: Vec<(f64, Vec2)>,
    fling: Vec2,
    fling_gone: bool,
    last_tick: f64,
    /// Coast after a flick; off for editors that already add their own momentum to wheel phases.
    pub synth_fling: bool,
    /// Android Back (button, gesture or mouse) was pressed since the host last looked.
    pub back: bool,
}

impl Default for AndroidInput {
    fn default() -> Self {
        Self {
            composing: false,
            modifiers: Modifiers::NONE,
            finger: Finger::Idle,
            pad: Pad::Idle,
            pressed: Vec::new(),
            later: Vec::new(),
            samples: Vec::new(),
            fling: Vec2::ZERO,
            fling_gone: true,
            last_tick: 0.0,
            synth_fling: true,
            back: false,
        }
    }
}

fn wheel(delta: Vec2, phase: TouchPhase, modifiers: Modifiers) -> Event {
    Event::MouseWheel { unit: MouseWheelUnit::Point, delta, phase, modifiers }
}

fn pointers(event: &Value, ppp: f32) -> Vec<(i64, Pos2)> {
    event["p"]
        .as_array()
        .into_iter()
        .flatten()
        .take(10)
        .filter_map(|p| {
            let id = p.get(0)?.as_i64()?;
            let x = p.get(1)?.as_f64()? as f32 / ppp;
            let y = p.get(2)?.as_f64()? as f32 / ppp;
            (x.is_finite() && y.is_finite()).then(|| (id, pos2(x, y)))
        })
        .collect()
}

fn modifiers_from(meta: u64) -> Modifiers {
    Modifiers { shift: meta & 0xc1 != 0, alt: meta & 0x32 != 0, ctrl: meta & 0x7000 != 0, mac_cmd: meta & 0x70000 != 0, command: meta & 0x77000 != 0 }
}

impl AndroidInput {
    pub fn reset_composition(&mut self) {
        self.composing = false;
    }

    /// Whether a gesture, a held-back event or a coast still needs frames.
    pub fn active(&self) -> bool {
        !self.later.is_empty() || self.fling != Vec2::ZERO || matches!(self.finger, Finger::Pending { .. })
    }

    /// Per-frame work without new input: deliver held-back events, promote a resting press to a
    /// long press and coast after a flick. `now` is seconds on any monotonic clock.
    pub fn tick(&mut self, raw: &mut RawInput, now: f64) {
        raw.events.append(&mut self.later);
        if let Finger::Pending { id, start, last, since } = self.finger
            && now - since >= LONG_PRESS
        {
            self.finger = Finger::Armed { id, start, last };
        }
        let dt = (now - self.last_tick).clamp(0.0, 0.1) as f32;
        self.last_tick = now;
        if self.fling != Vec2::ZERO {
            let keep = (-FLING_DECAY * dt).exp();
            let step = self.fling * ((1.0 - keep) / FLING_DECAY);
            self.fling *= keep;
            if self.fling.length() < STOP_FLING {
                self.fling = Vec2::ZERO;
                raw.events.push(wheel(Vec2::ZERO, TouchPhase::End, Modifiers::NONE));
                if self.fling_gone {
                    raw.events.push(Event::PointerGone);
                }
            } else {
                raw.events.push(wheel(step, TouchPhase::Move, Modifiers::NONE));
            }
        }
    }

    fn stop_fling(&mut self, raw: &mut RawInput) {
        if self.fling != Vec2::ZERO {
            self.fling = Vec2::ZERO;
            raw.events.push(wheel(Vec2::ZERO, TouchPhase::End, Modifiers::NONE));
        }
    }

    fn sample(&mut self, now: f64, delta: Vec2) {
        self.samples.retain(|(t, _)| now - t < 0.1);
        if self.samples.len() < 64 {
            self.samples.push((now, delta));
        }
    }

    /// End a pan: the editor learns the fingers lifted, and coasts if it was a flick.
    fn end_pan(&mut self, raw: &mut RawInput, now: f64, gone: bool) {
        self.samples.retain(|(t, _)| now - t < 0.1);
        let span = self.samples.first().map_or(0.0, |(t, _)| (now - t) as f32).max(1.0 / 120.0);
        let total: Vec2 = self.samples.iter().map(|(_, d)| *d).fold(Vec2::ZERO, |a, b| a + b);
        self.samples.clear();
        let velocity = total / span;
        if self.synth_fling && velocity.is_finite() && velocity.length() >= MIN_FLING {
            self.fling = velocity * (MAX_FLING / velocity.length()).min(1.0);
            self.fling_gone = gone;
        } else {
            raw.events.push(wheel(Vec2::ZERO, TouchPhase::End, self.modifiers));
            if gone {
                raw.events.push(Event::PointerGone);
            }
        }
    }

    fn click(&mut self, raw: &mut RawInput, pos: Pos2, button: PointerButton) {
        raw.events.push(Event::PointerMoved(pos));
        raw.events.push(Event::PointerButton { pos, button, pressed: true, modifiers: self.modifiers });
        self.later.push(Event::PointerButton { pos, button, pressed: false, modifiers: self.modifiers });
        self.later.push(Event::PointerGone);
    }

    /// A finger on the touch screen.
    fn finger(&mut self, event: &Value, raw: &mut RawInput, ppp: f32, now: f64) {
        let action = event["action"].as_u64().unwrap_or(u64::MAX);
        let points = pointers(event, ppp);
        let at = |id: i64| points.iter().find(|(p, _)| *p == id).map(|(_, pos)| *pos);
        match (action, self.finger) {
            (DOWN, _) => {
                self.stop_fling(raw);
                if let Some(&(id, pos)) = points.first() {
                    raw.events.push(Event::PointerMoved(pos));
                    self.finger = Finger::Pending { id, start: pos, last: pos, since: now };
                }
            }
            (POINTER_DOWN, Finger::Pending { .. } | Finger::Pan { .. } | Finger::Armed { .. }) => {
                if matches!(self.finger, Finger::Pan { .. }) {
                    raw.events.push(wheel(Vec2::ZERO, TouchPhase::End, self.modifiers));
                }
                self.samples.clear();
                self.finger = match (points.first(), points.get(1)) {
                    (Some(&(a, pa)), Some(&(b, pb))) => {
                        let center = pa + (pb - pa) * 0.5;
                        raw.events.push(Event::PointerMoved(center));
                        Finger::Pinch { a, b, distance: pa.distance(pb), center }
                    }
                    _ => Finger::Done,
                };
            }
            (MOVE, Finger::Pending { id, start, since, .. }) => {
                if let Some(pos) = at(id) {
                    if pos.distance(start) > SLOP {
                        // Moved before the long press: scroll the content with the finger.
                        raw.events.push(Event::PointerMoved(pos));
                        raw.events.push(wheel(pos - start, TouchPhase::Start, self.modifiers));
                        self.sample(now, pos - start);
                        self.finger = Finger::Pan { id, last: pos };
                    } else if now - since >= LONG_PRESS {
                        self.finger = Finger::Armed { id, start, last: pos };
                    } else {
                        self.finger = Finger::Pending { id, start, last: pos, since };
                    }
                }
            }
            (MOVE, Finger::Pan { id, last }) => {
                if let Some(pos) = at(id)
                    && pos != last
                {
                    raw.events.push(Event::PointerMoved(pos));
                    raw.events.push(wheel(pos - last, TouchPhase::Move, self.modifiers));
                    self.sample(now, pos - last);
                    self.finger = Finger::Pan { id, last: pos };
                }
            }
            (MOVE, Finger::Armed { id, start, .. }) => {
                if let Some(pos) = at(id) {
                    if pos.distance(start) > SLOP {
                        // Long press then drag: select, move a handle, drag an object.
                        raw.events.push(Event::PointerMoved(start));
                        raw.events.push(Event::PointerButton {
                            pos: start,
                            button: PointerButton::Primary,
                            pressed: true,
                            modifiers: self.modifiers,
                        });
                        raw.events.push(Event::PointerMoved(pos));
                        self.finger = Finger::Drag { id, last: pos };
                    } else {
                        self.finger = Finger::Armed { id, start, last: pos };
                    }
                }
            }
            (MOVE, Finger::Drag { id, last }) => {
                if let Some(pos) = at(id)
                    && pos != last
                {
                    raw.events.push(Event::PointerMoved(pos));
                    self.finger = Finger::Drag { id, last: pos };
                }
            }
            (MOVE, Finger::Pinch { a, b, distance, center }) => {
                if let (Some(pa), Some(pb)) = (at(a), at(b)) {
                    let d = pa.distance(pb);
                    let c = pa + (pb - pa) * 0.5;
                    raw.events.push(Event::PointerMoved(c));
                    if distance > 1.0 && d > 1.0 {
                        let zoom = d / distance;
                        if (zoom - 1.0).abs() > 1e-3 && zoom.is_finite() {
                            raw.events.push(Event::Zoom(zoom.clamp(0.5, 2.0)));
                        }
                    }
                    if c != center {
                        raw.events.push(wheel(c - center, TouchPhase::Move, self.modifiers));
                    }
                    self.finger = Finger::Pinch { a, b, distance: d, center: c };
                }
            }
            (POINTER_UP, Finger::Pinch { .. }) => {
                raw.events.push(wheel(Vec2::ZERO, TouchPhase::End, self.modifiers));
                self.finger = Finger::Done;
            }
            (UP, Finger::Pending { start, .. }) => {
                self.click(raw, start, PointerButton::Primary);
                self.finger = Finger::Idle;
            }
            (UP, Finger::Armed { start, .. }) => {
                self.click(raw, start, PointerButton::Secondary);
                self.finger = Finger::Idle;
            }
            (UP, Finger::Pan { .. }) => {
                self.end_pan(raw, now, true);
                self.finger = Finger::Idle;
            }
            (UP | CANCEL, Finger::Drag { last, .. }) => {
                raw.events.push(Event::PointerButton { pos: last, button: PointerButton::Primary, pressed: false, modifiers: self.modifiers });
                self.later.push(Event::PointerGone);
                self.finger = Finger::Idle;
            }
            (CANCEL, Finger::Pan { .. } | Finger::Pinch { .. }) => {
                raw.events.push(wheel(Vec2::ZERO, TouchPhase::Cancel, self.modifiers));
                raw.events.push(Event::PointerGone);
                self.finger = Finger::Idle;
            }
            (UP | CANCEL, _) => {
                if self.finger != Finger::Idle {
                    raw.events.push(Event::PointerGone);
                }
                self.finger = Finger::Idle;
            }
            _ => {}
        }
    }

    /// A touchpad gesture (two-finger swipe or pinch) reported through a fake finger.
    fn touchpad(&mut self, event: &Value, raw: &mut RawInput, ppp: f32, now: f64) {
        let action = event["action"].as_u64().unwrap_or(u64::MAX);
        let class = event["cls"].as_u64().unwrap_or(0);
        let points = pointers(event, ppp);
        let center = if points.is_empty() {
            None
        } else {
            let sum = points.iter().fold(Vec2::ZERO, |a, (_, p)| a + p.to_vec2());
            Some((sum / points.len() as f32).to_pos2())
        };
        match action {
            DOWN | POINTER_DOWN => {
                self.stop_fling(raw);
                self.pad = match (class, center) {
                    (CLASS_PINCH, _) => Pad::Pinch,
                    (_, Some(last)) => {
                        raw.events.push(wheel(Vec2::ZERO, TouchPhase::Start, self.modifiers));
                        Pad::Swipe { last }
                    }
                    _ => Pad::Idle,
                };
            }
            MOVE => match (self.pad, center) {
                (Pad::Swipe { last }, Some(pos)) => {
                    if pos != last {
                        raw.events.push(wheel(pos - last, TouchPhase::Move, self.modifiers));
                        self.sample(now, pos - last);
                    }
                    self.pad = Pad::Swipe { last: pos };
                }
                (Pad::Pinch, _) => {
                    let zoom = event["pinch"].as_f64().unwrap_or(1.0) as f32;
                    if zoom.is_finite() && zoom > 0.0 && (zoom - 1.0).abs() > 1e-4 {
                        raw.events.push(Event::Zoom(zoom.clamp(0.5, 2.0)));
                    }
                }
                _ => {}
            },
            UP | CANCEL | POINTER_UP => {
                if matches!(self.pad, Pad::Swipe { .. }) {
                    if action == CANCEL {
                        raw.events.push(wheel(Vec2::ZERO, TouchPhase::Cancel, self.modifiers));
                    } else {
                        // The cursor stays where it is: no PointerGone after a touchpad swipe.
                        self.end_pan(raw, now, false);
                    }
                }
                self.pad = Pad::Idle;
            }
            _ => {}
        }
    }

    fn press(&mut self, raw: &mut RawInput, pos: Pos2, button: PointerButton, pressed: bool) {
        if pressed == self.pressed.contains(&button) {
            return;
        }
        if pressed {
            self.pressed.push(button);
        } else {
            self.pressed.retain(|b| *b != button);
        }
        raw.events.push(Event::PointerButton { pos, button, pressed, modifiers: self.modifiers });
    }

    /// Mouse, stylus or a touchpad's cursor: a precise pointer.
    fn precise(&mut self, event: &Value, raw: &mut RawInput, ppp: f32) {
        let action = event["action"].as_u64().unwrap_or(u64::MAX);
        let Some(&(_, pos)) = pointers(event, ppp).first() else { return };
        let buttons = event["buttons"].as_u64().unwrap_or(0);
        let modifiers = self.modifiers;
        match action {
            HOVER_ENTER | HOVER_MOVE | MOVE => raw.events.push(Event::PointerMoved(pos)),
            DOWN => {
                self.stop_fling(raw);
                raw.events.push(Event::PointerMoved(pos));
                // A stylus side button (0x20) or a mouse's right button alone: secondary.
                let secondary = buttons & 0x22 != 0 && buttons & 0x1 == 0;
                self.press(raw, pos, if secondary { PointerButton::Secondary } else { PointerButton::Primary }, true);
            }
            UP | CANCEL => {
                raw.events.push(Event::PointerMoved(pos));
                for button in std::mem::take(&mut self.pressed) {
                    raw.events.push(Event::PointerButton { pos, button, pressed: false, modifiers });
                }
            }
            BUTTON_PRESS | BUTTON_RELEASE => {
                let button = match event["action_button"].as_u64().unwrap_or(0) {
                    2 => Some(PointerButton::Secondary),
                    4 => Some(PointerButton::Middle),
                    8 => Some(PointerButton::Extra1),
                    16 => Some(PointerButton::Extra2),
                    _ => None,
                };
                if let Some(button) = button {
                    self.press(raw, pos, button, action == BUTTON_PRESS);
                }
            }
            SCROLL => {
                let v = event["vs"].as_f64().unwrap_or(0.0) as f32;
                let h = event["hs"].as_f64().unwrap_or(0.0) as f32;
                if v.is_finite() && h.is_finite() && (v != 0.0 || h != 0.0) {
                    raw.events.push(Event::PointerMoved(pos));
                    // Android: +v is the wheel turned away (content down), +h scrolls right.
                    raw.events.push(Event::MouseWheel { unit: MouseWheelUnit::Line, delta: vec2(-h, v), phase: TouchPhase::Move, modifiers });
                }
            }
            _ => {}
        }
    }

    fn motion(&mut self, event: &Value, raw: &mut RawInput, ppp: f32, now: f64) {
        self.modifiers = modifiers_from(event["meta"].as_u64().unwrap_or(0));
        raw.events.push(Event::ModifiersChanged(self.modifiers));
        let class = event["cls"].as_u64().unwrap_or(0);
        let source = event["source"].as_u64().unwrap_or(0);
        let tool = event["tool"].as_u64().unwrap_or(TOOL_UNKNOWN);
        if class == CLASS_MULTI_FINGER_SWIPE {
            return; // three or more fingers: a system gesture
        }
        if class == CLASS_TWO_FINGER_SWIPE || class == CLASS_PINCH || self.pad != Pad::Idle {
            self.touchpad(event, raw, ppp, now);
        } else if source & SOURCE_TOUCHSCREEN == SOURCE_TOUCHSCREEN && matches!(tool, TOOL_FINGER | TOOL_UNKNOWN) {
            self.finger(event, raw, ppp, now);
        } else {
            self.precise(event, raw, ppp);
        }
    }

    /// Append this frame's Android events. `ppp` converts Android pixels to egui points.
    pub fn append(&mut self, source: &Value, raw: &mut RawInput, ppp: f32, now: f64) {
        // GameActivity still wakes winit for native keys. They are represented once by this
        // queue, with Android's Unicode mapping and modifier state, rather than twice. Pointers
        // never reach winit (Java consumes them), but drop any that do: they come from here.
        raw.events.retain(|e| {
            !matches!(
                e,
                Event::Key { .. }
                    | Event::Text(_)
                    | Event::Ime(_)
                    | Event::Copy
                    | Event::Cut
                    | Event::Paste(_)
                    | Event::PointerMoved(_)
                    | Event::PointerButton { .. }
                    | Event::PointerGone
                    | Event::Touch { .. }
                    | Event::MouseWheel { .. }
                    | Event::Zoom(_)
                    | Event::Rotate(_)
            )
        });
        let ppp = if ppp.is_finite() && ppp > 0.1 { ppp } else { 1.0 };
        self.tick(raw, now);
        for event in source.as_array().into_iter().flatten().take(1024) {
            match event["type"].as_str().unwrap_or("") {
                "motion" => self.motion(event, raw, ppp, now),
                "back" => self.back = true,
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
                    self.modifiers = modifiers_from(event["meta"].as_u64().unwrap_or(0));
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
            1.0,
            0.0,
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
            1.0,
            0.0,
        );
        assert!(matches!(raw.events.first(), Some(Event::Ime(ImeEvent::Preedit { .. }))));
        assert_eq!(raw.events.iter().filter(|e| matches!(e, Event::Ime(ImeEvent::Commit(t)) if t == "español")).count(), 1);
        assert!(raw.events.iter().any(|e| matches!(e, Event::Ime(ImeEvent::DeleteSurrounding { before_chars: 1, after_chars: 0 }))));
        assert!(raw.events.iter().any(|e| matches!(e, Event::Key { key: Key::Enter, pressed: true, .. })));
        assert!(raw.events.iter().any(|e| matches!(e, Event::Text(t) if t == "Hola 🙂")));
    }

    fn touch(action: u64, points: &[(i64, f32, f32)]) -> serde_json::Value {
        json!({"type":"motion","action":action,"source":0x1002,"tool":1,"cls":0,"meta":0,
            "p": points.iter().map(|(id, x, y)| json!([id, x, y, 1])).collect::<Vec<_>>()})
    }
    fn wheel_y(raw: &RawInput) -> f32 {
        raw.events.iter().map(|e| if let Event::MouseWheel { delta, .. } = e { delta.y } else { 0.0 }).sum()
    }
    fn has(raw: &RawInput, f: impl Fn(&Event) -> bool) -> bool {
        raw.events.iter().any(f)
    }

    #[test]
    fn finger_drag_scrolls_instead_of_selecting_and_never_zooms() {
        let mut input = AndroidInput::default();
        let mut raw = RawInput::default();
        input.append(&json!([touch(0, &[(1, 100.0, 400.0)])]), &mut raw, 2.0, 0.0);
        let mut raw = RawInput::default();
        input.append(&json!([touch(2, &[(1, 100.0, 300.0)])]), &mut raw, 2.0, 0.05);
        assert_eq!(wheel_y(&raw), -50.0, "finger up 100 px = content up 50 points");
        assert!(!has(&raw, |e| matches!(e, Event::PointerButton { .. } | Event::Zoom(_) | Event::Touch { .. })));
        let mut raw = RawInput::default();
        input.append(&json!([touch(1, &[(1, 100.0, 300.0)])]), &mut raw, 2.0, 0.6);
        assert!(has(&raw, |e| matches!(e, Event::MouseWheel { phase: TouchPhase::End, .. })));
    }

    #[test]
    fn tap_clicks_with_release_on_the_next_frame() {
        let mut input = AndroidInput::default();
        let mut raw = RawInput::default();
        input.append(&json!([touch(0, &[(4, 20.0, 20.0)]), touch(1, &[(4, 21.0, 20.0)])]), &mut raw, 2.0, 0.0);
        assert!(has(
            &raw,
            |e| matches!(e, Event::PointerButton { pressed: true, button: PointerButton::Primary, pos, .. } if *pos == pos2(10.0, 10.0))
        ));
        assert!(!has(&raw, |e| matches!(e, Event::PointerButton { pressed: false, .. })));
        let mut raw = RawInput::default();
        input.append(&json!([]), &mut raw, 2.0, 0.02);
        assert!(has(&raw, |e| matches!(e, Event::PointerButton { pressed: false, .. })));
    }

    #[test]
    fn long_press_opens_context_menu_and_long_press_drag_selects() {
        let mut input = AndroidInput::default();
        let mut raw = RawInput::default();
        input.append(&json!([touch(0, &[(1, 50.0, 50.0)])]), &mut raw, 1.0, 0.0);
        let mut raw = RawInput::default();
        input.append(&json!([touch(1, &[(1, 50.0, 50.0)])]), &mut raw, 1.0, 0.6);
        assert!(has(&raw, |e| matches!(e, Event::PointerButton { pressed: true, button: PointerButton::Secondary, .. })));

        let mut input = AndroidInput::default();
        let mut raw = RawInput::default();
        input.append(&json!([touch(0, &[(1, 50.0, 50.0)])]), &mut raw, 1.0, 0.0);
        input.append(&json!([touch(2, &[(1, 52.0, 50.0)])]), &mut raw, 1.0, 0.5);
        let mut raw = RawInput::default();
        input.append(&json!([touch(2, &[(1, 150.0, 50.0)])]), &mut raw, 1.0, 0.7);
        assert!(has(&raw, |e| matches!(e, Event::PointerButton { pressed: true, button: PointerButton::Primary, .. })));
        assert_eq!(wheel_y(&raw), 0.0);
    }

    #[test]
    fn two_fingers_pinch_zoom_but_a_resting_palm_does_not() {
        let mut input = AndroidInput::default();
        let mut raw = RawInput::default();
        input.append(
            &json!([
                touch(0, &[(1, 100.0, 100.0)]),
                touch(5, &[(1, 100.0, 100.0), (2, 200.0, 100.0)]),
                touch(2, &[(1, 50.0, 100.0), (2, 250.0, 100.0)])
            ]),
            &mut raw,
            1.0,
            0.0,
        );
        let zoom: f32 = raw.events.iter().map(|e| if let Event::Zoom(z) = e { *z } else { 1.0 }).product();
        assert!((zoom - 2.0).abs() < 1e-3);
        let mut raw = RawInput::default();
        input.append(&json!([touch(2, &[(1, 50.0, 100.0), (2, 250.0, 100.0)])]), &mut raw, 1.0, 0.1);
        assert!(!has(&raw, |e| matches!(e, Event::Zoom(_))));
    }

    #[test]
    fn mouse_wheel_buttons_and_touchpad_swipe() {
        let mut input = AndroidInput::default();
        let mut raw = RawInput::default();
        let mouse = |action: u64, buttons: u64, action_button: u64| json!({"type":"motion","action":action,"source":0x2002,"tool":3,"cls":0,"meta":0,"buttons":buttons,"action_button":action_button,"vs":-1.0,"hs":0.0,"p":[[0,40.0,40.0,3]]});
        input.append(&json!([mouse(7, 0, 0), mouse(0, 2, 0), mouse(11, 2, 2), mouse(12, 0, 2), mouse(1, 0, 0), mouse(8, 0, 0)]), &mut raw, 1.0, 0.0);
        let presses: Vec<_> = raw
            .events
            .iter()
            .filter_map(|e| if let Event::PointerButton { button, pressed, .. } = e { Some((*button, *pressed)) } else { None })
            .collect();
        assert_eq!(presses, [(PointerButton::Secondary, true), (PointerButton::Secondary, false)]);
        assert!(has(&raw, |e| matches!(e, Event::MouseWheel { unit: MouseWheelUnit::Line, delta, .. } if delta.y == -1.0)));

        let pad = |action: u64, y: f32| json!({"type":"motion","action":action,"source":0x2002,"tool":1,"cls":3,"meta":0,"p":[[0,300.0,y,1]]});
        let mut raw = RawInput::default();
        input.append(&json!([pad(0, 300.0), pad(2, 260.0)]), &mut raw, 1.0, 1.0);
        assert_eq!(wheel_y(&raw), -40.0);
        assert!(!has(&raw, |e| matches!(e, Event::PointerButton { .. } | Event::Zoom(_))));
    }

    #[test]
    fn flick_coasts_when_the_editor_has_no_momentum_of_its_own() {
        let mut input = AndroidInput::default();
        let mut raw = RawInput::default();
        input.append(&json!([touch(0, &[(1, 100.0, 500.0)])]), &mut raw, 1.0, 0.0);
        input.append(&json!([touch(2, &[(1, 100.0, 400.0)])]), &mut raw, 1.0, 0.02);
        input.append(&json!([touch(2, &[(1, 100.0, 300.0)])]), &mut raw, 1.0, 0.04);
        input.append(&json!([touch(1, &[(1, 100.0, 300.0)])]), &mut raw, 1.0, 0.05);
        assert!(input.active());
        let mut raw = RawInput::default();
        input.append(&json!([]), &mut raw, 1.0, 0.07);
        assert!(wheel_y(&raw) < 0.0);
        input.synth_fling = false;
        assert!(input.active());
    }
}
