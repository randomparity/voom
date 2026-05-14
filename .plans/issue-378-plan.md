# Plan: Instrument pure-publisher plugins in plugin_stats (#378)

## Problem

The dispatcher only times plugins that **subscribe** to events. Pure
publishers (Discovery, Phase Orchestrator, Policy Evaluator) emit events
but never enter the dispatcher path, so they have zero rows in
`plugin_stats`. Documented as a gap in `docs/cli-reference.md` and
`docs/architecture.md`.

## Approach selection

The issue lists three options:

1. **Publish-side timing** — instrument the publisher's call to
   `kernel.dispatch(event)`. Doesn't reflect plugin work; just measures
   dispatch latency.
2. **Plugin self-reporting via a `PluginStats::record(key, value)` host
   API** — requires plugin code changes; the right design but bigger
   scope than #378.
3. **Documented gap** — accept the limit; rely on Prometheus / OTel
   (Deliverables 2 and 3) for end-to-end visibility.

Option 1 measures the wrong thing (dispatch latency is a property of
the bus, not the publisher). Option 2 is the correct long-term design
but requires kernel API surface, WASM host-function additions, and
plugin migrations — out of scope for a single follow-up issue.

**Recommendation: Option 3** — document the gap explicitly as a design
decision, with a forward pointer to Prometheus/OTel for full coverage.
File a separate issue if/when someone needs Option 2's API.

## Changes

### `docs/architecture.md`

Expand the existing "Bus dispatcher instrumentation" subsection with:

```markdown
### Coverage: subscribers only

The dispatcher instruments only plugins that subscribe to events
through the bus. Pure publishers — Discovery, Phase Orchestrator,
Policy Evaluator — emit events but never have their work timed at the
dispatcher boundary.

This is intentional for Deliverable 1:

- The dispatcher's natural instrumentation point is the
  `handler.on_event(...)` invocation. Publishers do not pass through
  it.
- A publish-side timer (timing `bus.publish(event)`) would measure the
  cost of dispatching to other plugins, not the publisher's own work.
- A plugin-self-reporting host API (e.g. `PluginStats::record(key,
  value)`) is the right long-term primitive but requires kernel + WASM
  surface that is out of scope for #92.

For end-to-end visibility across publishers and subscribers, use
Deliverables 2 and 3 (Prometheus `/metrics`, OpenTelemetry).
```

### `docs/cli-reference.md`

The existing caveat under `voom plugin stats` says rows exist only for
subscribers. Strengthen it to reference the architecture-doc design
rationale:

```markdown
> **Coverage:** `plugin_stats` rows are recorded only for plugins that
> subscribe to events (Bus instrumentation point). Pure publishers
> (`discovery`, `phase-orchestrator`, `policy-evaluator`) do not appear
> in the table. See [Bus dispatcher instrumentation — Coverage:
> subscribers only][arch] for design rationale and the path to full
> coverage via Prometheus/OTel (Deliverables 2 and 3).
>
> [arch]: ../architecture.md#coverage-subscribers-only
```

### Follow-up issue (file at PR-merge time)

File `Add PluginStats::record host API for self-reporting publishers`
referencing #378's analysis, scoped to:

- Extend `voom-kernel::PluginContext` with a `stats: Arc<dyn StatsSink>`
  handle.
- Add a `record(plugin_id, event_type, duration, outcome)` host function.
- Migrate Discovery / Phase Orchestrator / Policy Evaluator to call it.
- WASM `host.wit` addition for WASM plugins.

This is real work; punt it as its own issue rather than smuggling it
into #378.

## Acceptance

- [ ] Architecture-doc subsection added.
- [ ] CLI-reference caveat strengthened with link to architecture doc.
- [ ] Follow-up issue filed for Option 2.
- [ ] No code changes (this is the documented-gap path).

## Validation commands

```bash
# Docs lint, if a markdown linter is configured (none currently).
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

(No code changes means tests should pass trivially.)

## Out of scope

- Implementing Option 1 or Option 2.
- Any code changes to plugins or the kernel.
