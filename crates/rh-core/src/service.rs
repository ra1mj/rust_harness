//! Typed services and reversible effects.

use std::sync::Arc;

/// Marker trait for anything that can be provided on and resolved from a
/// [`Context`](crate::Context).
///
/// `Service` is blanket-implemented for every `Send + Sync + 'static`
/// type, so both concrete types (`ToolRegistry`) and trait objects
/// (`dyn ModelProvider`) qualify without extra boilerplate.
pub trait Service: Send + Sync + 'static {}

impl<T: ?Sized + Send + Sync + 'static> Service for T {}

/// A reversible effect.
///
/// Dropping a `Disposer` (or calling [`Disposer::dispose`]) runs the
/// teardown exactly once. This is what "registrations are effects" means:
/// every `provide` / `on` call returns one of these, and unloading a plugin
/// is just dropping its accumulated disposers.
pub struct Disposer(Option<Box<dyn FnOnce() + Send + Sync>>);

impl Disposer {
    /// Build a disposer from a teardown closure.
    pub fn new<F>(f: F) -> Self
    where
        F: FnOnce() + Send + Sync + 'static,
    {
        Self(Some(Box::new(f)))
    }

    /// A disposer that does nothing.
    pub fn noop() -> Self {
        Self(None)
    }

    /// Run the teardown immediately (idempotent).
    pub fn dispose(mut self) {
        if let Some(f) = self.0.take() {
            f();
        }
    }

    /// Whether the teardown has already run.
    pub fn is_disposed(&self) -> bool {
        self.0.is_none()
    }
}

impl Drop for Disposer {
    fn drop(&mut self) {
        if let Some(f) = self.0.take() {
            f();
        }
    }
}

/// A bundle of disposers, e.g. everything a plugin mounted.
pub type Disposers = Vec<Disposer>;

/// Combine a shared value with its disposer, so a service can be handed
/// out as `Arc<T>` while its registration stays reversible.
#[allow(dead_code)]
pub fn provided<T: Service>(value: Arc<T>, disposer: Disposer) -> (Arc<T>, Disposer) {
    (value, disposer)
}
