# `voom verify`

Per-file media integrity verification. Distinct from `voom env check`,
which checks environment readiness.

## Modes

| Mode      | Tool        | Cost          | Detects                              |
|-----------|-------------|---------------|--------------------------------------|
| quick     | ffprobe     | <1s/file      | container header damage              |
| thorough  | ffmpeg      | ~playtime     | decode errors, truncated streams     |
| hash      | sha256      | ~disk read    | bit-rot (vs prior verification run)  |

## Examples

```bash
voom verify run                                # all files needing verification, quick mode
voom verify run ~/Movies/file.mkv              # specific file
voom verify run --thorough                     # full decode pass
voom verify run --hash                         # sha256 bit-rot detection
voom verify run --since 7d                     # re-verify files older than a week
voom verify run --all                          # force re-verify everything
voom verify run --workers 8                    # 8 parallel workers (quick/hash only)

voom verify report                             # detailed per-file listing
voom verify report --outcome error             # failing files only
voom verify report --file ~/Movies/x.mkv       # history for one file

voom report --integrity                        # aggregate dashboard summary
```

## Quarantine

When a policy uses `on_error: quarantine`, verification failures move the
file to `verifier.quarantine_dir` (config), preserving the basename. The
file's `status` becomes `quarantined` and it is excluded from default
scan/process passes.

Set `quarantine_dir` in `~/.config/voom/config.toml`:

```toml
[plugin.verifier]
quarantine_dir = "/path/to/quarantine"
```

If the policy uses `on_error: quarantine` but `quarantine_dir` is unset,
the action fails loudly (the file is left in place).

## DSL

```
policy "archival" {
  phase verify {
    verify thorough
    on_error: quarantine
  }
  phase backup {
    depends_on: [verify]
    skip when verify.outcome != "ok"
  }
}
```

`verify.outcome` resolves against the **most recent persisted verification
record for the file** — the verify phase in the same evaluation pass does
not synchronously update its own `PhaseOutput`. To pick up just-run
verifications, run the policy in two passes (verify first, then everything
else).

## Web API

Read-only JSON endpoints (see the REST API section of
[`docs/cli-reference.md`](../cli-reference.md) for the full list):

- `GET /api/verify` — list verifications, filterable by `mode` / `outcome` / `limit`
- `GET /api/verify/:file_id` — verifications for one file
- `GET /api/integrity-summary` — aggregate summary
