//! Input: keyboard, mouse and gamepad state with per-frame edge tracking and
//! string-named action bindings (defined in `project.ron`).
//!
//! The editor decides *when* to feed events in (viewport focus); game mode
//! forwards everything.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use winit::event::{ElementState, MouseButton, MouseScrollDelta};
use winit::keyboard::KeyCode;

use crate::math::Vec2;

/// One physical input, referenced by name so projects stay data-only.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Binding {
    /// Key by winit name, e.g. `"Space"`, `"ArrowLeft"`, `"KeyW"`.
    Key(String),
    Mouse(String),
    /// Gamepad button by name: `"South"` (A/Cross), `"East"`, `"North"`,
    /// `"West"`, `"Start"`, `"Select"`, `"Mode"`, `"LeftThumb"`, `"RightThumb"`,
    /// `"DPadUp"`/`"DPadDown"`/`"DPadLeft"`/`"DPadRight"`, `"LeftTrigger"`,
    /// `"RightTrigger"`, `"LeftTrigger2"`, `"RightTrigger2"`.
    Pad(String),
    /// Left stick pushed beyond `0.5` in a direction: `"Left"`, `"Right"`,
    /// `"Up"`, `"Down"`. Same for `"RightStickLeft"` etc.
    Stick(String),
}

/// Full input state for one frame window.
pub struct Input {
    keys_held: HashMap<KeyCode, bool>,
    keys_pressed: HashMap<KeyCode, bool>,
    keys_released: HashMap<KeyCode, bool>,

    pub mouse_pos: Vec2,
    pub mouse_delta: Vec2,
    pub wheel: f32,
    mouse_held: HashMap<MouseButton, bool>,
    mouse_pressed: HashMap<MouseButton, bool>,
    mouse_released: HashMap<MouseButton, bool>,

    pad_held: HashMap<String, bool>,
    pad_pressed: HashMap<String, bool>,
    pub pad_left: Vec2,
    pub pad_right: Vec2,
    pub pad_trigger: Vec2,

    actions: HashMap<String, Vec<Binding>>,
    gilrs: Option<gilrs::Gilrs>,
    /// Gamepad is polled lazily; set false when no device is present.
    pad_available: bool,
}

impl Default for Input {
    fn default() -> Self {
        Self::new()
    }
}

impl Input {
    pub fn new() -> Self {
        Self {
            keys_held: HashMap::new(),
            keys_pressed: HashMap::new(),
            keys_released: HashMap::new(),
            mouse_pos: Vec2::ZERO,
            mouse_delta: Vec2::ZERO,
            wheel: 0.0,
            mouse_held: HashMap::new(),
            mouse_pressed: HashMap::new(),
            mouse_released: HashMap::new(),
            pad_held: HashMap::new(),
            pad_pressed: HashMap::new(),
            pad_left: Vec2::ZERO,
            pad_right: Vec2::ZERO,
            pad_trigger: Vec2::ZERO,
            actions: HashMap::new(),
            gilrs: gilrs::Gilrs::new().ok(),
            pad_available: false,
        }
    }

    /// Install named action bindings (from `project.ron`).
    pub fn set_actions(&mut self, actions: HashMap<String, Vec<Binding>>) {
        self.actions = actions;
    }

    /// Clear all per-frame edges (pressed/released sets, deltas). Call after
    /// every consumer has read this frame's state (i.e. at frame end).
    pub fn end_frame(&mut self) {
        self.keys_pressed.clear();
        self.keys_released.clear();
        self.mouse_delta = Vec2::ZERO;
        self.wheel = 0.0;
        self.mouse_pressed.clear();
        self.mouse_released.clear();
        self.pad_pressed.clear();
        self.poll_gamepad();
    }

    /// Drop all held state (window lost focus) so keys don't stick.
    pub fn blur(&mut self) {
        self.keys_held.clear();
        self.mouse_held.clear();
        self.pad_held.clear();
        self.pad_left = Vec2::ZERO;
        self.pad_right = Vec2::ZERO;
        self.pad_trigger = Vec2::ZERO;
        self.end_frame();
    }

    // -----------------------------------------------------------------------
    // Event feed (winit events, forwarded by the app or editor)
    // -----------------------------------------------------------------------

    pub fn on_key(&mut self, code: KeyCode, state: ElementState) {
        let down = state == ElementState::Pressed;
        if down {
            if !self.keys_held.get(&code).copied().unwrap_or(false) {
                self.keys_pressed.insert(code, true);
            }
            self.keys_held.insert(code, true);
        } else {
            self.keys_released.insert(code, true);
            self.keys_held.insert(code, false);
        }
    }

    pub fn on_mouse_move(&mut self, pos: Vec2) {
        self.mouse_delta += pos - self.mouse_pos;
        self.mouse_pos = pos;
    }

    pub fn on_mouse_button(&mut self, button: MouseButton, state: ElementState) {
        let down = state == ElementState::Pressed;
        if down {
            self.mouse_pressed.insert(button, true);
            self.mouse_held.insert(button, true);
        } else {
            self.mouse_released.insert(button, true);
            self.mouse_held.insert(button, false);
        }
    }

    pub fn on_wheel(&mut self, delta: MouseScrollDelta) {
        self.wheel += match delta {
            MouseScrollDelta::LineDelta(_, y) => y,
            MouseScrollDelta::PixelDelta(p) => p.y as f32 / 40.0,
        };
    }

    // -----------------------------------------------------------------------
    // Queries
    // -----------------------------------------------------------------------

    pub fn key_held(&self, name: &str) -> bool {
        key_code(name)
            .map(|k| self.keys_held.get(&k).copied().unwrap_or(false))
            .unwrap_or(false)
    }

    pub fn key_pressed(&self, name: &str) -> bool {
        key_code(name)
            .map(|k| self.keys_pressed.contains_key(&k))
            .unwrap_or(false)
    }

    pub fn key_released(&self, name: &str) -> bool {
        key_code(name)
            .map(|k| self.keys_released.contains_key(&k))
            .unwrap_or(false)
    }

    pub fn mouse_held(&self, button: MouseButton) -> bool {
        self.mouse_held.get(&button).copied().unwrap_or(false)
    }

    pub fn mouse_pressed(&self, button: MouseButton) -> bool {
        self.mouse_pressed.contains_key(&button)
    }

    pub fn mouse_released(&self, button: MouseButton) -> bool {
        self.mouse_released.contains_key(&button)
    }

    /// Resolve a key name to a `KeyCode` using winit's own naming.
    pub fn key_name_down(&self, name: &str) -> bool {
        self.key_held(name)
    }

    // -----------------------------------------------------------------------
    // Actions
    // -----------------------------------------------------------------------

    pub fn action_held(&self, name: &str) -> bool {
        self.actions
            .get(name)
            .is_some_and(|bs| bs.iter().any(|b| self.binding_held(b)))
    }

    pub fn action_pressed(&self, name: &str) -> bool {
        self.actions
            .get(name)
            .is_some_and(|bs| bs.iter().any(|b| self.binding_pressed(b)))
    }

    pub fn action_released(&self, name: &str) -> bool {
        self.actions
            .get(name)
            .is_some_and(|bs| bs.iter().any(|b| self.binding_released(b)))
    }

    fn binding_held(&self, b: &Binding) -> bool {
        match b {
            Binding::Key(k) => self.key_held(k),
            Binding::Mouse(m) => mouse_button(m).map(|b| self.mouse_held(b)).unwrap_or(false),
            Binding::Pad(p) => self.pad_held.get(p).copied().unwrap_or(false),
            Binding::Stick(s) => {
                self.stick_active(s, self.pad_left, 0.5)
                    || self.stick_active(s, self.pad_right, 0.5)
            }
        }
    }

    fn binding_pressed(&self, b: &Binding) -> bool {
        match b {
            Binding::Key(k) => self.key_pressed(k),
            Binding::Mouse(m) => mouse_button(m)
                .map(|b| self.mouse_pressed(b))
                .unwrap_or(false),
            Binding::Pad(p) => self.pad_pressed.contains_key(p),
            Binding::Stick(_) => false, // edges are not tracked for sticks
        }
    }

    fn binding_released(&self, b: &Binding) -> bool {
        match b {
            Binding::Key(k) => self.key_released(k),
            Binding::Mouse(m) => mouse_button(m)
                .map(|b| self.mouse_released(b))
                .unwrap_or(false),
            Binding::Stick(_) => false,
            _ => false,
        }
    }

    fn stick_active(&self, name: &str, stick: Vec2, threshold: f32) -> bool {
        match name
            .trim_start_matches("LeftStick")
            .trim_start_matches("RightStick")
        {
            "Left" => stick.x < -threshold,
            "Right" => stick.x > threshold,
            "Up" => stick.y > threshold,
            "Down" => stick.y < -threshold,
            _ => false,
        }
    }

    fn poll_gamepad(&mut self) {
        let Some(gilrs) = self.gilrs.as_mut() else {
            return;
        };
        while let Some(event) = gilrs.next_event() {
            use gilrs::ev::EventType as Et;
            match event.event {
                Et::ButtonPressed(b, _) | Et::ButtonRepeated(b, _) => {
                    if let Some(name) = pad_button_name(b) {
                        self.pad_pressed.insert(name.to_string(), true);
                        self.pad_held.insert(name.to_string(), true);
                    }
                }
                Et::ButtonReleased(b, _) => {
                    if let Some(name) = pad_button_name(b) {
                        self.pad_held.insert(name.to_string(), false);
                    }
                }
                Et::AxisChanged(axis, v, _) => match axis {
                    gilrs::Axis::LeftStickX => self.pad_left.x = v,
                    gilrs::Axis::LeftStickY => self.pad_left.y = v,
                    gilrs::Axis::RightStickX => self.pad_right.x = v,
                    gilrs::Axis::RightStickY => self.pad_right.y = v,
                    gilrs::Axis::LeftZ => self.pad_trigger.x = v,
                    gilrs::Axis::RightZ => self.pad_trigger.y = v,
                    _ => {}
                },
                Et::Connected => self.pad_available = true,
                _ => {}
            }
        }
        self.pad_available = gilrs.gamepads().next().is_some();
    }

    pub fn gamepad_connected(&self) -> bool {
        self.pad_available
    }
}

fn key_code(name: &str) -> Option<KeyCode> {
    use KeyCode as K;
    Some(match name {
        "Space" => K::Space,
        "Escape" | "Esc" => K::Escape,
        "Enter" => K::Enter,
        "Backspace" => K::Backspace,
        "Tab" => K::Tab,
        "ShiftLeft" => K::ShiftLeft,
        "ShiftRight" => K::ShiftRight,
        "ControlLeft" | "Ctrl" => K::ControlLeft,
        "AltLeft" => K::AltLeft,
        "ArrowLeft" | "Left" => K::ArrowLeft,
        "ArrowRight" | "Right" => K::ArrowRight,
        "ArrowUp" | "Up" => K::ArrowUp,
        "ArrowDown" | "Down" => K::ArrowDown,
        "Minus" => K::Minus,
        "Equal" => K::Equal,
        "Comma" => K::Comma,
        "Period" => K::Period,
        "Slash" => K::Slash,
        "Semicolon" => K::Semicolon,
        "Quote" => K::Quote,
        "BracketLeft" => K::BracketLeft,
        "BracketRight" => K::BracketRight,
        "Backslash" => K::Backslash,
        _ => {
            let bytes = name.as_bytes();
            // "KeyA"..="KeyZ", "Digit0"..="Digit9", "F1"..="F12".
            if bytes.len() == 4 && &bytes[..3] == b"Key" {
                let c = bytes[3];
                if c.is_ascii_uppercase() {
                    return key_letter(c);
                }
            } else if bytes.len() == 6 && &bytes[..5] == b"Digit" {
                let d = bytes[5];
                if d.is_ascii_digit() {
                    return key_digit(d);
                }
            } else if bytes.len() == 2 && bytes[0] == b'F' {
                let f = bytes[1];
                if (b'1'..=b'9').contains(&f) {
                    return key_fn(f - b'0');
                }
            }
            return None;
        }
    })
}

fn key_letter(c: u8) -> Option<KeyCode> {
    Some(match c {
        b'A' => KeyCode::KeyA,
        b'B' => KeyCode::KeyB,
        b'C' => KeyCode::KeyC,
        b'D' => KeyCode::KeyD,
        b'E' => KeyCode::KeyE,
        b'F' => KeyCode::KeyF,
        b'G' => KeyCode::KeyG,
        b'H' => KeyCode::KeyH,
        b'I' => KeyCode::KeyI,
        b'J' => KeyCode::KeyJ,
        b'K' => KeyCode::KeyK,
        b'L' => KeyCode::KeyL,
        b'M' => KeyCode::KeyM,
        b'N' => KeyCode::KeyN,
        b'O' => KeyCode::KeyO,
        b'P' => KeyCode::KeyP,
        b'Q' => KeyCode::KeyQ,
        b'R' => KeyCode::KeyR,
        b'S' => KeyCode::KeyS,
        b'T' => KeyCode::KeyT,
        b'U' => KeyCode::KeyU,
        b'V' => KeyCode::KeyV,
        b'W' => KeyCode::KeyW,
        b'X' => KeyCode::KeyX,
        b'Y' => KeyCode::KeyY,
        b'Z' => KeyCode::KeyZ,
        _ => return None,
    })
}

fn key_digit(d: u8) -> Option<KeyCode> {
    Some(match d {
        b'0' => KeyCode::Digit0,
        b'1' => KeyCode::Digit1,
        b'2' => KeyCode::Digit2,
        b'3' => KeyCode::Digit3,
        b'4' => KeyCode::Digit4,
        b'5' => KeyCode::Digit5,
        b'6' => KeyCode::Digit6,
        b'7' => KeyCode::Digit7,
        b'8' => KeyCode::Digit8,
        b'9' => KeyCode::Digit9,
        _ => return None,
    })
}

fn key_fn(f: u8) -> Option<KeyCode> {
    Some(match f {
        1 => KeyCode::F1,
        2 => KeyCode::F2,
        3 => KeyCode::F3,
        4 => KeyCode::F4,
        5 => KeyCode::F5,
        6 => KeyCode::F6,
        7 => KeyCode::F7,
        8 => KeyCode::F8,
        9 => KeyCode::F9,
        _ => return None,
    })
}

fn mouse_button(name: &str) -> Option<MouseButton> {
    match name {
        "Left" => Some(MouseButton::Left),
        "Right" => Some(MouseButton::Right),
        "Middle" => Some(MouseButton::Middle),
        _ => None,
    }
}

fn pad_button_name(b: gilrs::Button) -> Option<&'static str> {
    use gilrs::Button as B;
    Some(match b {
        B::South => "South",
        B::East => "East",
        B::North => "North",
        B::West => "West",
        B::Start => "Start",
        B::Select => "Select",
        B::Mode => "Mode",
        B::LeftThumb => "LeftThumb",
        B::RightThumb => "RightThumb",
        B::DPadUp => "DPadUp",
        B::DPadDown => "DPadDown",
        B::DPadLeft => "DPadLeft",
        B::DPadRight => "DPadRight",
        B::LeftTrigger => "LeftTrigger",
        B::RightTrigger => "RightTrigger",
        B::LeftTrigger2 => "LeftTrigger2",
        B::RightTrigger2 => "RightTrigger2",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_edges() {
        let mut i = Input::new();
        i.end_frame();
        i.on_key(KeyCode::Space, ElementState::Pressed);
        assert!(i.key_pressed("Space"));
        assert!(i.key_held("Space"));
        assert!(!i.key_released("Space"));

        i.end_frame();
        assert!(!i.key_pressed("Space"));
        assert!(i.key_held("Space"));

        i.on_key(KeyCode::Space, ElementState::Released);
        assert!(i.key_released("Space"));
        assert!(!i.key_held("Space"));
    }

    #[test]
    fn actions() {
        let mut i = Input::new();
        let mut map = HashMap::new();
        map.insert(
            "jump".to_string(),
            vec![Binding::Key("Space".into()), Binding::Pad("South".into())],
        );
        i.set_actions(map);
        i.end_frame();
        i.on_key(KeyCode::Space, ElementState::Pressed);
        assert!(i.action_pressed("jump"));
        assert!(i.action_held("jump"));
        assert!(!i.action_pressed("nope"));
    }
}
