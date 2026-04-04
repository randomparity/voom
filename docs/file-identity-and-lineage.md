# File Identity and Lineage

This document explains how VOOM tracks file identity, detects changes, and
preserves history across external modifications.

## Core Concepts

### UUID vs Content Hash

VOOM uses two distinct identifiers for files:

| Identifier | Column | Purpose | Changes when |
|-----------|--------|---------|-------------|
| **UUID** | `files.id` | Database identity. Assigned once when a file record is created. Used as the primary key and foreign key in transitions, plans, and tracks. | Never. A new UUID always means a new database record. |
| **Content hash** | `files.content_hash` | Fingerprint of the file's bytes on disk. Computed during discovery scans. | Whenever the file's content changes (VOOM processing, external re-encode, manual edit). |
| **Expected hash** | `files.expected_hash` | The content hash VOOM expects to find on the next scan. Set after VOOM processes a file, or backfilled on first discovery. | Updated by VOOM after processing. If the on-disk hash differs from expected, VOOM knows the file was modified externally. |

### Identity Model

VOOM treats **different content as a different file identity**. When content
changes externally (not by VOOM), the old file record is retired and a new one
is created. This is architecturally intentional: the file at the path is
genuinely different from what VOOM last processed.

However, users think in terms of paths, not UUIDs. To bridge this gap, VOOM
uses a **superseded_by chain** to link old identities to new ones.

## The `superseded_by` Chain

When VOOM detects an external modification:

1. The old file record is marked `missing` with `path = NULL`
2. The old record's `superseded_by` column is set to the new file's UUID
3. A new file record is created at the path with a fresh UUID

This creates a singly-linked forward chain:

```
File A (superseded_by = B) → File B (superseded_by = C) → File C (active)
```

**Traversal:**
- **Forward** (old → new): Read `superseded_by` directly from the file record
- **Backward** (new → old): Query `SELECT ... FROM files WHERE superseded_by = ?`

The `voom history` command walks backward from the current file to reconstruct
full lineage, showing transitions from all predecessor identities.

## Reconciliation Scenarios

During a scan, VOOM reconciles discovered files against stored state:

| # | Scenario | Detection | Outcome | `superseded_by`? |
|---|----------|-----------|---------|------------------|
| 1 | File unchanged | Content hash matches `expected_hash` | Record updated in place (size, status) | No |
| 2 | File processed by VOOM | Hash matches `expected_hash` (VOOM set it after processing) | Same as #1 | No |
| 3 | External modification | Content hash differs from `expected_hash` | Old record retired (missing, `superseded_by` set). New record created. | Yes |
| 4 | File moved | Same content hash at a different path | Path updated on existing record | No |
| 5 | New file | Path and hash not seen before | New record created | No |
| 6 | File deleted | Known path not found in scan | Record marked missing | No |
| 7 | Successive external mods | Hash mismatch on each scan | Chain grows: A→B→C | Yes (each pair) |

## How `voom history` Uses Lineage

```
$ voom history /media/movie.mkv

5 transition entries for /media/movie.mkv:

 # | Date                | Source    | File ID    | From Hash | To Hash
---+---------------------+-----------+------------+-----------+---------
 1 | 2026-03-01 10:00:00 | discovery | a3f2bc01...| —         | 7f2a...
 2 | 2026-03-01 12:00:00 | voom      | a3f2bc01...| 7f2a...   | 9e1b...
   | ── external modification ──
 3 | 2026-03-15 08:00:00 | external  | a3f2bc01...| 9e1b...   | c4d8...
 4 | 2026-03-15 08:00:00 | discovery | f891de02...| —         | c4d8...
 5 | 2026-03-20 14:00:00 | voom      | f891de02...| c4d8...   | 1a2b...
```

The "File ID" column and separator row only appear when the history spans
multiple file identities. Single-identity histories look the same as before.

## Chain Limits

- Maximum chain depth: 50 predecessors (prevents infinite loops from corrupt data)
- Cycle detection: stops if a UUID appears twice in the chain
- Purged file records break the chain at that point (history shows partial lineage)
- The chain is unidirectional in storage (forward only), with reverse lookups via index
