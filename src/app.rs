//! Application state and event handling.
//!
//! Everything runs on a single loop: terminal events, results of background
//! work (jq evaluation, value suggestions, path indexing) and timers update
//! the state, then the whole screen is drawn again.

use std::{sync::Arc, time::Duration};

use arboard::Clipboard;
use crossterm::event::{
    Event, EventStream, KeyEvent, KeyEventKind, MouseButton, MouseEvent, MouseEventKind,
};
use futures::StreamExt;
use ratatui::{
    layout::{Position, Rect},
    widgets::ListState,
    DefaultTerminal,
};
use serde_json::Value;
use tokio::{
    sync::mpsc,
    time::{sleep_until, Instant},
};

use crate::{
    config::{keys::is_text_input, Config},
    editor::LineEditor,
    history::History,
    jq_worker::JqWorker,
    json_tree::{path_to_filter, pretty, JsonTree},
    suggestions::{PathIndex, Suggestions},
    ui, value_completion,
};

/// Number of paths published at once by the background indexer.
const PATH_LOAD_CHUNK: usize = 50_000;
/// How long a message stays in the bottom bar.
const TOAST_DURATION: Duration = Duration::from_secs(4);
/// Lines moved per mouse wheel step.
const SCROLL_STEP: usize = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Panel {
    Query,
    Suggestions,
    History,
    Result,
}

impl Panel {
    pub const ALL: [Panel; 4] = [
        Panel::Query,
        Panel::Suggestions,
        Panel::History,
        Panel::Result,
    ];

    pub fn number(self) -> usize {
        Panel::ALL.iter().position(|p| *p == self).unwrap_or(0) + 1
    }

    pub fn title(self) -> &'static str {
        match self {
            Panel::Query => "Query",
            Panel::Suggestions => "Suggestions",
            Panel::History => "History",
            Panel::Result => "Result",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Severity {
    Success,
    Warning,
    Error,
}

pub struct Toast {
    pub text: String,
    pub severity: Severity,
    until: Instant,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MenuAction {
    Path,
    Value,
    Result,
    Query,
}

pub struct MenuItem {
    pub key: char,
    pub label: String,
    pub action: MenuAction,
}

pub struct Menu {
    pub title: &'static str,
    pub items: Vec<MenuItem>,
    pub selected: usize,
}

pub enum Popup {
    Help { scroll: usize },
    Menu(Menu),
}

/// What the Result panel shows.
pub struct ResultView {
    pub tree: JsonTree,
    /// The query that produced `tree`.
    pub query: String,
}

/// Outcome of evaluating a query in the background.
pub enum Outcome {
    Tree(JsonTree),
    Empty,
    AllNull,
    Failed(String),
}

pub enum AppEvent {
    Evaluated {
        generation: u64,
        query: String,
        outcome: Outcome,
    },
    Values {
        source_query: String,
        result: anyhow::Result<Vec<String>>,
    },
    PathsProgress,
}

/// Screen areas from the last draw, used for mouse handling and paging.
#[derive(Default)]
pub struct Areas {
    pub query: Rect,
    pub suggestions: Rect,
    pub history: Rect,
    pub result: Rect,
}

pub struct App {
    pub config: Arc<Config>,
    pub focus: Panel,
    pub popup: Option<Popup>,
    pub editor: LineEditor,
    pub result: ResultView,
    /// Problem with the query currently typed (error, or `null` result):
    /// the Result panel then keeps showing the last valid result.
    pub query_status: Option<(String, Severity)>,
    /// The current query evaluated successfully to an empty stream.
    pub empty_result: bool,
    pub suggestions: Suggestions,
    pub suggestions_list: ListState,
    pub history: History,
    pub history_list: ListState,
    pub toast: Option<Toast>,
    pub evaluating: bool,
    pub spinner: usize,
    pub areas: Areas,
    pub paths: PathIndex,

    input: Arc<Vec<Value>>,
    worker: JqWorker,
    tx: mpsc::UnboundedSender<AppEvent>,
    generation: u64,
    eval_deadline: Option<Instant>,
    /// The query text last sent for evaluation.
    requested_query: String,
    /// Submitted query waiting for its result before being saved in the history.
    pending_history: Option<String>,
    /// Tree of the unfiltered input, kept while another query is shown so that
    /// going back to `.` is instant and keeps folds and cursor.
    input_tree: Option<JsonTree>,
    values_in_flight: Option<String>,
    quit: bool,
}

/// Whether the query is the identity filter, whose result is the input itself.
pub fn is_identity(query: &str) -> bool {
    matches!(query.trim(), "" | ".")
}

impl App {
    pub fn new(
        config: Config,
        input: Arc<Vec<Value>>,
        default_filter: Option<String>,
        tx: mpsc::UnboundedSender<AppEvent>,
    ) -> Self {
        let config = Arc::new(config);
        let history_file = config.history.persist.then(History::default_file).flatten();
        let history = History::load(history_file, config.history.max_entries);
        let worker = JqWorker::spawn(input.clone());

        let paths = PathIndex::default();
        {
            let tx = tx.clone();
            paths.spawn_loader(input.clone(), PATH_LOAD_CHUNK, move || {
                let _ = tx.send(AppEvent::PathsProgress);
            });
        }

        let mut app = Self {
            focus: Panel::Query,
            popup: None,
            editor: LineEditor::new(default_filter.as_deref().unwrap_or("")),
            result: ResultView {
                tree: JsonTree::new(input.clone()),
                query: ".".into(),
            },
            query_status: None,
            empty_result: false,
            suggestions: Suggestions::default(),
            suggestions_list: ListState::default(),
            history,
            history_list: ListState::default(),
            toast: None,
            evaluating: false,
            spinner: 0,
            areas: Areas::default(),
            paths,
            input,
            worker,
            tx,
            generation: 0,
            eval_deadline: None,
            requested_query: ".".into(),
            pending_history: None,
            input_tree: None,
            values_in_flight: None,
            quit: false,
            config,
        };
        app.refresh_suggestions();
        if default_filter.is_some() {
            app.evaluate_now();
        }
        app
    }

    pub async fn run(
        mut self,
        terminal: &mut DefaultTerminal,
        mut rx: mpsc::UnboundedReceiver<AppEvent>,
    ) -> anyhow::Result<Self> {
        let mut events = EventStream::new();
        let mut tick = tokio::time::interval(self.config.reactivity_control.spin_duration);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            terminal.draw(|frame| ui::draw(frame, &mut self))?;
            if self.quit {
                break;
            }
            let deadline = self.eval_deadline;
            tokio::select! {
                event = events.next() => match event {
                    Some(Ok(event)) => self.handle_event(event),
                    Some(Err(e)) => return Err(e.into()),
                    None => break,
                },
                Some(event) = rx.recv() => self.handle_app_event(event),
                _ = tick.tick(), if self.needs_tick() => self.on_tick(),
                _ = sleep_until(deadline.unwrap_or_else(Instant::now)), if deadline.is_some() => {
                    self.evaluate_now();
                }
            }
        }

        self.on_exit();
        Ok(self)
    }

    /// The result as text, for `--write-to-stdout`.
    pub fn formatted_result(&self) -> String {
        self.result.tree.formatted(self.config.json.indent)
    }

    fn on_exit(&mut self) {
        if self.query_status.is_none() && !is_identity(&self.result.query) {
            let query = self.result.query.clone();
            self.history.push(&query);
        }
        if let Err(e) = self.history.save() {
            // The terminal is about to be restored, nothing better to do than to report it.
            eprintln!("lazyjq: failed to save the history: {e}");
        }
    }

    fn needs_tick(&self) -> bool {
        self.evaluating || self.toast.is_some() || !self.paths.is_complete()
    }

    fn on_tick(&mut self) {
        self.spinner = self.spinner.wrapping_add(1);
        if self
            .toast
            .as_ref()
            .is_some_and(|t| t.until <= Instant::now())
        {
            self.toast = None;
        }
    }

    pub fn notify(&mut self, text: impl Into<String>, severity: Severity) {
        self.toast = Some(Toast {
            text: text.into(),
            severity,
            until: Instant::now() + TOAST_DURATION,
        });
    }

    // --- Query evaluation -------------------------------------------------

    fn schedule_evaluation(&mut self) {
        self.eval_deadline =
            Some(Instant::now() + self.config.reactivity_control.query_debounce_duration);
    }

    fn evaluate_now(&mut self) {
        self.eval_deadline = None;
        let query = self.editor.text();
        if query == self.requested_query {
            return;
        }
        self.requested_query = query.clone();
        self.generation += 1;
        let generation = self.generation;

        if is_identity(&query) {
            if is_identity(&self.result.query) {
                // Already showing the input.
                self.evaluating = false;
                self.query_status = None;
                self.empty_result = false;
                return;
            }
            if let Some(tree) = self.input_tree.take() {
                self.apply_tree(tree, query);
                return;
            }
        }

        self.evaluating = true;
        let (input, worker, tx) = (self.input.clone(), self.worker.clone(), self.tx.clone());
        tokio::spawn(async move {
            if let Some(outcome) = evaluate(input, worker, &query).await {
                let _ = tx.send(AppEvent::Evaluated {
                    generation,
                    query,
                    outcome,
                });
            }
        });
    }

    fn apply_tree(&mut self, tree: JsonTree, query: String) {
        self.evaluating = false;
        self.query_status = None;
        self.empty_result = false;
        let previous = std::mem::replace(&mut self.result, ResultView { tree, query });
        // Keep the unfiltered tree around (with its folds) for a quick return to `.`.
        if is_identity(&previous.query) && !is_identity(&self.result.query) {
            self.input_tree = Some(previous.tree);
        }
        self.save_pending_history();
    }

    fn save_pending_history(&mut self) {
        if let Some(query) = self.pending_history.take() {
            if query == self.result.query {
                self.history.push(&query);
            }
        }
    }

    fn handle_app_event(&mut self, event: AppEvent) {
        match event {
            AppEvent::Evaluated {
                generation,
                query,
                outcome,
            } => {
                if generation != self.generation {
                    return;
                }
                self.evaluating = false;
                match outcome {
                    Outcome::Tree(tree) => self.apply_tree(tree, query),
                    Outcome::Empty => {
                        self.apply_tree(JsonTree::new(Arc::new(Vec::new())), query);
                        self.empty_result = true;
                    }
                    Outcome::AllNull => {
                        self.pending_history = None;
                        self.query_status = Some((
                            "jq returned null: check the filter for typos".into(),
                            Severity::Warning,
                        ));
                    }
                    Outcome::Failed(e) => {
                        self.pending_history = None;
                        self.query_status = Some((e, Severity::Error));
                    }
                }
            }
            AppEvent::Values {
                source_query,
                result,
            } => {
                if self.values_in_flight.as_deref() == Some(source_query.as_str()) {
                    self.values_in_flight = None;
                }
                if self.is_previewing() {
                    return;
                }
                let Some(request) = value_completion::parse(&self.editor.text()) else {
                    return;
                };
                if request.source_query != source_query {
                    return;
                }
                match result {
                    Ok(values) => {
                        let truncated =
                            values.len() >= self.config.completion.max_value_suggestions;
                        self.suggestions.set_values(&request, values, truncated);
                    }
                    Err(e) => self.suggestions.set_values_error(e.to_string()),
                }
            }
            AppEvent::PathsProgress => {
                if !self.is_previewing() && value_completion::parse(&self.editor.text()).is_none() {
                    self.refresh_suggestions();
                }
            }
        }
    }

    // --- Suggestions ------------------------------------------------------

    fn is_previewing(&self) -> bool {
        self.suggestions.preview_origin.is_some()
    }

    /// Recompute the Suggestions panel for the query being typed.
    fn refresh_suggestions(&mut self) {
        let text = self.editor.text();
        if let Some(request) = value_completion::parse(&text) {
            if self.suggestions.set_values_from_cache(&request) {
                return;
            }
            self.suggestions.set_values_pending(&request);
            if self.values_in_flight.as_deref() != Some(request.source_query.as_str()) {
                self.values_in_flight = Some(request.source_query.clone());
                let (worker, tx) = (self.worker.clone(), self.tx.clone());
                let limit = self.config.completion.max_value_suggestions;
                let source_query = request.source_query;
                tokio::spawn(async move {
                    let result = worker.scalar_values(source_query.clone(), limit).await;
                    let _ = tx.send(AppEvent::Values {
                        source_query,
                        result,
                    });
                });
            }
        } else {
            let (items, truncated) = self
                .paths
                .matches(text.trim_start(), self.config.completion.max_suggestions);
            self.suggestions
                .set_paths(items, truncated, !self.paths.is_complete());
        }
    }

    /// The query text changed because of an edit: update everything depending on it.
    fn on_text_changed(&mut self) {
        self.history.reset_browse();
        self.refresh_suggestions();
        self.schedule_evaluation();
    }

    fn set_query(&mut self, text: &str) {
        self.editor.set_text(text);
        self.on_text_changed();
    }

    fn preview_selected_suggestion(&mut self) {
        if let Some(query) = self.suggestions.selected_query() {
            self.editor.set_text(&query);
            self.schedule_evaluation();
        }
    }

    fn start_completion(&mut self) {
        match self.suggestions.items.len() {
            0 => {
                let note = self
                    .suggestions
                    .note
                    .clone()
                    .unwrap_or_else(|| "No suggestion".into());
                self.notify(note, Severity::Warning);
            }
            1 => {
                if let Some(query) = self.suggestions.selected_query() {
                    self.set_query(&query);
                }
            }
            _ => {
                self.suggestions.preview_origin = Some(self.editor.text());
                self.suggestions.selected = 0;
                self.focus = Panel::Suggestions;
                self.preview_selected_suggestion();
            }
        }
    }

    // --- Focus ------------------------------------------------------------

    pub fn set_focus(&mut self, panel: Panel) {
        if self.focus == Panel::Suggestions && panel != Panel::Suggestions {
            // Leaving the panel keeps the previewed suggestion.
            if self.suggestions.preview_origin.take().is_some() {
                self.refresh_suggestions();
            }
        }
        self.focus = panel;
    }

    fn cycle_focus(&mut self, forward: bool) {
        let i = self.focus.number() - 1;
        let n = Panel::ALL.len();
        let next = if forward {
            (i + 1) % n
        } else {
            (i + n - 1) % n
        };
        self.set_focus(Panel::ALL[next]);
    }

    // --- Events -----------------------------------------------------------

    fn handle_event(&mut self, event: Event) {
        match event {
            Event::Key(key) if key.kind != KeyEventKind::Release => self.handle_key(key),
            Event::Mouse(mouse) => self.handle_mouse(mouse),
            Event::Paste(text) if self.focus == Panel::Query && self.popup.is_none() => {
                for ch in text.chars().filter(|c| !c.is_control()) {
                    self.editor.insert(ch);
                }
                self.on_text_changed();
            }
            _ => {}
        }
    }

    fn handle_key(&mut self, key: KeyEvent) {
        let config = self.config.clone();
        let global = &config.keybinds.global;

        if global.force_quit.matches(&key) {
            self.quit = true;
            return;
        }
        if self.popup.is_some() {
            self.handle_popup_key(&key);
            return;
        }

        let handled = match self.focus {
            Panel::Query => self.handle_query_key(&key),
            Panel::Suggestions => self.handle_suggestions_key(&key),
            Panel::History => self.handle_history_key(&key),
            Panel::Result => self.handle_result_key(&key),
        };
        if handled {
            return;
        }

        // While typing, plain characters belong to the query.
        if self.focus == Panel::Query && is_text_input(key.code, key.modifiers) {
            if let crossterm::event::KeyCode::Char(ch) = key.code {
                self.editor.insert(ch);
                self.on_text_changed();
            }
            return;
        }

        if global.quit.matches(&key) {
            self.quit = true;
        } else if global.help.matches(&key) {
            self.popup = Some(Popup::Help { scroll: 0 });
        } else if global.copy_query.matches(&key) {
            self.copy(MenuAction::Query);
        } else if global.copy_result.matches(&key) {
            self.copy(MenuAction::Result);
        } else if global.focus_query.matches(&key) {
            self.set_focus(Panel::Query);
        } else if global.focus_suggestions.matches(&key) {
            self.set_focus(Panel::Suggestions);
        } else if global.focus_history.matches(&key) {
            self.set_focus(Panel::History);
        } else if global.focus_result.matches(&key) {
            self.set_focus(Panel::Result);
        } else if global.next_panel.matches(&key) {
            self.cycle_focus(true);
        } else if global.previous_panel.matches(&key) {
            self.cycle_focus(false);
        }
    }

    fn handle_query_key(&mut self, key: &KeyEvent) -> bool {
        let config = self.config.clone();
        let k = &config.keybinds.query;
        let breaks = &config.editor.word_break_chars;
        let before = self.editor.text();

        if k.submit.matches(key) {
            let query = self.editor.text();
            self.pending_history = Some(query.clone());
            if query == self.result.query && self.query_status.is_none() {
                self.save_pending_history();
            }
            self.evaluate_now();
            self.set_focus(Panel::Result);
            return true;
        } else if k.leave.matches(key) {
            self.set_focus(Panel::Result);
            return true;
        } else if k.complete.matches(key) {
            self.start_completion();
            return true;
        } else if k.history_previous.matches(key) {
            if let Some(query) = self.history.older().map(str::to_string) {
                self.editor.set_text(&query);
                self.refresh_suggestions();
                self.schedule_evaluation();
            }
            return true;
        } else if k.history_next.matches(key) {
            if let Some(query) = self.history.newer().map(str::to_string) {
                self.editor.set_text(&query);
                self.refresh_suggestions();
                self.schedule_evaluation();
            }
            return true;
        } else if k.backward.matches(key) {
            self.editor.backward();
        } else if k.forward.matches(key) {
            self.editor.forward();
        } else if k.move_to_head.matches(key) {
            self.editor.move_to_head();
        } else if k.move_to_tail.matches(key) {
            self.editor.move_to_tail();
        } else if k.move_to_previous_word.matches(key) {
            self.editor.move_to_previous_word(breaks);
        } else if k.move_to_next_word.matches(key) {
            self.editor.move_to_next_word(breaks);
        } else if k.erase.matches(key) {
            self.editor.erase();
        } else if k.erase_forward.matches(key) {
            self.editor.erase_forward();
        } else if k.erase_all.matches(key) {
            self.editor.erase_all();
        } else if k.erase_to_previous_word.matches(key) {
            self.editor.erase_to_previous_word(breaks);
        } else if k.erase_to_next_word.matches(key) {
            self.editor.erase_to_next_word(breaks);
        } else if k.erase_to_tail.matches(key) {
            self.editor.erase_to_tail();
        } else {
            return false;
        }
        if self.editor.text() != before {
            self.on_text_changed();
        }
        true
    }

    /// Shared list navigation. Returns the new selection, or `None` if `key` is not a move.
    fn list_move(&self, key: &KeyEvent, selected: usize, len: usize, area: Rect) -> Option<usize> {
        let k = &self.config.keybinds.list;
        let last = len.saturating_sub(1);
        let page = usize::from(area.height.saturating_sub(2)).max(1);
        if k.up.matches(key) {
            Some(selected.saturating_sub(1))
        } else if k.down.matches(key) {
            Some((selected + 1).min(last))
        } else if k.page_up.matches(key) {
            Some(selected.saturating_sub(page))
        } else if k.page_down.matches(key) {
            Some((selected + page).min(last))
        } else if k.top.matches(key) {
            Some(0)
        } else if k.bottom.matches(key) {
            Some(last)
        } else {
            None
        }
    }

    fn handle_suggestions_key(&mut self, key: &KeyEvent) -> bool {
        let config = self.config.clone();
        let k = &config.keybinds.list;
        let len = self.suggestions.items.len();

        if let Some(selected) =
            self.list_move(key, self.suggestions.selected, len, self.areas.suggestions)
        {
            self.suggestions.selected = selected;
            if self.is_previewing() {
                self.preview_selected_suggestion();
            }
            return true;
        }
        if k.select.matches(key) {
            self.suggestions.preview_origin = None;
            if let Some(query) = self.suggestions.selected_query() {
                self.set_query(&query);
            }
            self.set_focus(Panel::Query);
            return true;
        }
        if k.back.matches(key) {
            if let Some(origin) = self.suggestions.preview_origin.take() {
                self.editor.set_text(&origin);
                self.refresh_suggestions();
                self.schedule_evaluation();
            }
            self.set_focus(Panel::Query);
            return true;
        }
        // While completing, any other key keeps the suggestion and goes on editing the query.
        if self.is_previewing() && !self.config.keybinds.global.help.matches(key) {
            self.set_focus(Panel::Query);
            if !self.handle_query_key(key) {
                if let crossterm::event::KeyCode::Char(ch) = key.code {
                    if is_text_input(key.code, key.modifiers) {
                        self.editor.insert(ch);
                        self.on_text_changed();
                    }
                }
            }
            return true;
        }
        false
    }

    fn handle_history_key(&mut self, key: &KeyEvent) -> bool {
        let config = self.config.clone();
        let k = &config.keybinds.list;
        let len = self.history.entries().len();

        if let Some(selected) = self.list_move(key, self.history.selected, len, self.areas.history)
        {
            self.history.selected = selected;
            return true;
        }
        if k.select.matches(key) {
            if let Some(query) = self.history.entries().get(self.history.selected).cloned() {
                self.set_query(&query);
                self.evaluate_now();
            }
            self.set_focus(Panel::Query);
            return true;
        }
        if k.delete.matches(key) {
            self.history.remove_selected();
            return true;
        }
        if k.back.matches(key) {
            self.set_focus(Panel::Query);
            return true;
        }
        false
    }

    fn result_page(&self) -> usize {
        (usize::from(self.areas.result.height.saturating_sub(2)) / 2).max(1)
    }

    fn handle_result_key(&mut self, key: &KeyEvent) -> bool {
        let config = self.config.clone();
        let k = &config.keybinds.result;
        let page = self.result_page();
        let tree = &mut self.result.tree;

        if k.up.matches(key) {
            tree.up(1);
        } else if k.down.matches(key) {
            tree.down(1);
        } else if k.page_up.matches(key) {
            tree.up(page);
        } else if k.page_down.matches(key) {
            tree.down(page);
        } else if k.top.matches(key) {
            tree.top();
        } else if k.bottom.matches(key) {
            tree.bottom();
        } else if k.toggle.matches(key) {
            tree.toggle();
        } else if k.expand.matches(key) {
            tree.expand();
        } else if k.collapse.matches(key) {
            tree.collapse();
        } else if k.expand_all.matches(key) {
            tree.set_all_collapsed(false);
        } else if k.collapse_all.matches(key) {
            tree.set_all_collapsed(true);
        } else if k.drill_down.matches(key) {
            self.drill_down();
        } else if k.copy_menu.matches(key) {
            self.open_copy_menu();
        } else if k.back.matches(key) {
            self.set_focus(Panel::Query);
        } else {
            return false;
        }
        true
    }

    /// Narrow the query down to the node under the cursor.
    fn drill_down(&mut self) {
        let Some((_, path)) = self.result.tree.cursor_path() else {
            return;
        };
        if path.is_empty() {
            return;
        }
        let path = path_to_filter(&path);
        let base = self.result.query.trim();
        // With several outputs the path is applied to all of them, which is what
        // drilling into e.g. `.users[]` means.
        let query = if is_identity(base) {
            path
        } else {
            format!("{base} | {path}")
        };
        self.set_query(&query);
        self.evaluate_now();
    }

    fn open_copy_menu(&mut self) {
        let path = self
            .result
            .tree
            .cursor_path()
            .map(|(_, path)| path_to_filter(&path));
        let mut items = Vec::new();
        if let Some(path) = path {
            items.push(MenuItem {
                key: 'p',
                label: format!("Copy path of the selected node: {path}"),
                action: MenuAction::Path,
            });
            items.push(MenuItem {
                key: 'v',
                label: "Copy value of the selected node".into(),
                action: MenuAction::Value,
            });
        }
        items.push(MenuItem {
            key: 'r',
            label: "Copy the whole result".into(),
            action: MenuAction::Result,
        });
        items.push(MenuItem {
            key: 'f',
            label: format!("Copy the query: {}", self.result.query),
            action: MenuAction::Query,
        });
        self.popup = Some(Popup::Menu(Menu {
            title: " Copy to clipboard ",
            items,
            selected: 0,
        }));
    }

    fn copy(&mut self, action: MenuAction) {
        let indent = self.config.json.indent;
        let text = match action {
            MenuAction::Query => Some(self.editor.text()),
            MenuAction::Result => Some(self.formatted_result()),
            MenuAction::Path => self
                .result
                .tree
                .cursor_path()
                .map(|(_, path)| path_to_filter(&path)),
            MenuAction::Value => self.result.tree.cursor_value().map(|v| pretty(v, indent)),
        };
        let Some(text) = text else {
            return;
        };
        match Clipboard::new().and_then(|mut clipboard| clipboard.set_text(text)) {
            Ok(()) => self.notify("Copied to clipboard", Severity::Success),
            Err(e) => self.notify(format!("Failed to copy to clipboard: {e}"), Severity::Error),
        }
    }

    fn handle_popup_key(&mut self, key: &KeyEvent) {
        let config = self.config.clone();
        let k = &config.keybinds.popup;
        let Some(popup) = &mut self.popup else {
            return;
        };
        let is_help = matches!(popup, Popup::Help { .. });
        if k.close.matches(key) || (is_help && config.keybinds.global.help.matches(key)) {
            self.popup = None;
            return;
        }
        match popup {
            Popup::Help { scroll } => {
                if k.up.matches(key) {
                    *scroll = scroll.saturating_sub(1);
                } else if k.down.matches(key) {
                    *scroll += 1;
                }
            }
            Popup::Menu(menu) => {
                let chosen = if k.up.matches(key) {
                    menu.selected = menu.selected.saturating_sub(1);
                    None
                } else if k.down.matches(key) {
                    menu.selected = (menu.selected + 1).min(menu.items.len().saturating_sub(1));
                    None
                } else if k.select.matches(key) {
                    menu.items.get(menu.selected).map(|item| item.action)
                } else if let crossterm::event::KeyCode::Char(ch) = key.code {
                    menu.items
                        .iter()
                        .find(|item| item.key == ch)
                        .map(|item| item.action)
                } else {
                    None
                };
                if let Some(action) = chosen {
                    self.popup = None;
                    self.copy(action);
                }
            }
        }
    }

    fn panel_at(&self, column: u16, row: u16) -> Option<(Panel, Rect)> {
        let position = Position::new(column, row);
        [
            (Panel::Query, self.areas.query),
            (Panel::Suggestions, self.areas.suggestions),
            (Panel::History, self.areas.history),
            (Panel::Result, self.areas.result),
        ]
        .into_iter()
        .find(|(_, area)| area.contains(position))
    }

    fn handle_mouse(&mut self, mouse: MouseEvent) {
        if self.popup.is_some() {
            return;
        }
        let Some((panel, area)) = self.panel_at(mouse.column, mouse.row) else {
            return;
        };
        // Line inside the panel borders.
        let line = usize::from(mouse.row.saturating_sub(area.y + 1));
        match mouse.kind {
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                let down = mouse.kind == MouseEventKind::ScrollDown;
                match panel {
                    Panel::Result => {
                        if down {
                            self.result.tree.down(SCROLL_STEP)
                        } else {
                            self.result.tree.up(SCROLL_STEP)
                        }
                    }
                    Panel::Suggestions => {
                        let last = self.suggestions.items.len().saturating_sub(1);
                        let s = &mut self.suggestions.selected;
                        *s = if down {
                            (*s + SCROLL_STEP).min(last)
                        } else {
                            s.saturating_sub(SCROLL_STEP)
                        };
                    }
                    Panel::History => {
                        let last = self.history.entries().len().saturating_sub(1);
                        let s = &mut self.history.selected;
                        *s = if down {
                            (*s + SCROLL_STEP).min(last)
                        } else {
                            s.saturating_sub(SCROLL_STEP)
                        };
                    }
                    Panel::Query => {}
                }
            }
            MouseEventKind::Down(MouseButton::Left) => {
                self.set_focus(panel);
                let inside = mouse.row > area.y && mouse.row + 1 < area.y + area.height;
                if !inside {
                    return;
                }
                match panel {
                    Panel::Result => self.result.tree.select_line(line),
                    Panel::Suggestions => {
                        let index = self.suggestions_list.offset() + line;
                        if index < self.suggestions.items.len() {
                            self.suggestions.selected = index;
                        }
                    }
                    Panel::History => {
                        let index = self.history_list.offset() + line;
                        if index < self.history.entries().len() {
                            self.history.selected = index;
                        }
                    }
                    Panel::Query => {}
                }
            }
            _ => {}
        }
    }
}

/// Evaluate the query and build the resulting tree.
/// Returns `None` when the query was superseded by a newer one.
async fn evaluate(input: Arc<Vec<Value>>, worker: JqWorker, query: &str) -> Option<Outcome> {
    // Building the rows for a large result is CPU heavy, keep it off the async workers.
    let build = |values: Arc<Vec<Value>>| async move {
        tokio::task::spawn_blocking(move || JsonTree::new(values))
            .await
            .ok()
    };

    if is_identity(query) {
        // Skip jq (and the conversions to and from its value type) entirely.
        return build(input).await.map(Outcome::Tree);
    }

    match worker.run_filter(query.to_string()).await? {
        Ok(values) if values.is_empty() => Some(Outcome::Empty),
        Ok(values) if values.iter().all(Value::is_null) => Some(Outcome::AllNull),
        Ok(values) => build(Arc::new(values)).await.map(Outcome::Tree),
        Err(e) => Some(Outcome::Failed(e.to_string())),
    }
}
