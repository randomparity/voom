//! Shared types for host function responses at the plugin/kernel boundary.
//!
//! These types are used by both `voom-kernel` (host implementation) and
//! `voom-plugin-sdk` (plugin-side interface) to avoid duplication.

/// Output from running an external tool.
#[non_exhaustive]
#[derive(Debug, Clone)]
pub struct ToolOutput {
    pub exit_code: i32,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

impl ToolOutput {
    /// Create a new tool output from the process results.
    #[must_use]
    pub fn new(exit_code: i32, stdout: Vec<u8>, stderr: Vec<u8>) -> Self {
        Self {
            exit_code,
            stdout,
            stderr,
        }
    }
}

/// Response from an HTTP request.
#[non_exhaustive]
#[derive(Debug, Clone)]
pub struct HttpResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl HttpResponse {
    /// Create a new HTTP response with no headers.
    #[must_use]
    pub fn new(status: u16, body: Vec<u8>) -> Self {
        Self {
            status,
            headers: Vec::new(),
            body,
        }
    }

    /// Create a response with headers.
    #[must_use]
    pub fn with_headers(status: u16, headers: Vec<(String, String)>, body: Vec<u8>) -> Self {
        Self {
            status,
            headers,
            body,
        }
    }
}
