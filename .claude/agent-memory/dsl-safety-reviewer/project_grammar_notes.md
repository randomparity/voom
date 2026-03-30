---
name: DSL Grammar Notes and Hazards
description: Grammar rule dependencies, hazards, and known parser quirks in grammar.pest
type: project
---

**Grammar file:** `crates/voom-dsl/src/grammar.pest`

**Named rule extractors in parser:**
- `lang_target = { "audio" | "subtitle" }` — added in code-health branch; parser uses `inner.find(|p| p.as_rule() == Rule::lang_target)` sequential iterator consumption. Safe.
- `run_if_trigger = { "modified" | "completed" }` — added in code-health branch; parser uses sequential `.find()` on `ident` then `run_if_trigger`. Safe.
- `leading_keyword()` helper extracted to reduce duplication: splits on whitespace/`:` / `(`.

**Key pest behavior (verified):**
- String literals (`"or"`, `"and"`, `"where"`, `"not"`, `"to"`) in non-atomic rules are SILENT — they do NOT produce pairs in `pair.into_inner()`. This is why `build_condition_or` correctly only sees `condition_and` pairs, not the `"or"` tokens.
- WHITESPACE and COMMENT are defined as `_{ }` (silent rules) — correct.
- This means the keyword dispatch via `leading_keyword(pair.as_str())` is the correct pattern for alternation rules.

**Known parser quirks:**
- For alternation rules (config_item, synth_item, action, filter_atom, condition_atom), pest silently consumes keyword alternatives as unnamed tokens. The parser dispatches by extracting the leading keyword from `pair.as_str()` using `leading_keyword()`. This is fragile for any alternation where the leading keyword overlaps with another keyword.
- `build_track_query`: special-cases the `"track"` literal target (which is silent in pairs) by checking `text.starts_with("track ")` on the raw text. Safe.
- `build_transcode`: extracts target via `text.split_whitespace().nth(1)` — the second word is always "video" or "audio" per grammar, "transcode" is the first.
- `build_actions`: extracts target via `text.split_whitespace().next()` — the first word.

**Grammar hazards noted:**
- `boolean` rule uses negative lookahead `~ !(ASCII_ALPHANUMERIC | "_")` to prevent "truehd" matching as "true". Correct.
- `ident` allows hyphens: `(ASCII_ALPHA | "_") ~ (ASCII_ALPHANUMERIC | "_" | "-")*`. Language codes like "en-US" parse as idents correctly.
- `number = @{ ASCII_DIGIT+ ~ ("." ~ ASCII_DIGIT+)? ~ ASCII_ALPHA* }` — the trailing `ASCII_ALPHA*` allows values like "5.1" or "192k" (bitrate-style). These are parsed as numbers but the alpha suffix is part of the raw string. `parse_number_f64` strips suffix correctly.
- Top-level policy rule enforces EOF: `SOI ~ ... ~ EOI`. No trailing garbage.
- `track_target` does NOT include `"track"` — but `track_query` does: `{ (track_target | "track") ~ ... }`. The parser builds a target string from `as_str()` in `build_keep_remove`, so `"track"` only appears in condition/filter contexts, never in keep/remove operations.
- Grammar enforces `phase+` (at least one phase required). Empty policy bodies are a parse error.
- `actions_op` accepts "subtitle" but NOT "subtitles" (asymmetric with track_target which has both). Not a bug, just potentially confusing.

**No exponential backtracking risk found:** PEG grammar with no nested repetitions of nullable rules. All repetitions (`*`, `+`) are inside atomic rules (`@{ }`) or have explicit terminal characters.

**Stack overflow risk:** Condition/filter nesting is limited by the parser to `MAX_NESTING_DEPTH = 100` levels using a depth counter. Parenthesized expressions track depth explicitly.

**Previously fixed bug (context from code-health branch):** `build_field_access` previously had trailing whitespace in path segments — this was fixed in an earlier sprint.

**New finding (2026-03-29):** `debug_assert!(false, ...)` in parser match arms (lines 86, 148, 448, 473) silently swallow unexpected CST nodes in release mode. In debug mode they panic. Better to return `DslError::Build` for these cases to maintain correctness across both build profiles.
