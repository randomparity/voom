---
name: Validator Coverage Gaps
description: Validation rules implemented, missing, and incomplete in validator.rs
type: project
---

**Implemented and confirmed complete:**
- Unknown codecs with did-you-mean suggestions (validator.rs ~539)
- Invalid language codes (ISO 639-style via `language::is_valid_language`)
- Circular phase dependencies (DFS cycle detection, validator.rs ~149)
- Unreachable phases (fixed-point reachability, validator.rs ~222)
- Conflicting keep+remove on same broad track category (validator.rs ~352)
- Invalid `on_error` values (delegated to `parse_error_strategy`)
- Invalid container names (now via `Container::from_extension`, expanded from hardcoded list)
- Invalid `run_if` trigger values (checked in validator even though grammar constrains it)
- Invalid phase references in `depends_on` and `run_if`
- Duplicate phase names
- set_tag + delete_tag conflict on same key
- set_tag before clear_tags warning
- Unknown transcode keys with did-you-mean suggestions (edit_distance threshold 3)
- Unknown hw values with did-you-mean suggestions
- hw_fallback without hw warning
- Invalid codec type for transcode target (audio vs video codec mismatch)
- Order tracks item validation (list of valid category names checked)
- Defaults kind and value validated (previously a gap — now fixed)

**Confirmed gaps:**
- `validate_filter` has `_ => {}` wildcard that silently skips TitleMatches regex validation.
  A policy with `title matches "[invalid"` passes validate but fails compile_ast.
  The web server's `/api/policy/validate` endpoint uses validate_source (not compile_policy),
  so it returns valid=true for invalid regex patterns. CLI uses compile_policy, so it correctly fails.
- Cycle detection only reports the FIRST independent cycle found, not all cycles. If graph has
  two separate cycles (A->B->A, C->D->C), only the first is reported per validate call.
- Keep+remove conflict detection is filter-blind: ANY keep + remove on the same broad track
  category reports a conflict, even when filters are non-overlapping (false positive).
  E.g., `keep audio where lang in [eng]` + `remove audio where commentary` incorrectly conflicts.
- Language code errors in config block are reported at the policy span (line 1), not the
  config item's span. Error location is imprecise.
- `build_defaults` uses `_ => "subtitle"` fallback for unknown default kinds instead of
  a proper error, meaning grammar additions would silently normalize to "subtitle".
