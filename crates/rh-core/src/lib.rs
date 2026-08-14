//! rh-core — the plugin host.
//!
//! This crate is the Rust analogue of DeepSeek Harness's Cordis layer:
//! **everything is a plugin**. Every part of a running agent — the model
//! adapter, the tool registry, the session log, the agent loop — is a
//! [`Plugin`] that contributes to a shared [`Context`]. There is no
//! privileged core to patch: you extend the harness by mounting a plugin
//! beside the others.
//!
//! A plugin contributes three kinds of things, all of which are reversible:
//!
//! * **typed services** — a swappable capability exposed by a trait object,
//!   registered with [`Context::provide`] and resolved with
//!   [`Context::service`]. This is the "Service Definition / Service
//!   Provider / Consumer" seam.
//! * **typed events** — extension points a plugin subscribes to with
//!   [`Context::on`] and publishes to with [`Context::emit`].
//! * **reversible effects** — every registration returns a [`Disposer`];
//!   dropping it unwinds the registration.
//!
//! Registrations are effects: `provide` / `on` return a `Disposer`, and a
//! plugin's [`Plugin::mount`] returns the list of disposers it accumulated,
//! so unloading a plugin unwinds exactly what it mounted.

mod context;
mod event;
mod service;

pub use context::{Context, Plugin};
pub use event::{Event, EventHandler};
pub use service::{Disposer, Disposers, Service};
