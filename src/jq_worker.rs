//! Dedicated thread that evaluates jq filters.
//!
//! jaq works on its own value type (`jaq_json::Val`), and converting the whole
//! input from `serde_json::Value` used to happen on *every* query. `Val` is
//! reference counted (`Rc`) and therefore cannot be shared across tokio tasks,
//! so a single thread owns the converted input, does the conversion exactly
//! once, and serves all evaluation requests (filter results and value
//! suggestions) over a channel.
//!
//! Filter requests carry a generation number: when a newer query has been
//! submitted, an older one still waiting in the queue (or still producing
//! outputs) is dropped instead of being evaluated to completion.

use std::{
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc as std_mpsc, Arc,
    },
    thread,
};

use jaq_json::Val;
use serde_json::Value;
use tokio::sync::oneshot;

use crate::json;

enum Request {
    Filter {
        query: String,
        generation: u64,
        reply: oneshot::Sender<Option<anyhow::Result<Vec<Value>>>>,
    },
    ScalarValues {
        query: String,
        limit: usize,
        reply: oneshot::Sender<anyhow::Result<Vec<String>>>,
    },
}

/// Handle to the jq evaluation thread. Cheap to clone.
#[derive(Clone)]
pub struct JqWorker {
    tx: std_mpsc::Sender<Request>,
    latest_generation: Arc<AtomicU64>,
}

impl JqWorker {
    /// Spawn the worker thread. The input is converted to jaq values on that thread,
    /// so this returns immediately.
    pub fn spawn(input: Arc<Vec<Value>>) -> Self {
        let (tx, rx) = std_mpsc::channel::<Request>();
        let latest_generation = Arc::new(AtomicU64::new(0));
        let generation = latest_generation.clone();

        thread::Builder::new()
            .name("jq-worker".into())
            .spawn(move || {
                let inputs: Vec<Val> = input.iter().map(json::to_val).collect();
                drop(input);

                for request in rx {
                    match request {
                        Request::Filter {
                            query,
                            generation: requested,
                            reply,
                        } => {
                            let is_stale = || generation.load(Ordering::Relaxed) != requested;
                            if is_stale() {
                                continue;
                            }
                            let _ =
                                reply.send(json::run_jaq(&query, &inputs, &is_stale).transpose());
                        }
                        Request::ScalarValues {
                            query,
                            limit,
                            reply,
                        } => {
                            let _ = reply.send(json::collect_scalar_values(&query, &inputs, limit));
                        }
                    }
                }
            })
            .expect("failed to spawn jq worker thread");

        Self {
            tx,
            latest_generation,
        }
    }

    /// Evaluate `query` against the input.
    /// Submitting a new query supersedes any previous one; superseded queries
    /// resolve to `None`.
    pub async fn run_filter(&self, query: String) -> Option<anyhow::Result<Vec<Value>>> {
        let generation = self.latest_generation.fetch_add(1, Ordering::Relaxed) + 1;
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(Request::Filter {
                query,
                generation,
                reply,
            })
            .ok()?;
        rx.await.ok().flatten()
    }

    /// Make the filter being evaluated (or waiting) stop, as if superseded.
    pub fn cancel(&self) {
        self.latest_generation.fetch_add(1, Ordering::Relaxed);
    }

    /// Collect the distinct scalar values produced by `query` (JSON encoded, sorted).
    pub async fn scalar_values(&self, query: String, limit: usize) -> anyhow::Result<Vec<String>> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(Request::ScalarValues {
                query,
                limit,
                reply,
            })
            .map_err(|_| anyhow::anyhow!("jq worker is not running"))?;
        rx.await
            .map_err(|_| anyhow::anyhow!("jq worker stopped unexpectedly"))?
    }
}
