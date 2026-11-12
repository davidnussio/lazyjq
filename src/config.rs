use serde::Deserialize;
use tokio::time::Duration;

mod duration;
use duration::duration_serde;
pub mod keys;
pub use keys::Keys;
mod style;
use style::style_serde;

use ratatui::{style::Style, widgets::BorderType};

pub static DEFAULT_CONFIG: &str = include_str!("../default.toml");

/// Application configuration.
///
/// The user configuration file is merged on top of the defaults (`default.toml`),
/// so it only needs to contain the settings that differ from them.
#[derive(Deserialize)]
pub struct Config {
    pub no_hint: bool,
    pub reactivity_control: ReactivityControl,
    pub ui: UiConfig,
    pub theme: Theme,
    pub editor: EditorConfig,
    pub json: JsonConfig,
    pub completion: CompletionConfig,
    pub history: HistoryConfig,
    pub open: OpenConfig,
    pub keybinds: Keybinds,
}

impl Config {
    /// Load the configuration, applying `content` on top of the defaults.
    pub fn load_from(content: &str) -> anyhow::Result<Self> {
        let mut base: toml::Table = toml::from_str(DEFAULT_CONFIG)?;
        let user: toml::Table = toml::from_str(content)?;
        let user_keybinds = user.get("keybinds").cloned();
        merge(&mut base, user);
        if let Some(toml::Value::Table(user_keybinds)) = user_keybinds {
            drop_shadowed_defaults(&mut base, &user_keybinds);
        }
        Ok(toml::Value::Table(base).try_into()?)
    }
}

/// In each keybinding section, the keys the user binds to an action are
/// removed from the *default* bindings of the other actions, which would
/// otherwise compete for them.
///
/// A configuration file written by an older version keeps the defaults of the
/// time (e.g. `copy_result = ["Ctrl+O"]`): those keep working as written, and a
/// newer default using the same key (`open_file = ["Ctrl+O"]`) gives way.
fn drop_shadowed_defaults(base: &mut toml::Table, user_keybinds: &toml::Table) {
    let Some(toml::Value::Table(sections)) = base.get_mut("keybinds") else {
        return;
    };
    for (name, section) in sections.iter_mut() {
        let (Some(toml::Value::Table(user_section)), toml::Value::Table(section)) =
            (user_keybinds.get(name), section)
        else {
            continue;
        };
        let taken: Vec<_> = user_section
            .values()
            .flat_map(labels)
            .filter_map(|label| keys::parse_label(&label).ok())
            .collect();
        for (action, value) in section.iter_mut() {
            if user_section.contains_key(action) {
                continue;
            }
            let kept: Vec<toml::Value> = labels(value)
                .into_iter()
                .filter(|label| keys::parse_label(label).map_or(true, |key| !taken.contains(&key)))
                .map(toml::Value::String)
                .collect();
            *value = toml::Value::Array(kept);
        }
    }
}

/// The labels of a binding written as `"Ctrl+O"` or `["Ctrl+O", "o"]`.
fn labels(value: &toml::Value) -> Vec<String> {
    match value {
        toml::Value::String(label) => vec![label.clone()],
        toml::Value::Array(items) => items
            .iter()
            .filter_map(|item| item.as_str().map(str::to_string))
            .collect(),
        _ => Vec::new(),
    }
}

/// Recursively merge `overlay` into `base`; tables are merged, any other value replaces.
fn merge(base: &mut toml::Table, overlay: toml::Table) {
    for (key, value) in overlay {
        match (base.get_mut(&key), value) {
            (Some(toml::Value::Table(base_table)), toml::Value::Table(overlay_table)) => {
                merge(base_table, overlay_table)
            }
            (_, value) => {
                base.insert(key, value);
            }
        }
    }
}

#[derive(Deserialize)]
pub struct ReactivityControl {
    #[serde(with = "duration_serde")]
    pub query_debounce_duration: Duration,
    #[serde(with = "duration_serde")]
    pub spin_duration: Duration,
}

#[derive(Clone, Copy, Deserialize)]
pub enum Border {
    Plain,
    Rounded,
    Double,
    Thick,
}

impl From<Border> for BorderType {
    fn from(border: Border) -> Self {
        match border {
            Border::Plain => BorderType::Plain,
            Border::Rounded => BorderType::Rounded,
            Border::Double => BorderType::Double,
            Border::Thick => BorderType::Thick,
        }
    }
}

#[derive(Deserialize)]
pub struct UiConfig {
    /// Width of the side panels column, in percent of the terminal width.
    pub side_panel_width: u16,
    /// Below this terminal width the panels are stacked vertically.
    pub portrait_threshold: u16,
    pub border: Border,
}

#[derive(Deserialize)]
pub struct Theme {
    #[serde(with = "style_serde")]
    pub active_border: Style,
    #[serde(with = "style_serde")]
    pub inactive_border: Style,
    #[serde(with = "style_serde")]
    pub title: Style,
    #[serde(with = "style_serde")]
    pub selected_line: Style,
    #[serde(with = "style_serde")]
    pub inactive_selected_line: Style,
    #[serde(with = "style_serde")]
    pub hint_key: Style,
    #[serde(with = "style_serde")]
    pub hint_text: Style,
    #[serde(with = "style_serde")]
    pub dim: Style,
    #[serde(with = "style_serde")]
    pub success: Style,
    #[serde(with = "style_serde")]
    pub warning: Style,
    #[serde(with = "style_serde")]
    pub error: Style,
    #[serde(with = "style_serde")]
    pub doc_heading: Style,
    #[serde(with = "style_serde")]
    pub doc_code: Style,
}

#[derive(Deserialize)]
pub struct EditorConfig {
    pub prompt: String,
    #[serde(with = "style_serde")]
    pub prompt_style: Style,
    pub word_break_chars: Vec<char>,
}

#[derive(Deserialize)]
pub struct JsonConfig {
    pub max_streams: Option<usize>,
    pub indent: usize,
    #[serde(with = "style_serde")]
    pub bracket_style: Style,
    #[serde(with = "style_serde")]
    pub key_style: Style,
    #[serde(with = "style_serde")]
    pub string_value_style: Style,
    #[serde(with = "style_serde")]
    pub number_value_style: Style,
    #[serde(with = "style_serde")]
    pub boolean_value_style: Style,
    #[serde(with = "style_serde")]
    pub null_value_style: Style,
}

#[derive(Deserialize)]
pub struct CompletionConfig {
    pub max_suggestions: usize,
    pub max_value_suggestions: usize,
}

#[derive(Deserialize)]
pub struct HistoryConfig {
    pub persist: bool,
    pub max_entries: usize,
}

#[derive(Deserialize)]
pub struct OpenConfig {
    /// Extensions of the files offered when completing a file name to open.
    pub extensions: Vec<String>,
}

#[derive(Deserialize)]
pub struct Keybinds {
    pub global: GlobalKeybinds,
    pub query: QueryKeybinds,
    pub list: ListKeybinds,
    pub result: ResultKeybinds,
    pub popup: PopupKeybinds,
}

#[derive(Deserialize)]
pub struct GlobalKeybinds {
    pub quit: Keys,
    pub force_quit: Keys,
    pub help: Keys,
    pub copy_query: Keys,
    pub copy_result: Keys,
    pub save_result: Keys,
    pub open_file: Keys,
    pub focus_query: Keys,
    pub focus_suggestions: Keys,
    pub focus_history: Keys,
    pub focus_result: Keys,
    pub next_panel: Keys,
    pub previous_panel: Keys,
}

#[derive(Deserialize)]
pub struct QueryKeybinds {
    pub submit: Keys,
    pub leave: Keys,
    pub complete: Keys,
    pub history_previous: Keys,
    pub history_next: Keys,
    pub backward: Keys,
    pub forward: Keys,
    pub move_to_head: Keys,
    pub move_to_tail: Keys,
    pub move_to_previous_word: Keys,
    pub move_to_next_word: Keys,
    pub erase: Keys,
    pub erase_forward: Keys,
    pub erase_all: Keys,
    pub erase_to_previous_word: Keys,
    pub erase_to_next_word: Keys,
    pub erase_to_tail: Keys,
}

#[derive(Deserialize)]
pub struct ListKeybinds {
    pub up: Keys,
    pub down: Keys,
    pub page_up: Keys,
    pub page_down: Keys,
    pub top: Keys,
    pub bottom: Keys,
    pub select: Keys,
    pub back: Keys,
    pub delete: Keys,
}

#[derive(Deserialize)]
pub struct ResultKeybinds {
    pub up: Keys,
    pub down: Keys,
    pub page_up: Keys,
    pub page_down: Keys,
    pub top: Keys,
    pub bottom: Keys,
    pub toggle: Keys,
    pub expand: Keys,
    pub collapse: Keys,
    pub expand_all: Keys,
    pub collapse_all: Keys,
    pub drill_down: Keys,
    pub drill_up: Keys,
    pub copy_menu: Keys,
    pub back: Keys,
}

#[derive(Deserialize)]
pub struct PopupKeybinds {
    pub up: Keys,
    pub down: Keys,
    pub select: Keys,
    pub close: Keys,
    pub toggle_all_files: Keys,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_loads() {
        let config = Config::load_from("").unwrap();
        assert_eq!(config.json.indent, 2);
    }

    #[test]
    fn user_config_overrides_only_given_keys() {
        let config = Config::load_from(
            r#"
            [json]
            indent = 4
            [keybinds.global]
            quit = "Ctrl+X"
            "#,
        )
        .unwrap();
        assert_eq!(config.json.indent, 4);
        assert_eq!(config.json.max_streams, None);
        assert_eq!(config.keybinds.global.quit.display(), Some("Ctrl+X"));
        assert!(config.keybinds.global.force_quit.display().is_some());
    }

    #[test]
    fn user_bindings_win_over_other_defaults() {
        // As written by version 0.3.0, where Ctrl+O copied the result.
        let config = Config::load_from(
            r#"
            [keybinds.global]
            copy_result = ["Ctrl+O"]
            "#,
        )
        .unwrap();
        let global = &config.keybinds.global;
        let ctrl_o = crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('o'),
            crossterm::event::KeyModifiers::CONTROL,
        );
        assert!(global.copy_result.matches(&ctrl_o));
        assert!(!global.open_file.matches(&ctrl_o));
        assert_eq!(global.open_file.display(), None);

        // Both set by the user: kept as written.
        let config = Config::load_from(
            r#"
            [keybinds.global]
            copy_result = "ctrl+o"
            open_file = ["Ctrl+O", "Ctrl+P"]
            "#,
        )
        .unwrap();
        assert!(config.keybinds.global.open_file.matches(&ctrl_o));

        // Without bindings of the user, the defaults apply.
        let config = Config::load_from("").unwrap();
        assert!(config.keybinds.global.open_file.matches(&ctrl_o));
    }

    #[test]
    fn invalid_values_are_reported() {
        assert!(Config::load_from("[json]\nindent = \"two\"").is_err());
        assert!(Config::load_from("[keybinds.global]\nquit = \"Ctrl+\"").is_err());
    }
}
