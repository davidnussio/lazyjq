//! Application state and event handling.
//!
//! Everything runs on a single loop: terminal events, results of background
//! work (jq evaluation, value suggestions, path indexing) and timers update
//! the state, then the whole screen is drawn again.

use std::{path::PathBuf, sync::Arc, time::Duration};

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
    builtins,
    config::{keys::is_text_input, Config, QueryKeybinds},
    editor::LineEditor,
    history::History,
    jq_worker::JqWorker,
    json_tree::{formatted_values, path_to_filter, pretty, JsonTree},
    save,
    suggestions::{PathIndex, Suggestions},
    ui, value_completion,
};

/// Number of paths published at once by the background indexer.
const PATH_LOAD_CHUNK: usize = 50_000;
/// How long a message stays in the bottom bar.
const TOAST_DURATION: Duration = Duration::from_secs(4);
/// Lines moved per mouse wheel step.
const SCROLL_STEP: usize = 3;
/// Word boundaries when editing a file name.
const PATH_BREAKS: [char; 5] = ['/', '.', '-', '_', ' '];

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
    Save,
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

/// What confirming a [`Prompt`] does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PromptAction {
    SaveResult,
}

/// A popup asking for one line of text, e.g. a file name.
pub struct Prompt {
    pub title: &'static str,
    pub action: PromptAction,
    pub editor: LineEditor,
    /// Why the text was not accepted (shown under it until it is edited).
    pub message: Option<(String, Severity)>,
    /// Text whose confirmation would overwrite an existing file: confirming
    /// the same text again does it.
    overwrite: Option<String>,
    /// File names offered by the last completion.
    pub completion: Option<Completion>,
}

/// File names matching the one being typed in a [`Prompt`].
pub struct Completion {
    /// Text before the completed name.
    base: String,
    /// Text before going through the candidates, restored when going back.
    original: String,
    pub candidates: Vec<String>,
    /// Candidate put in the text while going through them.
    pub selected: Option<usize>,
}

pub enum Popup {
    Help { scroll: usize },
    Menu(Menu),
    Prompt(Prompt),
}

/// What the Result panel shows.
pub struct ResultView {
    pub tree: JsonTree,
    /// The query that produced `tree`.
    pub query: String,
}

/// Documentation of the selected builtin, shown in place of History while
/// the Suggestions panel lists functions.
#[derive(Default)]
pub struct DocsView {
    /// Builtin the scroll position belongs to.
    pub name: String,
    pub scroll: usize,
    /// Largest useful scroll position, from the last draw.
    pub max_scroll: usize,
}

/// A result left by drilling down into one of its nodes, to come back to.
pub struct DrillStep {
    pub query: String,
    /// The tree as it was left (cursor and folds), once the drilled-down result
    /// has replaced it. `None` for the input itself (kept in `input_tree`) or
    /// while the drilled-down query is still being evaluated.
    tree: Option<JsonTree>,
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
    Saved {
        path: PathBuf,
        /// Number of bytes written.
        result: std::io::Result<usize>,
    },
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
    pub docs: DocsView,
    pub toast: Option<Toast>,
    pub evaluating: bool,
    pub spinner: usize,
    pub areas: Areas,
    pub paths: PathIndex,
    /// Results to go back to, one per drill-down (most recent last).
    pub drill_stack: Vec<DrillStep>,

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
    /// The input file, `None` for stdin.
    source: Option<PathBuf>,
    /// File name last confirmed in the save prompt, proposed again next time.
    last_save_path: Option<String>,
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
        source: Option<PathBuf>,
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
            docs: DocsView::default(),
            toast: None,
            evaluating: false,
            spinner: 0,
            areas: Areas::default(),
            paths,
            drill_stack: Vec::new(),
            input,
            worker,
            tx,
            generation: 0,
            eval_deadline: None,
            requested_query: ".".into(),
            pending_history: None,
            input_tree: None,
            values_in_flight: None,
            source,
            last_save_path: None,
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
        } else if let Some(step) = self
            .drill_stack
            .last_mut()
            .filter(|step| step.tree.is_none() && step.query == previous.query)
        {
            // The result we drilled down from: keep it as it was left.
            step.tree = Some(previous.tree);
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
            AppEvent::Saved { path, result } => match result {
                Ok(bytes) => self.notify(
                    format!("Saved {} to {}", save::human_size(bytes), path.display()),
                    Severity::Success,
                ),
                Err(e) => self.notify(
                    format!("Failed to save {}: {e}", path.display()),
                    Severity::Error,
                ),
            },
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
            let limit = self.config.completion.max_suggestions;
            let (items, truncated) = self.paths.matches(text.trim_start(), limit);
            if items.is_empty() {
                if let Some((head, functions, truncated)) = builtins::complete(&text, limit) {
                    if !functions.is_empty() {
                        self.suggestions.set_functions(head, functions, truncated);
                        return;
                    }
                }
            }
            self.suggestions
                .set_paths(items, truncated, !self.paths.is_complete());
        }
    }

    /// The builtin whose documentation panel 3 shows instead of History:
    /// the one selected in Suggestions while a function name is being typed or
    /// the list is browsed, otherwise the one whose arguments are being typed.
    pub fn docs_function(&self) -> Option<String> {
        let text = self.editor.text();
        let enclosing = builtins::enclosing_call(&text);
        let selected = self.suggestions.selected_function();
        let typing_name = text.len() > self.suggestions.head.len();
        let browsing = self.focus == Panel::Suggestions || self.is_previewing();
        match (selected, enclosing) {
            (Some(name), Some(_)) if typing_name || browsing => Some(name.to_string()),
            (_, Some(name)) => Some(name.to_string()),
            (selected, None) => selected.map(str::to_string),
        }
    }

    /// Whether panel 3 shows the documentation of a builtin instead of History.
    pub fn showing_docs(&self) -> bool {
        self.docs_function().is_some()
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
            Event::Paste(text) => {
                let chars = text.chars().filter(|c| !c.is_control());
                match &mut self.popup {
                    Some(Popup::Prompt(prompt)) => {
                        chars.for_each(|ch| prompt.editor.insert(ch));
                        prompt.edited();
                    }
                    None if self.focus == Panel::Query => {
                        chars.for_each(|ch| self.editor.insert(ch));
                        self.on_text_changed();
                    }
                    _ => {}
                }
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
        } else if global.save_result.matches(&key) {
            self.open_save_prompt();
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
        } else if !edit_line(&mut self.editor, k, key, breaks) {
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
        if self.showing_docs() {
            return self.handle_docs_key(key);
        }
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

    fn handle_docs_key(&mut self, key: &KeyEvent) -> bool {
        let k = &self.config.keybinds.list;
        if k.select.matches(key) || k.back.matches(key) {
            self.set_focus(Panel::Query);
            return true;
        }
        // `list_move` clamps to `len - 1`: positions are scroll offsets.
        let len = self.docs.max_scroll + 1;
        if let Some(scroll) = self.list_move(key, self.docs.scroll, len, self.areas.history) {
            self.docs.scroll = scroll;
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
        } else if k.drill_up.matches(key) {
            self.drill_up();
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
        self.drill_stack.push(DrillStep {
            query: self.result.query.clone(),
            tree: None,
        });
        self.set_query(&query);
        self.evaluate_now();
    }

    /// Go back to the result the last drill-down started from,
    /// with its cursor and folds as they were left.
    fn drill_up(&mut self) {
        let Some(step) = self.drill_stack.pop() else {
            self.notify("Nothing to go back to", Severity::Warning);
            return;
        };
        let tree = step.tree.or_else(|| {
            is_identity(&step.query)
                .then(|| self.input_tree.take())
                .flatten()
        });

        self.editor.set_text(&step.query);
        self.history.reset_browse();
        self.refresh_suggestions();
        match tree {
            Some(tree) => {
                // Shown as is: drop any evaluation still running.
                self.eval_deadline = None;
                self.generation += 1;
                self.requested_query = step.query.clone();
                self.pending_history = None;
                self.evaluating = false;
                self.query_status = None;
                self.empty_result = false;
                self.result = ResultView {
                    tree,
                    query: step.query,
                };
            }
            // The tree is not available (e.g. the drill-down was undone before
            // its result arrived): evaluate the query again.
            None => self.evaluate_now(),
        }
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
        items.push(MenuItem {
            key: 's',
            label: "Save the result to a file…".into(),
            action: MenuAction::Save,
        });
        self.popup = Some(Popup::Menu(Menu {
            title: " Copy or save ",
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
            MenuAction::Save => {
                self.open_save_prompt();
                return;
            }
        };
        let Some(text) = text else {
            return;
        };
        match Clipboard::new().and_then(|mut clipboard| clipboard.set_text(text)) {
            Ok(()) => self.notify("Copied to clipboard", Severity::Success),
            Err(e) => self.notify(format!("Failed to copy to clipboard: {e}"), Severity::Error),
        }
    }

    // --- Saving ------------------------------------------------------------

    /// Ask for the file to save the result to.
    fn open_save_prompt(&mut self) {
        let path = self
            .last_save_path
            .clone()
            .unwrap_or_else(|| save::default_path(self.source.as_deref()));
        self.popup = Some(Popup::Prompt(Prompt {
            title: " Save the result to ",
            action: PromptAction::SaveResult,
            editor: LineEditor::new(&path),
            message: None,
            overwrite: None,
            completion: None,
        }));
    }

    fn confirm_prompt(&mut self) {
        let Some(Popup::Prompt(prompt)) = &mut self.popup else {
            return;
        };
        match prompt.action {
            PromptAction::SaveResult => self.confirm_save(),
        }
    }

    fn confirm_save(&mut self) {
        let confirm = self
            .config
            .keybinds
            .popup
            .select
            .display()
            .unwrap_or("Enter")
            .to_string();
        let Some(Popup::Prompt(prompt)) = &mut self.popup else {
            return;
        };
        let text = prompt.editor.text().trim().to_string();
        if text.is_empty() {
            prompt.message = Some(("Type the name of the file".into(), Severity::Warning));
            return;
        }
        let path = save::resolve_path(&text);
        if path.is_dir() {
            prompt.message = Some(("This is a directory".into(), Severity::Error));
            return;
        }
        if path.exists() && prompt.overwrite.as_deref() != Some(text.as_str()) {
            prompt.message = Some((
                format!("The file exists: {confirm} again to overwrite it"),
                Severity::Warning,
            ));
            prompt.overwrite = Some(text);
            return;
        }

        self.popup = None;
        self.last_save_path = Some(text);
        let values = self.result.tree.values().clone();
        let indent = self.config.json.indent;
        let tx = self.tx.clone();
        // Formatting and writing a large result takes a while: keep it off the UI.
        tokio::task::spawn_blocking(move || {
            let mut text = formatted_values(&values, indent);
            if !text.is_empty() {
                text.push('\n');
            }
            let result = save::write_atomic(&path, text.as_bytes()).map(|()| text.len());
            let _ = tx.send(AppEvent::Saved { path, result });
        });
    }

    fn handle_prompt_key(&mut self, key: &KeyEvent) {
        let config = self.config.clone();
        let k = &config.keybinds.popup;
        let complete = &config.keybinds.query.complete;
        let list = &config.keybinds.list;
        // Printable keys are typed, whatever they are bound to.
        let typing = is_text_input(key.code, key.modifiers);
        let Some(Popup::Prompt(prompt)) = &mut self.popup else {
            return;
        };

        if let Some(completion) = &prompt.completion {
            if !typing && k.close.matches(key) {
                // Back to the text before going through the candidates.
                if completion.selected.is_some() {
                    let original = completion.original.clone();
                    prompt.editor.set_text(&original);
                }
                prompt.completion = None;
                return;
            }
            if !typing && k.select.matches(key) && completion.selected.is_some() {
                // Keep the candidate: Tab then completes inside a directory.
                prompt.completion = None;
                return;
            }
            if !typing && (complete.matches(key) || list.down.matches(key)) {
                prompt.cycle(true);
                return;
            }
            if !typing && list.up.matches(key) {
                prompt.cycle(false);
                return;
            }
        }

        if !typing && k.close.matches(key) {
            self.popup = None;
            return;
        }
        if !typing && k.select.matches(key) {
            prompt.completion = None;
            self.confirm_prompt();
            return;
        }
        if !typing && complete.matches(key) {
            prompt.complete();
            return;
        }
        let before = prompt.editor.text();
        if !edit_line(
            &mut prompt.editor,
            &config.keybinds.query,
            key,
            &PATH_BREAKS,
        ) && typing
        {
            if let crossterm::event::KeyCode::Char(ch) = key.code {
                prompt.editor.insert(ch);
            }
        }
        if prompt.editor.text() != before {
            prompt.edited();
        }
    }

    fn handle_popup_key(&mut self, key: &KeyEvent) {
        if matches!(self.popup, Some(Popup::Prompt(_))) {
            self.handle_prompt_key(key);
            return;
        }
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
            Popup::Prompt(_) => {}
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
                    Panel::History if self.showing_docs() => {
                        let d = &mut self.docs;
                        d.scroll = if down {
                            (d.scroll + SCROLL_STEP).min(d.max_scroll)
                        } else {
                            d.scroll.saturating_sub(SCROLL_STEP)
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
                    Panel::History if self.showing_docs() => {}
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

impl Prompt {
    /// The text changed: what was said about the previous one no longer applies.
    fn edited(&mut self) {
        self.message = None;
        self.overwrite = None;
        self.completion = None;
    }

    /// Complete the file name being typed: a single match is put in the
    /// text, several are listed after putting in their common beginning, or
    /// gone through when there is none to add.
    fn complete(&mut self) {
        let text = self.editor.text();
        let (base, candidates) = save::complete_path(&text);
        self.message = None;
        self.overwrite = None;
        self.completion = None;
        match candidates.as_slice() {
            [] => self.message = Some(("No file matches".into(), Severity::Warning)),
            [only] => self.editor.set_text(&format!("{base}{only}")),
            _ => {
                let common = save::common_prefix(&candidates);
                let typed = &text[base.len()..];
                let extends = common.chars().count() > typed.chars().count();
                if extends {
                    self.editor.set_text(&format!("{base}{common}"));
                }
                self.completion = Some(Completion {
                    base,
                    original: self.editor.text(),
                    candidates,
                    selected: None,
                });
                if !extends {
                    self.cycle(true);
                }
            }
        }
    }

    /// Put the next (or previous) candidate in the text.
    fn cycle(&mut self, forward: bool) {
        let Some(c) = &mut self.completion else {
            return;
        };
        let len = c.candidates.len();
        let next = match (c.selected, forward) {
            (None, true) => 0,
            (None, false) => len - 1,
            (Some(i), true) => (i + 1) % len,
            (Some(i), false) => (i + len - 1) % len,
        };
        c.selected = Some(next);
        self.editor
            .set_text(&format!("{}{}", c.base, c.candidates[next]));
    }
}

/// Apply an editing key (cursor moves and deletions) to a line editor.
/// Returns whether `key` is one of them.
fn edit_line(editor: &mut LineEditor, k: &QueryKeybinds, key: &KeyEvent, breaks: &[char]) -> bool {
    if k.backward.matches(key) {
        editor.backward();
    } else if k.forward.matches(key) {
        editor.forward();
    } else if k.move_to_head.matches(key) {
        editor.move_to_head();
    } else if k.move_to_tail.matches(key) {
        editor.move_to_tail();
    } else if k.move_to_previous_word.matches(key) {
        editor.move_to_previous_word(breaks);
    } else if k.move_to_next_word.matches(key) {
        editor.move_to_next_word(breaks);
    } else if k.erase.matches(key) {
        editor.erase();
    } else if k.erase_forward.matches(key) {
        editor.erase_forward();
    } else if k.erase_all.matches(key) {
        editor.erase_all();
    } else if k.erase_to_previous_word.matches(key) {
        editor.erase_to_previous_word(breaks);
    } else if k.erase_to_next_word.matches(key) {
        editor.erase_to_next_word(breaks);
    } else if k.erase_to_tail.matches(key) {
        editor.erase_to_tail();
    } else {
        return false;
    }
    true
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::suggestions::SuggestionKind;

    async fn app(json: &str) -> (App, mpsc::UnboundedReceiver<AppEvent>) {
        let config = Config::load_from("[history]\npersist = false").unwrap();
        let input = Arc::new(crate::json::deserialize(json, None).unwrap());
        let (tx, rx) = mpsc::unbounded_channel();
        (App::new(config, input, None, None, tx), rx)
    }

    /// Process background events until the pending evaluation is applied.
    async fn settle(app: &mut App, rx: &mut mpsc::UnboundedReceiver<AppEvent>) {
        while app.evaluating {
            let event = rx.recv().await.unwrap();
            app.handle_app_event(event);
        }
    }

    const SAMPLE: &str = r#"{"users":[{"name":"Al","tags":["a","b"]}],"n":1}"#;

    #[tokio::test]
    async fn drill_up_restores_previous_results() {
        let (mut app, mut rx) = app(SAMPLE).await;

        app.result.tree.down(1); // "users": [
        app.drill_down();
        settle(&mut app, &mut rx).await;
        assert_eq!(app.result.query, ".users");

        app.result.tree.down(1); // { (users[0])
        app.result.tree.toggle(); // folded, to check it is kept
        app.result.tree.down(1); // ] (end of users)
        app.result.tree.up(1); // back on the folded object
        app.drill_down();
        settle(&mut app, &mut rx).await;
        assert_eq!(app.result.query, ".users | .[0]");
        assert_eq!(app.editor.text(), ".users | .[0]");

        app.drill_up();
        assert_eq!(app.result.query, ".users");
        assert_eq!(app.editor.text(), ".users");
        assert_eq!(app.result.tree.cursor(), 1);
        assert_eq!(
            app.result
                .tree
                .render(5, &app.config.json, Default::default())
                .0[1]
                .to_string(),
            "  {…} 2 keys"
        );

        app.drill_up();
        assert_eq!(app.result.query, ".");
        assert_eq!(app.result.tree.cursor(), 1);
        assert!(app.drill_stack.is_empty());

        app.drill_up();
        assert!(app.toast.is_some());
    }

    #[tokio::test]
    async fn functions_are_suggested_when_no_path_matches() {
        let (mut app, _rx) = app(SAMPLE).await;
        app.paths
            .spawn_loader(app.input.clone(), 10, || {})
            .join()
            .unwrap();

        app.set_query(".users");
        assert_eq!(app.suggestions.kind, SuggestionKind::Paths);
        assert!(!app.showing_docs());

        app.set_query(".users[0] | sel");
        assert_eq!(app.suggestions.kind, SuggestionKind::Functions);
        assert_eq!(app.suggestions.items, vec!["select"]);
        assert_eq!(app.suggestions.selected_function(), Some("select"));
        assert!(app.showing_docs());
        assert_eq!(
            app.suggestions.selected_query().as_deref(),
            Some(".users[0] | select")
        );

        app.set_query(".users | map(");
        assert!(app.suggestions.items.len() > 50);
        // The documentation stays on the function whose arguments are typed...
        assert_eq!(app.docs_function().as_deref(), Some("map"));
        app.set_query(".users | map(.name == \"A");
        assert_eq!(app.docs_function().as_deref(), Some("map"));
        // ...unless another function name is being typed.
        app.set_query(".users | map(sel");
        assert_eq!(app.docs_function().as_deref(), Some("select"));
        app.set_query(".users | map(select(.n) | len");
        assert_eq!(app.docs_function().as_deref(), Some("length"));
        app.set_query(".users | map(.name)");
        assert_eq!(app.docs_function(), None);

        app.set_query(".nope");
        assert_eq!(app.suggestions.kind, SuggestionKind::Paths);
        assert!(!app.showing_docs());
    }

    fn press(app: &mut App, code: crossterm::event::KeyCode) {
        use crossterm::event::KeyModifiers;
        app.handle_key(KeyEvent::new(code, KeyModifiers::NONE));
    }

    fn type_text(app: &mut App, text: &str) {
        for ch in text.chars() {
            press(app, crossterm::event::KeyCode::Char(ch));
        }
    }

    /// Process background events until the save is reported.
    async fn until_saved(app: &mut App, rx: &mut mpsc::UnboundedReceiver<AppEvent>) {
        loop {
            let event = rx.recv().await.unwrap();
            let saved = matches!(event, AppEvent::Saved { .. });
            app.handle_app_event(event);
            if saved {
                return;
            }
        }
    }

    fn prompt(app: &App) -> &Prompt {
        match &app.popup {
            Some(Popup::Prompt(prompt)) => prompt,
            _ => panic!("no prompt open"),
        }
    }

    #[tokio::test]
    async fn saves_the_result_to_a_file() {
        use crossterm::event::{KeyCode, KeyModifiers};

        let dir = std::env::temp_dir().join(format!("lazyjq-app-save-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("out.json");
        std::fs::write(&file, "old").unwrap();

        let (mut app, mut rx) = app(r#"{"a":1} {"a":2}"#).await;
        app.set_query(".a");
        app.evaluate_now();
        settle(&mut app, &mut rx).await;

        app.handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
        assert_eq!(prompt(&app).editor.text(), "result.json");

        // Printable keys bound to popup actions (`q` closes) are typed.
        app.handle_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        type_text(&mut app, "q");
        assert_eq!(prompt(&app).editor.text(), "q");
        press(&mut app, KeyCode::Backspace);
        type_text(&mut app, &file.display().to_string());

        // An existing file is only overwritten when confirmed twice.
        press(&mut app, KeyCode::Enter);
        assert!(prompt(&app).message.is_some());
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "old");
        press(&mut app, KeyCode::Enter);
        assert!(app.popup.is_none());

        until_saved(&mut app, &mut rx).await;
        assert_eq!(app.toast.as_ref().unwrap().severity, Severity::Success);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "1\n2\n");

        // The path is proposed again, and Esc cancels.
        app.handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
        assert_eq!(prompt(&app).editor.text(), file.display().to_string());
        press(&mut app, KeyCode::Esc);
        assert!(app.popup.is_none());

        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn save_errors_are_reported() {
        let (mut app, mut rx) = app(SAMPLE).await;
        app.open_save_prompt();
        app.handle_key(KeyEvent::new(
            crossterm::event::KeyCode::Char('u'),
            crossterm::event::KeyModifiers::CONTROL,
        ));
        press(&mut app, crossterm::event::KeyCode::Enter);
        assert_eq!(prompt(&app).message.as_ref().unwrap().1, Severity::Warning);

        type_text(&mut app, "/nonexistent-lazyjq-dir/out.json");
        press(&mut app, crossterm::event::KeyCode::Enter);
        until_saved(&mut app, &mut rx).await;
        assert_eq!(app.toast.as_ref().unwrap().severity, Severity::Error);
    }

    #[tokio::test]
    async fn completes_the_file_name() {
        use crossterm::event::{KeyCode, KeyModifiers};

        let dir = std::env::temp_dir().join(format!("lazyjq-app-complete-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("data")).unwrap();
        std::fs::write(dir.join("data").join("users.json"), "").unwrap();
        std::fs::write(dir.join("dump-a.json"), "").unwrap();
        std::fs::write(dir.join("dump-b.json"), "").unwrap();
        let base = format!("{}/", dir.display());
        let text = |app: &App| prompt(app).editor.text();

        let (mut app, _rx) = app(SAMPLE).await;
        app.open_save_prompt();
        app.handle_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        type_text(&mut app, &format!("{base}da"));

        // A single match is completed, into the directory.
        press(&mut app, KeyCode::Tab);
        assert_eq!(text(&app), format!("{base}data/"));
        assert!(prompt(&app).completion.is_none());
        press(&mut app, KeyCode::Tab);
        assert_eq!(text(&app), format!("{base}data/users.json"));

        // Several matches: their common beginning, then each of them.
        app.handle_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        type_text(&mut app, &format!("{base}du"));
        press(&mut app, KeyCode::Tab);
        assert_eq!(text(&app), format!("{base}dump-"));
        assert_eq!(prompt(&app).completion.as_ref().unwrap().selected, None);
        press(&mut app, KeyCode::Tab);
        assert_eq!(text(&app), format!("{base}dump-a.json"));
        press(&mut app, KeyCode::Tab);
        assert_eq!(text(&app), format!("{base}dump-b.json"));
        press(&mut app, KeyCode::BackTab);
        assert_eq!(text(&app), format!("{base}dump-a.json"));

        // The matches are listed, the selected one with its own footer.
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 30)).unwrap();
        terminal.draw(|frame| ui::draw(frame, &mut app)).unwrap();
        let screen: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(screen.contains("dump-a.json  dump-b.json"));
        assert!(screen.contains("tab: next | enter: choose | esc: back"));

        // Esc undoes going through the matches, without closing the prompt.
        press(&mut app, KeyCode::Esc);
        assert_eq!(text(&app), format!("{base}dump-"));
        assert!(prompt(&app).completion.is_none());

        // With nothing to add, the first Tab already goes through the matches;
        // Enter keeps the match instead of saving.
        press(&mut app, KeyCode::Tab);
        assert_eq!(text(&app), format!("{base}dump-a.json"));
        press(&mut app, KeyCode::Enter);
        assert!(app.popup.is_some());
        assert!(prompt(&app).completion.is_none());

        // No match is reported until the text changes.
        type_text(&mut app, "zz");
        press(&mut app, KeyCode::Tab);
        assert_eq!(prompt(&app).message.as_ref().unwrap().1, Severity::Warning);
        type_text(&mut app, "x");
        assert!(prompt(&app).message.is_none());

        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn drill_up_before_the_result_arrives() {
        let (mut app, mut rx) = app(SAMPLE).await;
        app.result.tree.down(1);
        app.drill_down();
        app.drill_up();
        assert_eq!(app.result.query, ".");
        // The late result of the abandoned drill-down is ignored.
        while let Ok(event) = tokio::time::timeout(Duration::from_millis(200), rx.recv()).await {
            app.handle_app_event(event.unwrap());
        }
        assert_eq!(app.result.query, ".");
        assert!(!app.evaluating);
    }
}
