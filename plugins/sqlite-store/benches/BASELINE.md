# scan_session baseline (issue-366-baseline)

Captured against the corrected bench fixture (template-clone, per-iteration
isolation, decision/outcome assertions). The prior baseline captured during
PR #373 was contaminated — see issue #366 follow-up.

Hardware: Darwin 25.4.0 arm64, Apple M5 Max
Date captured: 2026-05-11

| group | mean | 95% CI | sample size |
| --- | --- | --- | --- |
| `ingest/new_1000` | 343.80 ms | [341.63 ms, 346.06 ms] | 20 |
| `ingest/unchanged_1000` | 104.99 ms | [104.13 ms, 105.98 ms] | 20 |
| `finish/100k_seed_5k_unseen_500_moves` | 36.458 s | [35.676 s, 37.310 s] | 10 |

To reproduce:

    cargo bench -p voom-sqlite-store --bench scan_session -- --save-baseline issue-366-baseline

To compare against this baseline after a code change:

    cargo bench -p voom-sqlite-store --bench scan_session -- --baseline issue-366-baseline

## Notes

- The `ingest/new_1000` number on this corrected baseline (343 ms) is roughly
  2.3x the prior contaminated baseline (147 ms). The old bench grew the
  database across iterations, but more importantly criterion's warmup phase
  ran the routine many times before the measurement phase — so the
  contaminated baseline's "new_1000" was measuring against a partially-grown
  DB, while this corrected baseline measures against a guaranteed-clean 100k-
  row clone every time.
- The `ingest/unchanged_1000` number (105 ms) is roughly half the prior
  contaminated baseline (207 ms). The old bench's samples 2–N routed
  through `recover_stub_in_tx` instead of the unchanged fast-path because
  cancel-between-iterations left rows with `last_seen_session_id` pointing
  at a cancelled session. This corrected baseline exercises the genuine
  unchanged path on every sample.
- The `finish/100k_seed_5k_unseen_500_moves` number (36.5 s) is within
  noise of the prior contaminated baseline (37.8 s). The finish bench was
  structurally correct before — its prior issue was setup-time waste
  (re-seeding 100k rows every iteration), which is now ~600x faster via
  the same template-clone pattern.
