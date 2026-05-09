//! Web server library for VOOM.
//!
//! Provides:
//! - REST API (JSON) for files, jobs, plans, plugins, stats, policy validate/format
//! - Web dashboard with Tera templates, htmx, and Alpine.js
//! - SSE for live job/scan progress updates
//!
//! Kernel event-bus integration lives in `voom-web-sse-bridge`; this crate is
//! called directly by the application and does not implement `voom_kernel::Plugin`.

pub mod api;
pub mod errors;
pub mod middleware;
pub mod router;
pub mod server;
pub mod sse;
pub mod state;
pub mod templates;
pub mod views;
