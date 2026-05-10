# Issue 322 Adversarial Review: Backup Destination Health

Branch: `feat/issue-322-backup-destination-health`

## Acceptance Criteria Mapping

| Criterion | Status | Notes |
|-----------|--------|-------|
| `voom health check` reports one check per backup destination | Implemented | The deprecated alias uses the same `env check` path; each valid destination emits `backup_destination:<name>`. |
| Validate config shape, rclone availability, remote reachability, and safe write/delete probe | Implemented | Config validation reuses backup-manager validation; rclone probes run `version`, `lsf`, `copyto`, and `deletefile`. |
| Do not log credentials | Implemented | Human, JSON, and persisted details include destination name/kind/status only, not remote URLs or rclone output. |
| Tests cover healthy, missing rclone, unreachable remote, and duplicate destination names | Implemented | Unit tests exercise those paths with a fake runner. |
| Document destination health failures | Implemented | `docs/remote-backups.md`, CLI reference, and functional test plan updated. |

## Risks And Mitigations

1. The write/delete probe can create a leftover remote object if deletion fails.
   - Mitigation: object names are under `.voom-health/<uuid>.tmp`, and a delete
     failure is reported as `probe_failed`.

2. Some destinations may permit reads but intentionally deny writes.
   - Mitigation: the health check reports `probe_failed`; users should grant the
     write/delete permissions required for backup uploads.

3. Invalid duplicate destination names cannot produce one record per destination.
   - Mitigation: invalid config emits `backup_destinations_config` with
     `config_invalid`, because duplicate names are rejected before destinations
     can be treated as independently addressable.
