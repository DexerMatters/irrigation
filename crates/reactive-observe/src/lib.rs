//! Observation and projection for the incremental runtime.
//!
//! The runtime emits compact facts; this crate folds them into a bounded
//! revision journal, renders Views, and serves both to a page.
//!
//! Rendering has two sources. A host may register a renderer for a View it wants
//! drawn its own way; when it does not, [`generic`] builds the rows from the
//! runtime's facts — the field's shape, its entities in their own order, and the
//! revision each unit moved at — and spells values through the View's own
//! `ContentVtable::describe`. A plate says which of the two drew it. It is
//! read-only with respect to computation: it inspects committed state and never
//! demands a node, so opening the page cannot manufacture the work it shows.
//!
//! ```no_run
//! use reactive_observe::{Observatory, ObserveLevel, observed_runtime};
//!
//! # fn main() -> std::io::Result<()> {
//! // A runtime that reports what it does, plus the recorder that collects it.
//! let (runtime, recorder) = observed_runtime(ObserveLevel::Nodes);
//! // … install your components and seed your Views, as usual …
//!
//! Observatory::builder(runtime, recorder)
//!     // .renderer::<MyView>(draw)   // one call per View you want drawn
//!     .bind_from_args()             // --bind 127.0.0.1:7777
//!     .serve()?;                    // finds the page, prints the URL, waits
//! # Ok(())
//! # }
//! ```

#![warn(missing_docs)]
#![forbid(unsafe_code)]

pub mod collector;
pub mod generic;
pub mod http;
pub mod journal;
pub mod project;
pub mod recorder;
pub mod render;
pub mod wire;

use std::sync::Arc;

pub use collector::{Collector, Frame, Subscription, TextChannel};
pub use journal::{Activity, Journal, RevisionRecord};
pub use project::{Caps, ProjectedField, Renderers};
pub use reactive_runtime::observe::{DemandCause, ObserveLevel, RuntimeEvent, RuntimeObserver};
pub use recorder::{Recorder, recorder};
pub use render::{
    FieldRender, GraphEdge, GraphNode, ListRow, MapRow, RenderError, RenderModel, ScalarRow,
    SetMember, TreeRow, UnitRef, ValueFormatter, ValuePreview, ViewRenderer, formatter,
    multi_field, single_field,
};
pub use wire::{
    DepWire, EventWire, FieldRenderWire, GraphEdgeWire, GraphNodeWire, NodeDetailWire, NodeWire,
    RevisionWire, RowState, StatsWire, UnitWire, ViewRenderWire, ViewWire, WorkspaceWire,
};

use reactive_runtime::Runtime;
use reactive_runtime::snapshot::{Snapshot, ViewSchema};

/// Where the built page is, when the host did not say.
///
/// `OBSERVATORY_DIST` wins; otherwise the working directory and its ancestors are
/// searched for `web/observatory/dist`. A host outside this workspace should set
/// the variable — and gets told so, rather than a blank page, when nothing is
/// found.
#[must_use]
pub fn page_dir() -> Option<std::path::PathBuf> {
    if let Some(dir) = std::env::var_os("OBSERVATORY_DIST") {
        let dir = std::path::PathBuf::from(dir);
        return dir.is_dir().then_some(dir);
    }
    let mut here = std::env::current_dir().ok()?;
    loop {
        let candidate = here.join("web/observatory/dist");
        if candidate.is_dir() {
            return Some(candidate);
        }
        if !here.pop() {
            return None;
        }
    }
}

/// A runtime that reports what it does, with the recorder that collects it.
///
/// The three pieces every host would otherwise write by hand: the recorder, the
/// observer handle, and a runtime configured to emit into it.
#[must_use]
pub fn observed_runtime(level: ObserveLevel) -> (Runtime, Arc<Recorder>) {
    let recorder = recorder(level);
    let observer: Arc<dyn RuntimeObserver> = Arc::clone(&recorder) as Arc<_>;
    let runtime = Runtime::with_observer(reactive_runtime::RuntimeConfig::default(), observer);
    (runtime, recorder)
}

/// A running observation server.
pub struct Observatory {
    collector: Arc<Collector>,
    server: http::Server,
    drain: Option<std::thread::JoinHandle<()>>,
}

impl std::fmt::Debug for Observatory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Observatory")
            .field("url", &self.url())
            .finish()
    }
}

impl Observatory {
    /// Starts building an observatory for `runtime`.
    #[must_use]
    pub fn builder(runtime: Runtime, recorder: Arc<Recorder>) -> Builder {
        Builder::new(runtime, recorder)
    }

    /// The page URL.
    #[must_use]
    pub fn url(&self) -> String {
        self.server.url()
    }

    /// The shared collector.
    #[must_use]
    pub fn collector(&self) -> Arc<Collector> {
        Arc::clone(&self.collector)
    }

    /// The observed runtime.
    #[must_use]
    pub fn runtime(&self) -> Runtime {
        self.collector.runtime().clone()
    }

    /// Stops the server and the collector.
    pub fn shutdown(&self) {
        self.server.shutdown();
        self.collector.stop();
    }

    /// Blocks until the server is shut down from another thread.
    pub fn wait(mut self) {
        self.server.join();
        self.collector.stop();
        if let Some(handle) = self.drain.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for Observatory {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Builder for [`Observatory`].
pub struct Builder {
    runtime: Runtime,
    recorder: Arc<Recorder>,
    bind: String,
    token: Option<String>,
    static_dir: Option<std::path::PathBuf>,
    history: usize,
    caps: Caps,
    renderers: Renderers,
    texts: Vec<(reactive_core::ids::ViewId, collector::TextChannel)>,
}

impl std::fmt::Debug for Builder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Builder")
            .field("bind", &self.bind)
            .field("history", &self.history)
            .finish_non_exhaustive()
    }
}

impl Builder {
    fn new(runtime: Runtime, recorder: Arc<Recorder>) -> Self {
        Self {
            runtime,
            recorder,
            bind: String::from("127.0.0.1:7777"),
            token: None,
            static_dir: None,
            history: 512,
            caps: Caps::default(),
            renderers: Renderers::default(),
            texts: Vec::new(),
        }
    }

    /// The address to bind.
    #[must_use]
    pub fn bind(mut self, address: impl Into<String>) -> Self {
        self.bind = address.into();
        self
    }

    /// Takes the address from `--bind <addr>` or `--bind=<addr>`.
    ///
    /// Every host wrote the same eight lines to read one flag; the page is a dev
    /// tool, so the flag belongs to the library that serves it.
    #[must_use]
    pub fn bind_from_args(mut self) -> Self {
        let mut args = std::env::args().skip(1);
        while let Some(arg) = args.next() {
            if let Some(value) = arg.strip_prefix("--bind=") {
                self.bind = value.to_owned();
            } else if arg == "--bind"
                && let Some(value) = args.next()
            {
                self.bind = value;
            }
        }
        self
    }

    /// A shared secret; required off loopback by the example host.
    #[must_use]
    pub fn token(mut self, token: impl Into<String>) -> Self {
        self.token = Some(token.into());
        self
    }

    /// Directory with the built page.
    #[must_use]
    pub fn static_dir(mut self, dir: impl Into<std::path::PathBuf>) -> Self {
        self.static_dir = Some(dir.into());
        self
    }

    /// Retained revisions.
    #[must_use]
    pub fn history(mut self, revisions: usize) -> Self {
        self.history = revisions.max(1);
        self
    }

    /// Server caps.
    #[must_use]
    pub fn caps(mut self, caps: Caps) -> Self {
        self.caps = caps;
        self
    }

    /// Registers a renderer for a single-field View.
    #[must_use]
    pub fn renderer<V: ViewSchema>(
        mut self,
        render: impl Fn(&Snapshot) -> RenderModel + Send + Sync + 'static,
    ) -> Self {
        self.renderers.insert(single_field::<V, _>(render));
        self
    }

    /// Registers a renderer that draws several fields.
    #[must_use]
    pub fn field_renderer<V: ViewSchema>(
        mut self,
        render: impl Fn(&Snapshot) -> Vec<FieldRender> + Send + Sync + 'static,
    ) -> Self {
        self.renderers.insert(multi_field::<V, _>(render));
        self
    }

    /// Registers a value formatter for one View.
    #[must_use]
    pub fn formatter<V: ViewSchema>(
        mut self,
        format: impl Fn(&Snapshot) -> ValuePreview + Send + Sync + 'static,
    ) -> Self {
        self.renderers.insert_formatter(formatter::<V, _>(format));
        self
    }

    /// Registers a View's text channel, so the page can edit that View in
    /// place. The host owns the syntax; the View can be of any kind.
    #[must_use]
    pub fn text_view<V: ViewSchema>(
        mut self,
        read: impl Fn() -> String + Send + Sync + 'static,
        write: impl Fn(&str) -> Result<String, String> + Send + Sync + 'static,
    ) -> Self {
        self.texts.push((
            V::view_id(),
            collector::TextChannel {
                read: Arc::new(read),
                write: Arc::new(write),
            },
        ));
        self
    }

    /// Starts the server, prints where it is, and blocks until it stops.
    ///
    /// The shape every host ends with, in one call.
    ///
    /// # Errors
    ///
    /// Returns the bind error when the address is unavailable.
    pub fn serve(self) -> std::io::Result<()> {
        let observatory = self.start()?;
        println!("Observatory listening on {}", observatory.url());
        observatory.wait();
        Ok(())
    }

    /// Starts the collector and the HTTP server.
    ///
    /// # Errors
    ///
    /// Returns the bind error when the address is unavailable.
    pub fn start(self) -> std::io::Result<Observatory> {
        let level = self.recorder.level();
        if level == ObserveLevel::Off {
            return Err(std::io::Error::other(
                "the recorder is Off; build it with ObserveLevel::Summary or higher",
            ));
        }
        let collector = Collector::new(
            self.runtime,
            Arc::clone(&self.recorder),
            self.renderers,
            self.history,
            self.caps,
        );
        for (view, channel) in self.texts {
            collector.set_text_view(view, channel);
        }
        let drain = collector.spawn();
        let server = http::serve(
            Arc::clone(&collector),
            http::HttpConfig {
                bind: self.bind,
                token: self.token,
                static_dir: self.static_dir.or_else(page_dir),
            },
        )?;
        Ok(Observatory {
            collector,
            server,
            drain: Some(drain),
        })
    }
}
