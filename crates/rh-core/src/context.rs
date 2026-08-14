//! The shared composition context and the plugin trait.

use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

use anyhow::Result;

use crate::event::{Event, EventHandler};
use crate::service::{Disposer, Disposers, Service};

/// A plugin contributes services, events, and reversible effects to a
/// shared [`Context`].
///
/// `mount` returns the disposers for everything it registered; the host
/// keeps them keyed by plugin and drops them to unload the plugin.
pub trait Plugin: Send + Sync {
    /// Stable, human-readable plugin name (used by `dump-config`).
    fn name(&self) -> &'static str;

    /// Contribute to the context. Returned disposers unwind the
    /// contributions when dropped.
    fn mount(&self, ctx: &Context) -> Result<Disposers>;
}

struct HandlerEntry {
    id: u64,
    handler: Arc<dyn Any + Send + Sync>,
}

struct ContextInner {
    /// Typed services, keyed by the `TypeId` of the service type. The
    /// stored value is an `Arc<T>` (the service handle) erased to
    /// `dyn Any`; wrapping the handle in its own `Arc` keeps it `Sized`
    /// even when `T` is a trait object such as `dyn ModelProvider`.
    services: RwLock<HashMap<TypeId, Arc<dyn Any + Send + Sync>>>,
    service_names: RwLock<HashMap<TypeId, String>>,
    handlers: RwLock<HashMap<TypeId, Vec<HandlerEntry>>>,
    next_handler_id: AtomicU64,
}

/// The shared composition context.
///
/// Cheap to clone: clones share the same underlying registries, so any
/// component holding a `Context` sees the same services and events.
#[derive(Clone)]
pub struct Context(Arc<ContextInner>);

impl Default for Context {
    fn default() -> Self {
        Self::new()
    }
}

impl Context {
    /// Create a fresh, empty context.
    pub fn new() -> Self {
        Self(Arc::new(ContextInner {
            services: RwLock::new(HashMap::new()),
            service_names: RwLock::new(HashMap::new()),
            handlers: RwLock::new(HashMap::new()),
            next_handler_id: AtomicU64::new(0),
        }))
    }

    /// Register a typed service. Returns a disposer that removes it.
    ///
    /// Later registrations of the same type replace earlier ones; swapping
    /// a provider this way changes every consumer at once — the whole point
    /// of the capability seam.
    pub fn provide<T: Service + ?Sized>(&self, service: Arc<T>) -> Disposer {
        self.provide_named(std::any::type_name::<T>(), service)
    }

    /// Register a typed service under a stable diagnostic name.
    pub fn provide_named<T: Service + ?Sized>(
        &self,
        name: impl Into<String>,
        service: Arc<T>,
    ) -> Disposer {
        let key = TypeId::of::<T>();
        let inner = Arc::clone(&self.0);
        // `Arc<T>` is a `Sized` value even when `T` is unsized, so wrapping
        // the handle in another `Arc` lets us erase to `dyn Any` and
        // downcast back to `Arc<T>` on retrieval.
        let erased: Arc<dyn Any + Send + Sync> = Arc::new(service);
        inner
            .services
            .write()
            .expect("service map poisoned")
            .insert(key, erased);
        inner
            .service_names
            .write()
            .expect("service-name map poisoned")
            .insert(key, name.into());
        Disposer::new(move || {
            inner
                .services
                .write()
                .expect("service map poisoned")
                .remove(&key);
        })
    }

    /// Resolve a typed service, if registered.
    pub fn service<T: Service + ?Sized>(&self) -> Option<Arc<T>> {
        let erased = Arc::clone(
            self.0
                .services
                .read()
                .expect("service map poisoned")
                .get(&TypeId::of::<T>())?,
        );
        let wrapped = erased.downcast::<Arc<T>>().ok()?;
        Some(Arc::try_unwrap(wrapped).unwrap_or_else(|w| (*w).clone()))
    }

    /// Resolve a typed service, or fail with a descriptive error.
    pub fn service_or<T: Service + ?Sized>(&self, what: &str) -> Result<Arc<T>> {
        self.service::<T>()
            .ok_or_else(|| anyhow::anyhow!("no service registered for {what}"))
    }

    /// Subscribe to a typed event. Returns a disposer that removes the
    /// subscription.
    pub fn on<E: Event>(&self, handler: EventHandler<E>) -> Disposer {
        let key = TypeId::of::<E>();
        let inner = Arc::clone(&self.0);
        let id = inner.next_handler_id.fetch_add(1, Ordering::Relaxed);
        let erased: Arc<dyn Any + Send + Sync> = Arc::new(handler);
        inner
            .handlers
            .write()
            .expect("handler map poisoned")
            .entry(key)
            .or_default()
            .push(HandlerEntry { id, handler: erased });
        Disposer::new(move || {
            let mut map = inner.handlers.write().expect("handler map poisoned");
            if let Some(entries) = map.get_mut(&key) {
                entries.retain(|entry| entry.id != id);
            }
        })
    }

    /// Publish an event to every subscriber of its type.
    ///
    /// Handlers run inline, in registration order. A snapshot is taken
    /// before dispatch, so handlers may subscribe/unsubscribe safely while
    /// an event is being delivered.
    pub fn emit<E: Event>(&self, event: &E) {
        let key = TypeId::of::<E>();
        let snapshot: Vec<Arc<dyn Any + Send + Sync>> = self
            .0
            .handlers
            .read()
            .expect("handler map poisoned")
            .get(&key)
            .map(|entries| entries.iter().map(|entry| Arc::clone(&entry.handler)).collect())
            .unwrap_or_default();
        for erased in snapshot {
            if let Some(handler) = erased.downcast_ref::<EventHandler<E>>() {
                handler(event);
            }
        }
    }

    /// Human-readable names of registered services (for `dump-config`).
    pub fn service_names(&self) -> Vec<String> {
        self.0
            .service_names
            .read()
            .expect("service-name map poisoned")
            .values()
            .cloned()
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Ping(String);

    #[test]
    fn service_roundtrip_and_replace() {
        let ctx = Context::new();
        let d1 = ctx.provide(Arc::new(Ping("one".into())));
        assert_eq!(ctx.service::<Ping>().unwrap().0, "one");

        // Later registration replaces the earlier one.
        let d2 = ctx.provide(Arc::new(Ping("two".into())));
        assert_eq!(ctx.service::<Ping>().unwrap().0, "two");

        // Disposing the registration removes the service.
        d2.dispose();
        assert!(ctx.service::<Ping>().is_none());

        drop(d1);
    }

    trait Greeter: Send + Sync {
        fn greet(&self) -> String;
    }

    struct Hi;

    impl Greeter for Hi {
        fn greet(&self) -> String {
            "hi".into()
        }
    }

    #[test]
    fn trait_object_service_roundtrip() {
        let ctx = Context::new();
        let greeter: Arc<dyn Greeter> = Arc::new(Hi);
        // Hold the disposer: dropping it unwinds the registration.
        let _registration = ctx.provide(greeter);
        assert_eq!(ctx.service::<dyn Greeter>().unwrap().greet(), "hi");
    }

    #[derive(Debug, PartialEq)]
    struct Evt {
        n: u32,
    }

    #[test]
    fn event_pub_sub_is_reversible() {
        let ctx = Context::new();
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen2 = Arc::clone(&seen);
        let sub = ctx.on::<Evt>(Arc::new(move |event: &Evt| {
            seen2.lock().unwrap().push(event.n);
        }));

        ctx.emit(&Evt { n: 1 });
        ctx.emit(&Evt { n: 2 });
        sub.dispose();
        ctx.emit(&Evt { n: 3 });

        assert_eq!(*seen.lock().unwrap(), vec![1, 2]);
    }
}
