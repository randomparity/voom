# Issue 321 Adversarial Review: Remote Backup Verification

Branch: `feat/issue-321-remote-verify`

## Acceptance Criteria Mapping

| Criterion | Status | Notes |
|-----------|--------|-------|
| Add `voom backup verify --destination <name>` | Implemented | New backup subcommand verifies one configured destination. |
| Compare inventory against remote object size and optional hash metadata | Implemented | Size is checked with `rclone size --json`; SHA-256 is compared when both inventory and rclone provide it. |
| Report missing, mismatched, and verified backups in table and JSON formats | Implemented | Results use `verified`, `missing`, `size_mismatch`, `hash_mismatch`, and `error`. |
| Tests cover clean, missing object, size mismatch, and destination config errors | Implemented | CLI unit tests use a fake rclone and direct config validation. |
| Document generated-corpus verification workflow | Implemented | `docs/functional-test-plan-remote-backups.md` includes `backup verify`. |

## Risks And Mitigations

1. Old inventory records do not have `sha256`.
   - Mitigation: the field is optional and defaults during deserialization.
     Verification still checks size for old rows and reports hash as
     `not_recorded`.

2. Not every rclone backend exposes SHA-256 metadata.
   - Mitigation: `hashsum SHA-256` failure is treated as hash unavailable after
     size succeeds. Missing objects are still detected by the mandatory size
     check.

3. The CLI exits non-zero after reporting any non-verified record.
   - Mitigation: JSON/table output is written before the error, so automation can
     parse the report and still use process status as a guardrail.

4. Hash verification covers newly recorded uploads only.
   - Mitigation: future uploads persist SHA-256. Existing rows remain usable for
     size verification without migration.
