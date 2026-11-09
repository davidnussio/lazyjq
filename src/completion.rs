use std::{collections::BTreeSet, sync::Arc};

use promkit_widgets::{
    core::{crossterm::event::Event, grapheme::StyledGraphemes, Widget},
    listbox::{self, Listbox},
    serde_json,
};
use tokio::{
    sync::{mpsc, Mutex, RwLock},
    task::{self, JoinHandle},
};

use crate::{
    config::CompletionKeybinds,
    context::{Index, SharedContext},
    guide::{GuideAction, GuideMessage},
    json,
    query_editor::QueryEditorAction,
    value_completion,
};

/// Maximum number of distinct values collected for value completion.
const MAX_VALUE_SUGGESTIONS: usize = 10_000;

/// Progress information for loading suggestions
#[derive(Clone, Default)]
pub struct SuggestionLoadProgress {
    pub is_complete: bool,
    pub loaded_path_count: usize,
}

/// Store for suggestions with thread-safe access
struct SuggestionStore {
    /// Set of all paths extracted from JSON input
    paths: BTreeSet<String>,
    progress: SuggestionLoadProgress,
}

#[derive(Clone)]
pub struct SharedSuggestionStore(Arc<Mutex<SuggestionStore>>);

impl SharedSuggestionStore {
    /// Collect suggestions that start with the given prefix
    pub async fn collect_matches(&self, prefix: &str) -> (Vec<String>, SuggestionLoadProgress) {
        let store = self.0.lock().await;
        let items = store
            .paths
            .iter()
            .filter(|p| p.starts_with(prefix))
            .cloned()
            .collect::<Vec<_>>();
        (items, store.progress.clone())
    }
}

/// Spawn a background loader and return shared suggestion store with task handle.
pub fn spawn_initialize(
    input: &'static str,
    max_streams: Option<usize>,
    chunk_size: usize,
) -> (SharedSuggestionStore, JoinHandle<()>) {
    let shared = SharedSuggestionStore(Arc::new(Mutex::new(SuggestionStore {
        paths: BTreeSet::new(),
        progress: SuggestionLoadProgress::default(),
    })));

    let shared_for_loading = shared.clone();
    let loader_task = task::spawn(async move {
        // Load paths in a streaming manner and update the shared store incrementally
        let iter = match json::get_all_paths(input, max_streams).await {
            Ok(iter) => iter,
            Err(_) => {
                let mut store = shared_for_loading.0.lock().await;
                store.progress.is_complete = true;
                return;
            }
        };

        // Process paths in chunks to avoid holding the lock for too long
        let mut batch = Vec::with_capacity(chunk_size);
        for path in iter {
            batch.push(path);

            if batch.len() >= chunk_size {
                let loaded = batch.len();
                let mut store = shared_for_loading.0.lock().await;
                for item in batch.drain(..) {
                    store.paths.insert(item);
                }
                store.progress.loaded_path_count += loaded;
            }
        }

        // Insert any remaining paths after the loop
        let remaining = batch.len();
        let mut store = shared_for_loading.0.lock().await;
        for item in batch {
            store.paths.insert(item);
        }

        // Mark loading as complete and update progress
        store.progress.loaded_path_count += remaining;
        store.progress.is_complete = true;
    });

    (shared, loader_task)
}

/// Navigator for managing the state of suggestions
/// and interactions in the completion view.
pub struct CompletionNavigator {
    shared_suggestions: SharedSuggestionStore,
    state: listbox::State,
    /// Number of suggestions to load in each chunk
    /// when the user scrolls near the end of the list.
    search_result_chunk_size: usize,
    /// Buffered suggestions that are not yet visible in the listbox.
    remaining_items: Vec<String>,
    /// Query text prepended to the selected item when replacing the query.
    /// Empty for path suggestions (items are whole queries); for value
    /// suggestions it holds the query up to the value (e.g. `select(.name == `).
    item_prefix: String,
    /// Parsed input JSON used to evaluate value suggestions.
    /// `None` until the JSON viewer has finished loading the input.
    source: Option<Arc<Vec<serde_json::Value>>>,
}

/// Outcome of starting a completion session.
enum EnterOutcome {
    Paths {
        head: Option<String>,
        progress: SuggestionLoadProgress,
    },
    Values {
        head: Option<String>,
        count: usize,
        truncated: bool,
    },
    ValuesUnavailable(String),
}

impl CompletionNavigator {
    pub fn new(
        shared_suggestions: SharedSuggestionStore,
        state: listbox::State,
        search_result_chunk_size: usize,
    ) -> Self {
        Self {
            shared_suggestions,
            state,
            search_result_chunk_size,
            remaining_items: Default::default(),
            item_prefix: Default::default(),
            source: None,
        }
    }

    /// Provide the parsed input JSON, enabling value suggestions.
    pub fn set_source(&mut self, source: Arc<Vec<serde_json::Value>>) {
        self.source = Some(source);
    }

    /// Get the query text corresponding to the currently selected item in listbox.
    fn get_current_item(&self) -> String {
        format!("{}{}", self.item_prefix, self.state.listbox.get())
    }

    /// Create graphemes for rendering the completion navigator.
    pub fn create_graphemes(&self, width: u16, height: u16) -> StyledGraphemes {
        self.state.create_graphemes(width, height)
    }

    /// Returns true when the cursor is close enough to the visible tail
    /// and preloading the next chunk is beneficial.
    fn is_near_visible_tail(&self) -> bool {
        self.state
            .listbox
            .len()
            .saturating_sub(self.state.listbox.position())
            < self.state.config.lines.unwrap_or(1)
    }

    fn move_down(&mut self) {
        // First, move the cursor down by one item.
        self.state.listbox.forward();

        // Then, check if we need to load more items
        // when the cursor is close to the end.
        if self.is_near_visible_tail() {
            self.append_next_chunk_if_needed();
        }
    }

    fn append_next_chunk_if_needed(&mut self) {
        if self.remaining_items.is_empty() {
            return;
        }
        let items = self.remaining_items.drain(
            ..self
                .search_result_chunk_size
                .min(self.remaining_items.len()),
        );
        for item in items {
            self.state.listbox.push_string(item);
        }
    }

    /// Handle a user input event to update the completion navigator's state accordingly.
    /// Returns `Some(String)` if the event triggers a selection change that should update the query editor,
    fn handle_user_event(
        &mut self,
        event: &Event,
        completion_keybinds: &CompletionKeybinds,
    ) -> Option<String> {
        if self.state.listbox.is_empty() {
            return None;
        }

        // Move up.
        if completion_keybinds.up.contains(event) {
            self.state.listbox.backward();
            return Some(self.get_current_item());
        }

        // Move down (and load more if near the end).
        if completion_keybinds.down.contains(event) {
            self.move_down();
            return Some(self.get_current_item());
        }

        None
    }

    async fn enter(&mut self, prefix: &str) -> EnterOutcome {
        if let Some(request) = value_completion::parse(prefix) {
            return self.enter_values(request).await;
        }

        let (items, progress) = self.shared_suggestions.collect_matches(prefix).await;
        let head = self.initialize_session_items(items, String::new());
        EnterOutcome::Paths { head, progress }
    }

    /// Start a session suggesting the distinct values produced by the
    /// left-hand side of the comparison being typed.
    async fn enter_values(
        &mut self,
        request: value_completion::ValueCompletionRequest,
    ) -> EnterOutcome {
        self.clear_session_state();

        let Some(source) = self.source.clone() else {
            return EnterOutcome::ValuesUnavailable("input is still loading".to_string());
        };

        let query = request.source_query.clone();
        let collected = task::spawn_blocking(move || {
            json::collect_scalar_values(&query, &source, MAX_VALUE_SUGGESTIONS)
        })
        .await;

        let values = match collected {
            Ok(Ok(values)) => values,
            Ok(Err(e)) => return EnterOutcome::ValuesUnavailable(e.to_string()),
            Err(e) => return EnterOutcome::ValuesUnavailable(e.to_string()),
        };
        let truncated = values.len() >= MAX_VALUE_SUGGESTIONS;

        let items = values
            .into_iter()
            .filter(|v| request.accepts(v))
            .collect::<Vec<_>>();
        let count = items.len();
        let head = self.initialize_session_items(items, request.head);
        EnterOutcome::Values {
            head,
            count,
            truncated,
        }
    }

    /// Initialize a completion session with a new search result set.
    /// This method always resets previous session state first.
    fn initialize_session_items(
        &mut self,
        mut items: Vec<String>,
        item_prefix: String,
    ) -> Option<String> {
        self.clear_session_state();
        self.item_prefix = item_prefix;

        if items.is_empty() {
            return None;
        }

        let used = items
            .drain(..self.search_result_chunk_size.min(items.len()))
            .collect::<Vec<_>>();
        self.remaining_items = items;
        self.state.listbox = Listbox::from(used);
        Some(self.get_current_item())
    }

    /// Reset completion session state.
    /// This clears both visible list items and buffered remaining items.
    fn clear_session_state(&mut self) {
        self.state.listbox = Listbox::from(Vec::<String>::new());
        self.remaining_items.clear();
        self.item_prefix.clear();
    }
}

pub enum CompletionAction {
    /// Triggered when the user enters the completion view with a current query as prefix.
    Enter { prefix: String },
    /// Triggered when the user leaves the completion view.
    Leave,
    /// Triggered on user input events within the completion view, such as navigation keys.
    UserEvent(Event),
}

/// Spawn a background task to manage the completion navigator's state and interactions.
pub fn start_completion_task(
    mut action_rx: mpsc::Receiver<CompletionAction>,
    shared_ctx: SharedContext,
    shared_completion: Arc<RwLock<CompletionNavigator>>,
    shared_renderer: promkit_widgets::core::render::SharedRenderer<Index>,
    query_editor_action_tx: mpsc::Sender<QueryEditorAction>,
    guide_action_tx: mpsc::Sender<GuideAction>,
    completion_keybinds: CompletionKeybinds,
) -> JoinHandle<anyhow::Result<()>> {
    tokio::spawn(async move {
        loop {
            tokio::select! {
                Some(action) = action_rx.recv() => {
                    let area = shared_ctx.area().await;
                    let completion_view = {
                        let mut completion = shared_completion.write().await;
                        match action {
                            CompletionAction::Enter { prefix } => {
                                let (head_item, message) = match completion.enter(&prefix).await {
                                    EnterOutcome::Paths { head, progress } => {
                                        let message = if progress.is_complete {
                                            GuideMessage::LoadedAllSuggestions(progress.loaded_path_count)
                                        } else {
                                            GuideMessage::LoadedPartiallySuggestions(progress.loaded_path_count)
                                        };
                                        (head, message)
                                    }
                                    EnterOutcome::Values { head, count, truncated } => {
                                        (head, GuideMessage::LoadedValueSuggestions { count, truncated })
                                    }
                                    EnterOutcome::ValuesUnavailable(reason) => {
                                        (None, GuideMessage::ValueSuggestionsUnavailable(reason))
                                    }
                                };
                                match head_item {
                                    Some(head) => {
                                        guide_action_tx.send(GuideAction::Show(message)).await?;
                                        query_editor_action_tx
                                            .send(QueryEditorAction::ReplaceText(head))
                                            .await?;
                                    }
                                    None => {
                                        let message = match message {
                                            GuideMessage::ValueSuggestionsUnavailable(_) => message,
                                            _ => GuideMessage::NoSuggestionFound(prefix),
                                        };
                                        guide_action_tx.send(GuideAction::Show(message)).await?;
                                        shared_ctx.set_active_index(Index::QueryEditor).await;
                                        completion.clear_session_state();
                                    }
                                }
                            }
                            CompletionAction::UserEvent(event) => {
                                if let Some(text) = completion.handle_user_event(&event, &completion_keybinds) {
                                    query_editor_action_tx
                                        .send(QueryEditorAction::ReplaceText(text))
                                        .await?;
                                } else {
                                    shared_ctx.set_active_index(Index::QueryEditor).await;
                                    completion.clear_session_state();
                                    query_editor_action_tx
                                        .send(QueryEditorAction::UserEvent(event))
                                        .await?;
                                }
                            }
                            CompletionAction::Leave => {
                                completion.clear_session_state();
                            }
                        }
                        completion.create_graphemes(area.0, area.1)
                    };

                    shared_renderer
                        .update([(Index::Completion, completion_view)])
                        .render()
                        .await?;
                }
                else => break,
            }
        }
        Ok(())
    })
}
