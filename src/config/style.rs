//! Styles are written with the termcfg notation (`"fg=blue,attr=bold"`),
//! parsed into crossterm styles and converted to ratatui styles.

use crossterm::style::{Attribute, Color as CColor, ContentStyle};
use ratatui::style::{Color, Modifier, Style};

pub mod style_serde {
    use super::*;
    use serde::Deserializer;
    use termcfg::crossterm_config::content_style_serde;

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Style, D::Error> {
        content_style_serde::deserialize(deserializer).map(to_style)
    }
}

pub fn to_style(style: ContentStyle) -> Style {
    let mut out = Style::default();
    if let Some(fg) = style.foreground_color {
        out = out.fg(to_color(fg));
    }
    if let Some(bg) = style.background_color {
        out = out.bg(to_color(bg));
    }
    if let Some(ul) = style.underline_color {
        out = out.underline_color(to_color(ul));
    }
    const ATTRIBUTES: [(Attribute, Modifier); 9] = [
        (Attribute::Bold, Modifier::BOLD),
        (Attribute::Dim, Modifier::DIM),
        (Attribute::Italic, Modifier::ITALIC),
        (Attribute::Underlined, Modifier::UNDERLINED),
        (Attribute::SlowBlink, Modifier::SLOW_BLINK),
        (Attribute::RapidBlink, Modifier::RAPID_BLINK),
        (Attribute::Reverse, Modifier::REVERSED),
        (Attribute::Hidden, Modifier::HIDDEN),
        (Attribute::CrossedOut, Modifier::CROSSED_OUT),
    ];
    for (attribute, modifier) in ATTRIBUTES {
        if style.attributes.has(attribute) {
            out = out.add_modifier(modifier);
        }
    }
    out
}

/// crossterm names the bright colors without a prefix (`Red`) and the normal ones `Dark*`,
/// ratatui uses `Light*` for the bright ones.
fn to_color(color: CColor) -> Color {
    match color {
        CColor::Reset => Color::Reset,
        CColor::Black => Color::Black,
        CColor::DarkGrey => Color::DarkGray,
        CColor::Red => Color::LightRed,
        CColor::DarkRed => Color::Red,
        CColor::Green => Color::LightGreen,
        CColor::DarkGreen => Color::Green,
        CColor::Yellow => Color::LightYellow,
        CColor::DarkYellow => Color::Yellow,
        CColor::Blue => Color::LightBlue,
        CColor::DarkBlue => Color::Blue,
        CColor::Magenta => Color::LightMagenta,
        CColor::DarkMagenta => Color::Magenta,
        CColor::Cyan => Color::LightCyan,
        CColor::DarkCyan => Color::Cyan,
        CColor::White => Color::White,
        CColor::Grey => Color::Gray,
        CColor::Rgb { r, g, b } => Color::Rgb(r, g, b),
        CColor::AnsiValue(v) => Color::Indexed(v),
    }
}
