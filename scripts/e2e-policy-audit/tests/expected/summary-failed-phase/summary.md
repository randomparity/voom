# E2E Run Summary — FAIL

Run dir: `<RUN_DIR>`
Generated: <GENERATED_AT>

## Counts

- Pre-run files: 4
- Post-run files: 4

## Hard criteria

- FAIL: phase transcode-video: 1/3 plans failed

## Soft criteria

No warnings.

## Per-phase plan summary

```
phase	total	completed	skipped	failed
containerize	1	1	0	0
transcode-video	3	1	1	1
```

## Anomaly section

### Failure clusters
```
phase	signature	exit_code	container	video_codec	count	top_resolution	sample_path	sample_plan_id	sample_error
transcode-video	unknown				1			plan-4	encoder failed
```

### Failed plans (first 20)
```
plan_id	file_id	phase	result
plan-4	file-4	transcode-video	{"error":"encoder failed"}
```

### Top 10 longest jobs
```
10	job-2	completed
5	job-1	completed
```

### First 50 db-vs-ffprobe-post divergences (introspection bugs)

## Linked artifacts

- [diffs/](diffs/)
- [logs/](logs/)
- [reports/](reports/)
- [db-export/](db-export/)
- [web-smoke/](web-smoke/)
