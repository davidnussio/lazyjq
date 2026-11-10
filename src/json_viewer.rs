use std::sync::Arc;

use promkit_widgets::{
    core::{crossterm::event::Event, grapheme::StyledGraphemes, render::SharedRenderer, Widget},
    jsonstream::{self, JsonStream},
    serde_json::{self, Value},
};
use tokio::{
    sync::{mpsc, Mutex},
    task::JoinHandle,
};

use crate::{
    config::{JsonConfig, JsonViewerKeybinds},
    context::{Index, SharedContext, State},
    guide::{self, GuideAction, GuideMessage},
    jq_worker::JqWorker,
    json,
};

/// Represent the trigger for rendering views.
pub enum RenderTrigger {
    /// User actions such as key presses
    UserAction(Event),
    /// Query changes such as new jq filter input
    QueryChanged { query: String },
    /// Terminal resize events
    AreaResized,
}

/// JSON viewer that maintains the state of JSON stream
/// and handles user interactions and query processing.
pub struct JsonViewer {
    state: jsonstream::State,
    json: Arc<Vec<serde_json::Value>>,
    /// Thread evaluating jq filters against the (pre-converted) input.
    worker: JqWorker,
    keybinds: JsonViewerKeybinds,
}

pub type SharedJsonViewer = Arc<Mutex<JsonViewer>>;

impl JsonViewer {
    /// Get the parsed (unfiltered) input.
    pub fn input(&self) -> Arc<Vec<serde_json::Value>> {
        self.json.clone()
    }

    /// Get the handle to the jq evaluation thread.
    pub fn worker(&self) -> JqWorker {
        self.worker.clone()
    }

    /// Get the formatted content of current JSON stream.
    pub fn formatted_content(&self) -> String {
        self.state.config.format_raw_json(self.state.stream.rows())
    }

    /// Handle user event and update the viewer state accordingly.
    fn handle_user_event(&mut self, event: &Event) {
        // Navigation on an empty result (e.g. a `select` that matched nothing)
        // is a no-op; the underlying rows would otherwise be indexed out of bounds.
        if self.state.stream.rows().is_empty() {
            return;
        }

        match event {
            // Move up.
            event if self.keybinds.up.contains(event) => {
                self.state.stream.up();
            }

            // Move down.
            event if self.keybinds.down.contains(event) => {
                self.state.stream.down();
            }

            // Move to head
            event if self.keybinds.move_to_head.contains(event) => {
                self.state.stream.head();
            }

            // Move to tail
            event if self.keybinds.move_to_tail.contains(event) => {
                self.state.stream.tail();
            }

            // Toggle collapse/expand
            event if self.keybinds.toggle.contains(event) => {
                self.state.stream.toggle();
            }

            event if self.keybinds.expand.contains(event) => {
                self.state.stream.set_nodes_visibility(false);
            }

            event if self.keybinds.collapse.contains(event) => {
                self.state.stream.set_nodes_visibility(true);
            }

            _ => (),
        }
    }

    /// Apply the outcome of a query to the viewer state and return the guide message to show.
    ///
    /// While a query is being typed it is often temporarily invalid
    /// (e.g. an unterminated string in `select(.name == "Al`) or evaluates to `null`.
    /// In those cases the last valid result (including cursor and fold state)
    /// is kept on screen instead of falling back to the unfiltered input,
    /// so the filtered data doesn't flicker away.
    fn apply_result(&mut self, query: String, result: QueryResult) -> Option<GuideMessage> {
        match result {
            QueryResult::Stream(stream) => {
                self.state.stream = stream;
                None
            }
            QueryResult::Empty(stream) => {
                self.state.stream = stream;
                Some(GuideMessage::JqReturnedEmpty(query))
            }
            QueryResult::AllNull => Some(GuideMessage::JqReturnedNull(query)),
            QueryResult::Failed(e) => Some(GuideMessage::JqFailed(e)),
        }
    }
}

/// Outcome of evaluating a query, computed without holding the viewer lock.
enum QueryResult {
    Stream(JsonStream),
    Empty(JsonStream),
    AllNull,
    Failed(String),
}

/// Whether the query is the identity filter, whose result is the input itself.
fn is_identity(query: &str) -> bool {
    matches!(query.trim(), "" | ".")
}

/// Evaluate the query and build the resulting stream.
/// Returns `None` when the query was superseded by a newer one.
async fn evaluate(source: Arc<Vec<Value>>, worker: JqWorker, query: &str) -> Option<QueryResult> {
    // Building the rows for a large result is CPU heavy, keep it off the async workers.
    let build_stream = |values: Arc<Vec<Value>>| async move {
        tokio::task::spawn_blocking(move || JsonStream::new(values.iter()))
            .await
            .ok()
    };

    if is_identity(query) {
        // Skip jq (and the conversions to and from its value type) entirely.
        return build_stream(source).await.map(QueryResult::Stream);
    }

    match worker.run_filter(query.to_string()).await? {
        Ok(ret) if ret.is_empty() => Some(QueryResult::Empty(JsonStream::new(ret.iter()))),
        Ok(ret) if ret.iter().all(Value::is_null) => Some(QueryResult::AllNull),
        Ok(ret) => build_stream(Arc::new(ret)).await.map(QueryResult::Stream),
        Err(e) => Some(QueryResult::Failed(e.to_string())),
    }
}

/// Initialize the JSON viewer with the given input, configuration, keybinds, and shared context.
pub async fn initialize(
    input: &'static str,
    config: JsonConfig,
    keybinds: JsonViewerKeybinds,
    shared_ctx: SharedContext,
    shared_renderer: SharedRenderer<Index>,
) -> anyhow::Result<SharedJsonViewer> {
    // Set state to Loading to prevent overwriting by spinner frames in terminal.
    {
        let mut ctx = shared_ctx.lock().await;
        if let Some(task) = ctx.current_task.take() {
            task.abort();
        }
        ctx.state = State::Loading;
    }

    let input_stream = Arc::new(json::deserialize(input, config.max_streams)?);
    let worker = JqWorker::spawn(input_stream.clone());
    let stream = JsonStream::new(input_stream.iter());
    let state = jsonstream::State {
        stream,
        config: config.stream,
    };

    // Set state to Idle to prevent overwriting by spinner frames in terminal.
    {
        let mut ctx = shared_ctx.lock().await;
        ctx.state = State::Idle;
    }

    {
        let ctx = shared_ctx.lock().await;
        let area = ctx.area;
        drop(ctx);

        // TODO: error handling
        let _ = shared_renderer
            .update([(Index::JsonViewer, state.create_graphemes(area.0, area.1))])
            .render()
            .await;
    }

    Ok(Arc::new(Mutex::new(JsonViewer {
        json: input_stream,
        worker,
        state,
        keybinds,
    })))
}

pub async fn render(
    trigger: RenderTrigger,
    shared_ctx: SharedContext,
    shared_viewer_state: SharedJsonViewer,
    shared_renderer: SharedRenderer<Index>,
    guide_action_tx: mpsc::Sender<GuideAction>,
) {
    match trigger {
        RenderTrigger::UserAction(event) => {
            handle_user_event(shared_viewer_state, shared_renderer, shared_ctx, event).await;
        }
        RenderTrigger::QueryChanged { query } => {
            handle_query_changed(
                shared_viewer_state,
                shared_renderer,
                shared_ctx,
                guide_action_tx,
                query,
            )
            .await;
        }
        RenderTrigger::AreaResized => {
            // The query result does not depend on the terminal size:
            // only the view needs to be laid out again.
            let area = shared_ctx.area().await;
            let graphemes = {
                let viewer = shared_viewer_state.lock().await;
                viewer.state.create_graphemes(area.0, area.1)
            };
            let _ = shared_renderer
                .update([(Index::JsonViewer, graphemes)])
                .render()
                .await;
        }
    }
}

async fn handle_user_event(
    shared_viewer_state: SharedJsonViewer,
    shared_renderer: SharedRenderer<Index>,
    shared_ctx: SharedContext,
    event: Event,
) {
    let area = {
        let ctx = shared_ctx.lock().await;
        ctx.area
    };

    let graphemes = {
        let mut viewer = shared_viewer_state.lock().await;
        viewer.handle_user_event(&event);
        viewer.state.create_graphemes(area.0, area.1)
    };

    // TODO: error handling
    let _ = shared_renderer
        .update([(Index::JsonViewer, graphemes)])
        .render()
        .await;
}

async fn handle_query_changed(
    shared_viewer_state: SharedJsonViewer,
    shared_renderer: SharedRenderer<Index>,
    shared_ctx: SharedContext,
    guide_action_tx: mpsc::Sender<GuideAction>,
    query: String,
) {
    // Abort any ongoing processing task to prevent race conditions
    // and ensure the new render reflects the latest terminal size.
    {
        let mut ctx = shared_ctx.lock().await;
        if let Some(task) = ctx.current_task.take() {
            task.abort();
        }
    }

    let task = spawn_query_update_task(
        shared_viewer_state.clone(),
        shared_ctx.clone(),
        guide_action_tx,
        shared_renderer,
        query,
    );

    // Store the new processing task handle in shared context
    // to allow future cancellation if needed.
    {
        let mut ctx = shared_ctx.lock().await;
        ctx.current_task = Some(task);
    }
}

// Spawn a background task to process the jq query and update the viewer state with the results,
// while managing the viewer state to prevent race conditions and ensure the view reflects the latest terminal size.
fn spawn_query_update_task(
    shared_viewer_state: SharedJsonViewer,
    shared_ctx: SharedContext,
    guide_action_tx: mpsc::Sender<GuideAction>,
    shared_renderer: SharedRenderer<Index>,
    query: String,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        // Set state to Processing to prevent overwriting by spinner frames in terminal.
        {
            let mut ctx = shared_ctx.lock().await;
            ctx.state = State::Processing;
        }

        let (source, worker) = {
            let viewer = shared_viewer_state.lock().await;
            (viewer.input(), viewer.worker())
        };

        // Superseded by a newer query: that one will update the view.
        let Some(result) = evaluate(source, worker, &query).await else {
            return;
        };

        let (maybe_guide, maybe_resp) = {
            let area = shared_ctx.area().await;
            let mut viewer = shared_viewer_state.lock().await;
            let guide = viewer.apply_result(query, result);
            (guide, Some(viewer.state.create_graphemes(area.0, area.1)))
        };

        // Set state to Idle to allow rendering of spinner frames in terminal.
        {
            let mut ctx = shared_ctx.lock().await;
            ctx.state = State::Idle;
        }

        if let Some(message) = maybe_guide {
            let _ = guide_action_tx.send(GuideAction::Show(message)).await;
        }

        // TODO: error handling
        let _ = shared_renderer
            .update([(
                Index::JsonViewer,
                maybe_resp.unwrap_or(StyledGraphemes::default()),
            )])
            .render()
            .await;
    })
}

/// Represent the actions that can be performed in JSON viewer,
/// including copying results to clipboard, handling user events, and processing query changes.
pub enum ViewerAction {
    /// Copy the current JSON stream results to clipboard.
    CopyResult,
    /// Handle user events such as key presses for navigation and toggling.
    UserEvent(Event),
    /// Handle changes in jq query input for dynamic filtering of JSON stream.
    QueryChanged(String),
}

/// Spawn a background task to handle viewer actions such as user events and query changes,
/// and update the viewer state and rendered view accordingly.
pub fn start_viewer_task(
    mut action_rx: mpsc::Receiver<ViewerAction>,
    shared_ctx: SharedContext,
    shared_viewer_state: SharedJsonViewer,
    shared_renderer: SharedRenderer<Index>,
    guide_action_tx: mpsc::Sender<GuideAction>,
) -> JoinHandle<anyhow::Result<()>> {
    tokio::spawn(async move {
        loop {
            tokio::select! {
                Some(action) = action_rx.recv() => {
                    match action {
                        ViewerAction::CopyResult => {
                            let runtime = shared_viewer_state.lock().await;
                            let message = guide::copy_to_clipboard_message(&runtime.formatted_content());
                            let _ = guide_action_tx.send(GuideAction::Show(message)).await;
                        }
                        ViewerAction::UserEvent(event) => {
                            render(
                                RenderTrigger::UserAction(event),
                                shared_ctx.clone(),
                                shared_viewer_state.clone(),
                                shared_renderer.clone(),
                                guide_action_tx.clone(),
                            )
                            .await;
                        }
                        ViewerAction::QueryChanged(query) => {
                            render(
                                RenderTrigger::QueryChanged { query },
                                shared_ctx.clone(),
                                shared_viewer_state.clone(),
                                shared_renderer.clone(),
                                guide_action_tx.clone(),
                            )
                            .await;
                        }
                    }
                }
                else => break,
            }
        }
        Ok(())
    })
}
