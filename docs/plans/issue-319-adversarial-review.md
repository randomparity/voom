# Issue 319 Remote Backup Inventory Adversarial Review

## Scope Reviewed

Implemented surface:

- Remote backup records are persisted as JSONL under
  `<data_dir>/backup-manager/remote-backups.jsonl`.
- `backup-manager` appends one record per successful remote destination upload.
- `voom backup list --destination <name>` reads the persistent inventory.
- Existing local `.vbak` path listing still works.

## Acceptance Criteria Review

| Criterion | Status | Evidence / risk |
|---|---|---|
| Add persistent storage for remote backup records | Implemented | JSONL inventory survives process exit and is under VOOM `data_dir`. |
| Add `voom backup list --destination <name>` | Implemented | CLI parser and command read inventory by destination. |
| Preserve existing local `.vbak` listing behavior | Implemented | Existing path-based listing remains the default when `--destination` is absent. |
| Tests for empty inventory, one destination, multiple destinations, and missing destination | Implemented | Inventory tests cover empty file, multiple records, destination filtering, and absent destination names. CLI parser tests cover destination usage. |
| Update docs and CLI reference | Implemented | `docs/remote-backups.md`, `docs/cli-reference.md`, and the functional test plan were updated. |

## Adversarial Findings

1. JSONL is transparent and agent-friendly, but it is append-only. Cleanup and
   restore issues must define how records become stale, restored, or deleted.

2. `voom backup list --destination` filters inventory by destination name but
   does not validate that the destination is still present in current config.
   That preserves historical visibility but may surprise users after config
   renames.

3. There is no de-duplication if a process retries and uploads the same source
   to the same destination more than once. That is acceptable because each
   record has a unique backup UUID.

4. The command-level behavior is covered mostly through parser and inventory
   unit tests. A future integration test could run the CLI with isolated
   `XDG_CONFIG_HOME` and a seeded inventory file, but the current behavior is
   built on the tested inventory reader.
