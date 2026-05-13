//! Test-only hooks for injecting delays into the discovery walker.
//!
//! Compiled only with `--features test-hooks`. Used by acceptance tests in
//! `crates/voom-cli/tests/` to force deterministic walk timing across roots
//! when testing `--execute-during-discovery` scenarios.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

static DELAYS: Mutex<Option<HashMap<PathBuf, Duration>>> = Mutex::new(None);

/// Inject a sleep before the walker begins scanning `root`.
///
/// Call this before invoking `process::run`. The delay fires at the top of
/// `walk_media_files`, blocking the rayon worker assigned to that root.
pub fn set_delay(root: PathBuf, delay: Duration) {
    let mut g = DELAYS.lock().expect("test_hooks delay lock");
    g.get_or_insert_with(HashMap::new).insert(root, delay);
}

/// Remove all injected delays. Call from test teardown.
pub fn clear() {
    *DELAYS.lock().expect("test_hooks delay lock") = None;
}

/// Return the configured delay for `root`, or `None` if none is set.
#[must_use]
pub fn delay_for(root: &Path) -> Option<Duration> {
    DELAYS
        .lock()
        .expect("test_hooks delay lock")
        .as_ref()
        .and_then(|m| m.get(root).copied())
}
