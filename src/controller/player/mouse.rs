//! Pointer and mouse-wheel forwarding from Slint's video area into `libmpv`.

use std::collections::HashMap;

use memento_player::controller::{
    QT_ALT_MODIFIER, QT_CONTROL_MODIFIER, QT_META_MODIFIER, QT_NO_MODIFIER, QT_SHIFT_MODIFIER,
};
use memento_player::{format_modifiers, MouseButton, MpvInstance, WheelButton};

/// Stateful pointer/wheel forwarder from Slint's video area into `libmpv`
/// (`mouse`, `keydown`/`keyup` `MOUSE_BTN*`, and wheel buttons `3..6`) for
/// `mpv` scripts such as `uosc`.
#[derive(Debug, Default)]
pub struct MouseInputForwarder {
    last_pos: Option<(i32, i32)>,
    pressed_buttons: HashMap<u32, String>,
}

impl MouseInputForwarder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn modifiers_mask(control: bool, shift: bool, alt: bool, meta: bool) -> i32 {
        let mut mods = QT_NO_MODIFIER;
        if shift {
            mods |= QT_SHIFT_MODIFIER;
        }
        if control {
            mods |= QT_CONTROL_MODIFIER;
        }
        if alt {
            mods |= QT_ALT_MODIFIER;
        }
        if meta {
            mods |= QT_META_MODIFIER;
        }
        mods
    }

    pub fn map_to_render_coords(
        x: f32,
        y: f32,
        viewport_w: f32,
        viewport_h: f32,
        scale_factor: f32,
        render_size: (u32, u32),
    ) -> (i32, i32) {
        let (rw, rh) = render_size;
        if viewport_w > 0.0 && viewport_h > 0.0 && rw > 0 && rh > 0 {
            let fit = (viewport_w / rw as f32).min(viewport_h / rh as f32);
            if fit > 0.0 && fit.is_finite() {
                let drawn_w = rw as f32 * fit;
                let drawn_h = rh as f32 * fit;
                let off_x = (viewport_w - drawn_w) * 0.5;
                let off_y = (viewport_h - drawn_h) * 0.5;
                let mx = ((x - off_x) / fit).round().clamp(0.0, rw as f32) as i32;
                let my = ((y - off_y) / fit).round().clamp(0.0, rh as f32) as i32;
                return (mx, my);
            }
        }
        let scale = if scale_factor.is_finite() && scale_factor > 0.0 {
            scale_factor
        } else {
            1.0
        };
        ((x * scale).round() as i32, (y * scale).round() as i32)
    }

    pub fn record_move(&mut self, x: i32, y: i32) -> Option<[String; 3]> {
        if self.last_pos == Some((x, y)) {
            return None;
        }
        self.last_pos = Some((x, y));
        Some(["mouse".to_owned(), x.to_string(), y.to_string()])
    }

    pub fn send_move(&mut self, player: &MpvInstance, x: i32, y: i32) {
        if let Some(cmd) = self.record_move(x, y) {
            let _ = player.command_async(&[&cmd[0], &cmd[1], &cmd[2]]);
        }
    }

    pub fn record_button(
        &mut self,
        x: i32,
        y: i32,
        qt_button: u32,
        modifiers: i32,
        pressed: bool,
    ) -> Vec<Vec<String>> {
        let Some(button) = MouseButton::from_qt(qt_button) else {
            return Vec::new();
        };
        let mut commands = Vec::with_capacity(3);
        self.last_pos = Some((x, y));
        commands.push(vec!["mouse".to_owned(), x.to_string(), y.to_string()]);
        let key = format!("{}{}", format_modifiers(modifiers), button.as_str());
        if pressed {
            if let Some(prev_key) = self.pressed_buttons.remove(&qt_button) {
                commands.push(vec!["keyup".to_owned(), prev_key]);
            }
            self.pressed_buttons.insert(qt_button, key.clone());
            commands.push(vec!["keydown".to_owned(), key]);
        } else {
            let release_key = self.pressed_buttons.remove(&qt_button).unwrap_or(key);
            commands.push(vec!["keyup".to_owned(), release_key]);
        }
        commands
    }

    pub fn send_button(
        &mut self,
        player: &MpvInstance,
        x: i32,
        y: i32,
        qt_button: u32,
        modifiers: i32,
        pressed: bool,
    ) {
        for cmd in self.record_button(x, y, qt_button, modifiers, pressed) {
            let refs: Vec<&str> = cmd.iter().map(String::as_str).collect();
            let _ = player.command_async(&refs);
        }
    }

    pub fn record_release_all(&mut self) -> Vec<Vec<String>> {
        let mut keys: Vec<(u32, String)> = self.pressed_buttons.drain().collect();
        keys.sort_by_key(|(btn, _)| *btn);
        keys.into_iter()
            .map(|(_, key)| vec!["keyup".to_owned(), key])
            .collect()
    }

    pub fn release_all(&mut self, player: &MpvInstance) {
        for cmd in self.record_release_all() {
            let refs: Vec<&str> = cmd.iter().map(String::as_str).collect();
            let _ = player.command_async(&refs);
        }
    }

    pub fn record_wheel(
        &mut self,
        x: i32,
        y: i32,
        delta_x: f32,
        delta_y: f32,
    ) -> Option<[String; 4]> {
        let dx = if delta_x.abs() >= 0.5 {
            delta_x.round() as i32
        } else if delta_x != 0.0 {
            delta_x.signum() as i32
        } else {
            0
        };
        let dy = if delta_y.abs() >= 0.5 {
            delta_y.round() as i32
        } else if delta_y != 0.0 {
            delta_y.signum() as i32
        } else {
            0
        };
        let wheel = WheelButton::from_deltas(dx, dy)?;
        self.last_pos = Some((x, y));
        Some([
            "mouse".to_owned(),
            x.to_string(),
            y.to_string(),
            wheel.as_str().to_owned(),
        ])
    }

    pub fn send_wheel(
        &mut self,
        player: &MpvInstance,
        x: i32,
        y: i32,
        delta_x: f32,
        delta_y: f32,
    ) {
        if let Some(cmd) = self.record_wheel(x, y, delta_x, delta_y) {
            let _ = player.command_async(&[&cmd[0], &cmd[1], &cmd[2], &cmd[3]]);
        }
    }
}
