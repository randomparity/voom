//! Per-root execution gating used by streaming process pipeline.
//!
//! Each scanned root has an independent gate that is "closed" while
//! discovery is walking that root and "opens" exactly once when
//! `RootWalkCompleted` arrives for that root. The gate is retained for
//! observability and as a building block; the holding buffer (below) is
//! what actually defers job enqueue in the gate-at-enqueue model.

// Pipeline wiring lands in a later commit (--execute-during-discovery flag +
// RootWalkCompleted event flow). Suppress dead-code lint until then.
#![allow(dead_code)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use parking_lot::{Mutex, RwLock};
use tokio::sync::Notify;

#[derive(Clone, Default)]
pub struct RootGate {
    inner: Arc<RootGateInner>,
}

#[derive(Default)]
struct RootGateInner {
    map: RwLock<HashMap<PathBuf, RootEntry>>,
}

struct RootEntry {
    notify: Arc<Notify>,
    opened: Arc<AtomicBool>,
}

impl RootGate {
    #[must_use]
    pub fn new(roots: &[PathBuf]) -> Self {
        let mut map = HashMap::with_capacity(roots.len());
        for r in roots {
            map.insert(
                r.clone(),
                RootEntry {
                    notify: Arc::new(Notify::new()),
                    opened: Arc::new(AtomicBool::new(false)),
                },
            );
        }
        Self {
            inner: Arc::new(RootGateInner {
                map: RwLock::new(map),
            }),
        }
    }

    /// Open every root unconditionally. Used by dry-run / plan-only callers.
    pub fn open_all(&self) {
        let guard = self.inner.map.read();
        for entry in guard.values() {
            entry.opened.store(true, Ordering::SeqCst);
            entry.notify.notify_waiters();
        }
    }

    /// Mark `root` open. Idempotent.
    pub fn open(&self, root: &Path) {
        let guard = self.inner.map.read();
        if let Some(entry) = guard.get(root) {
            entry.opened.store(true, Ordering::SeqCst);
            entry.notify.notify_waiters();
        }
    }

    /// True iff `root`'s gate is currently open. Used by the ingest stage to
    /// decide whether to send-through or hold.
    #[must_use]
    pub fn is_open(&self, root: &Path) -> bool {
        let guard = self.inner.map.read();
        guard
            .get(root)
            .is_some_and(|e| e.opened.load(Ordering::SeqCst))
    }
}

/// Per-root holding buffer used by the streaming ingest stage to defer
/// WorkItems for roots that have not yet completed their filesystem walk.
///
/// Items are released by [`HoldingBuffer::drain_root`] in FIFO insertion
/// order. The ingest stage preserves priority ordering when calling `push`,
/// so the drained order reflects the original priority. Workers see only
/// items that are already eligible to execute.
#[derive(Default)]
pub struct HoldingBuffer<T> {
    inner: Mutex<HashMap<PathBuf, Vec<T>>>,
}

impl<T> HoldingBuffer<T> {
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
        }
    }

    /// Append `item` to the buffer for `root`.
    pub fn push(&self, root: &Path, item: T) {
        self.inner
            .lock()
            .entry(root.to_path_buf())
            .or_default()
            .push(item);
    }

    /// Drain every item buffered against `root` in FIFO order.
    pub fn drain_root(&self, root: &Path) -> Vec<T> {
        self.inner.lock().remove(root).unwrap_or_default()
    }

    /// Drain every buffered item across all roots. Used on cancellation.
    pub fn drain_all(&self) -> Vec<T> {
        let mut map = self.inner.lock();
        let mut out = Vec::new();
        for (_, mut v) in map.drain() {
            out.append(&mut v);
        }
        out
    }

    /// Number of items currently held for `root`. For tests and metrics only.
    #[must_use]
    pub fn len(&self, root: &Path) -> usize {
        self.inner.lock().get(root).map_or(0, Vec::len)
    }

    /// Whether `root` currently has any items held. For tests and metrics only.
    #[must_use]
    pub fn is_empty(&self, root: &Path) -> bool {
        self.len(root) == 0
    }
}

/// Returns the configured root containing `path` (longest prefix), or `None`.
/// Used by ingest to decide which `HoldingBuffer` slot an item belongs to.
#[must_use]
pub fn root_for_path<'a>(roots: &'a [PathBuf], path: &Path) -> Option<&'a Path> {
    roots
        .iter()
        .filter(|r| path.starts_with(r))
        .max_by_key(|r| r.components().count())
        .map(PathBuf::as_path)
}

#[cfg(test)]
mod root_gate_tests {
    use super::*;

    #[test]
    fn closed_gate_reports_not_open() {
        let gate = RootGate::new(&[PathBuf::from("/a")]);
        assert!(!gate.is_open(Path::new("/a")));
    }

    #[test]
    fn open_marks_gate_open() {
        let gate = RootGate::new(&[PathBuf::from("/a")]);
        gate.open(Path::new("/a"));
        assert!(gate.is_open(Path::new("/a")));
    }

    #[test]
    fn open_all_opens_every_gate() {
        let gate = RootGate::new(&[PathBuf::from("/a"), PathBuf::from("/b")]);
        gate.open_all();
        assert!(gate.is_open(Path::new("/a")));
        assert!(gate.is_open(Path::new("/b")));
    }

    #[test]
    fn unknown_root_reports_not_open() {
        let gate = RootGate::new(&[PathBuf::from("/a")]);
        assert!(!gate.is_open(Path::new("/other")));
    }
}

#[cfg(test)]
mod holding_buffer_tests {
    use super::*;

    #[test]
    fn drain_returns_items_in_push_order() {
        let buf = HoldingBuffer::<u32>::new();
        let r = Path::new("/r");
        buf.push(r, 1);
        buf.push(r, 2);
        buf.push(r, 3);
        assert_eq!(buf.drain_root(r), vec![1, 2, 3]);
        assert_eq!(buf.len(r), 0);
    }

    #[test]
    fn drain_root_only_drains_that_root() {
        let buf = HoldingBuffer::<u32>::new();
        let a = Path::new("/a");
        let b = Path::new("/b");
        buf.push(a, 1);
        buf.push(b, 99);
        assert_eq!(buf.drain_root(a), vec![1]);
        assert_eq!(buf.len(b), 1);
        assert_eq!(buf.drain_root(b), vec![99]);
    }

    #[test]
    fn drain_all_empties_every_root() {
        let buf = HoldingBuffer::<u32>::new();
        buf.push(Path::new("/a"), 1);
        buf.push(Path::new("/b"), 2);
        let mut got = buf.drain_all();
        got.sort();
        assert_eq!(got, vec![1, 2]);
    }

    #[test]
    fn is_empty_returns_true_when_no_items() {
        let buf = HoldingBuffer::<u32>::new();
        let r = Path::new("/r");
        assert!(buf.is_empty(r));
        buf.push(r, 1);
        assert!(!buf.is_empty(r));
    }
}

#[cfg(test)]
mod root_for_path_tests {
    use super::*;

    #[test]
    fn longest_prefix_wins() {
        let roots = vec![PathBuf::from("/m"), PathBuf::from("/m/sub")];
        assert_eq!(
            root_for_path(&roots, Path::new("/m/sub/file.mkv")),
            Some(Path::new("/m/sub"))
        );
    }

    #[test]
    fn no_match_returns_none() {
        let roots = vec![PathBuf::from("/m")];
        assert_eq!(root_for_path(&roots, Path::new("/other/file.mkv")), None);
    }

    #[test]
    fn exact_root_path_returns_root() {
        let roots = vec![PathBuf::from("/m")];
        assert_eq!(
            root_for_path(&roots, Path::new("/m")),
            Some(Path::new("/m"))
        );
    }
}
