//! Fogbreak Carmel Stone / Monterey Shale palette overlay for egui.

use egui::{Color32, Context, Stroke, Visuals};

pub const CARMEL_STONE: Color32 = Color32::from_rgb(0xF7, 0xFA, 0xFC);
pub const MONTEREY_SHALE: Color32 = Color32::from_rgb(0x11, 0x18, 0x21);
pub const FOGBREAK_TEAL: Color32 = Color32::from_rgb(0x08, 0x7F, 0x9C);
pub const PANEL_LIGHT: Color32 = Color32::WHITE;
pub const PANEL_DARK: Color32 = Color32::from_rgb(0x1A, 0x24, 0x31);
pub const BORDER_LIGHT: Color32 = Color32::from_rgb(0xCB, 0xD5, 0xE1);
pub const BORDER_DARK: Color32 = Color32::from_rgb(0x2B, 0x39, 0x4B);

pub fn apply_palette(ctx: &Context, dark: bool) {
    let mut visuals = if dark { Visuals::dark() } else { Visuals::light() };
    let bg = if dark { MONTEREY_SHALE } else { CARMEL_STONE };
    let panel = if dark { PANEL_DARK } else { PANEL_LIGHT };
    let ink = if dark { CARMEL_STONE } else { MONTEREY_SHALE };
    let border = if dark { BORDER_DARK } else { BORDER_LIGHT };

    visuals.window_fill = panel;
    visuals.panel_fill = bg;
    visuals.faint_bg_color = bg;
    visuals.extreme_bg_color = bg;
    visuals.selection.bg_fill = FOGBREAK_TEAL;
    visuals.selection.stroke = Stroke::new(1.0, ink);
    visuals.widgets.noninteractive.bg_fill = panel;
    visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, border);
    visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0, ink);
    visuals.widgets.inactive.bg_fill = panel;
    visuals.widgets.inactive.bg_stroke = Stroke::new(1.0, border);
    visuals.widgets.inactive.fg_stroke = Stroke::new(1.0, ink);
    visuals.widgets.active.bg_fill = FOGBREAK_TEAL;
    visuals.widgets.active.fg_stroke = Stroke::new(1.0, Color32::WHITE);
    visuals.hyperlink_color = FOGBREAK_TEAL;
    ctx.set_visuals(visuals);
}
