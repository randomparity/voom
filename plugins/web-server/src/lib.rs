//! Web Server Plugin for VOOM.
//!
//! Provides:
//! - REST API (JSON) for files, jobs, plans, plugins, stats, policy validate/format
//! - Web dashboard with Tera templates, htmx, and Alpine.js
//! - SSE for live job/scan progress updates

pub mod api;
pub mod errors;
pub mod middleware;
pub mod router;
pub mod server;
pub mod sse;
pub mod state;
pub mod templates;
pub mod views;
