use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use serde::{Deserialize, Deserializer};
use termcfg::event::parse::parse_shortcut;

/// A set of key bindings for one action, e.g. `["Down", "j"]`.
///
/// The textual form of each binding is kept for the hint bar and the help popup.
#[derive(Clone, Debug, Default)]
pub struct Keys {
    bindings: Vec<(KeyCode, KeyModifiers)>,
    labels: Vec<String>,
}

impl Keys {
    /// Whether `key` triggers this action.
    pub fn matches(&self, key: &KeyEvent) -> bool {
        let pressed = normalize(key.code, key.modifiers);
        self.bindings.contains(&pressed)
    }

    /// The label of the first binding, used in hints.
    pub fn display(&self) -> Option<&str> {
        self.labels.first().map(String::as_str)
    }

    /// All labels joined, used in the help popup.
    pub fn display_all(&self) -> String {
        self.labels.join(", ")
    }

    fn parse(labels: Vec<String>) -> Result<Self, String> {
        let bindings = labels
            .iter()
            .map(|label| parse_label(label))
            .collect::<Result<_, _>>()?;
        Ok(Self { bindings, labels })
    }
}

/// The key a binding such as `Ctrl+O` or `G` stands for.
pub fn parse_label(label: &str) -> Result<(KeyCode, KeyModifiers), String> {
    // termcfg lowercases single letters (`G` would become `g`): take them literally.
    let mut chars = label.chars();
    if let (Some(ch), None) = (chars.next(), chars.next()) {
        return Ok(normalize(KeyCode::Char(ch), KeyModifiers::NONE));
    }
    let event: Event = parse_shortcut(label).map_err(|e| e.to_string())?.into();
    match event {
        Event::Key(key) => Ok(normalize(key.code, key.modifiers)),
        _ => Err(format!("`{label}` is not a key")),
    }
}

/// Whether the key types a character into a text field.
pub fn is_text_input(code: KeyCode, modifiers: KeyModifiers) -> bool {
    matches!(code, KeyCode::Char(_))
        && !modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER)
}

/// Make bindings independent of how terminals report Shift:
/// a character already carries its case (`G`, `?`), and Shift+Tab is BackTab.
fn normalize(code: KeyCode, modifiers: KeyModifiers) -> (KeyCode, KeyModifiers) {
    match code {
        KeyCode::Char(_) => (code, modifiers - KeyModifiers::SHIFT),
        KeyCode::Tab if modifiers.contains(KeyModifiers::SHIFT) => {
            (KeyCode::BackTab, modifiers - KeyModifiers::SHIFT)
        }
        KeyCode::BackTab => (code, modifiers - KeyModifiers::SHIFT),
        _ => (code, modifiers),
    }
}

impl<'de> Deserialize<'de> for Keys {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Input {
            One(String),
            Many(Vec<String>),
        }
        let labels = match Input::deserialize(deserializer)? {
            Input::One(label) => vec![label],
            Input::Many(labels) => labels,
        };
        Keys::parse(labels).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(labels: &[&str]) -> Keys {
        Keys::parse(labels.iter().map(|s| s.to_string()).collect()).unwrap()
    }

    fn press(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    #[test]
    fn shift_is_ignored_for_characters() {
        let k = keys(&["G", "?"]);
        assert!(k.matches(&press(KeyCode::Char('G'), KeyModifiers::SHIFT)));
        assert!(k.matches(&press(KeyCode::Char('G'), KeyModifiers::NONE)));
        assert!(k.matches(&press(KeyCode::Char('?'), KeyModifiers::SHIFT)));
        assert!(!k.matches(&press(KeyCode::Char('g'), KeyModifiers::NONE)));
    }

    #[test]
    fn shift_tab_is_backtab() {
        let k = keys(&["Shift+Tab"]);
        assert!(k.matches(&press(KeyCode::BackTab, KeyModifiers::SHIFT)));
        assert!(k.matches(&press(KeyCode::BackTab, KeyModifiers::NONE)));
        assert!(!k.matches(&press(KeyCode::Tab, KeyModifiers::NONE)));
    }

    #[test]
    fn modifiers_must_match() {
        let k = keys(&["Ctrl+C"]);
        assert!(k.matches(&press(KeyCode::Char('c'), KeyModifiers::CONTROL)));
        assert!(!k.matches(&press(KeyCode::Char('c'), KeyModifiers::NONE)));
        assert!(!is_text_input(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert!(is_text_input(KeyCode::Char('Q'), KeyModifiers::SHIFT));
    }
}
