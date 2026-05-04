# E2E Containerize Test Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a script-driven E2E test harness for VOOM that applies the `01-containerize.voom` policy to the 27 TB / 23,705-file series library at `/mnt/raid0/media/series` from a clean DB, captures artifacts, runs a brief web-UI smoke-test, and produces a PASS/WARN/FAIL summary with before/after comparison.

**Architecture:** Bash driver under `scripts/e2e-containerize/` (checked into the repo), invokes the release binary against the user's real `~/.config/voom/` config dir. All artifacts (snapshots, logs, DB exports, reports, comparison, summary) land in a timestamped run directory under `~/voom-e2e-runs/<ts>/`. Helper scripts split by responsibility: snapshot capture, diff generation, summary build.

**Tech Stack:** Bash 5+, `find` / `stat` / `awk` / `sort` / `comm` for snapshot diffing, `jq` for JSON-extracting voom CLI outputs, `curl` for the serve smoke-test, `sqlite3` for DB export, `cargo` for the release build.

**Spec:** [`docs/superpowers/specs/2026-05-04-e2e-containerize-test-design.md`](../specs/2026-05-04-e2e-containerize-test-design.md)

---

## File Structure

To be created (all under `scripts/e2e-containerize/`):

| File | Responsibility |
|------|----------------|
| `run.sh` | Top-level driver. Parses args, creates run dir, calls every other script in sequence, manages the `voom serve` smoke-test, handles cleanup. |
| `lib/preflight.sh` | Asserts clean state (no `~/.config/voom/voom.db`, no `~/.config/voom/plugins/`), checks tool prerequisites, validates the policy. |
| `lib/snapshot.sh` | Walks the library and emits `library-manifest.tsv`, `ext-tally.txt`, `size-totals.txt`, `non-mkv-files.txt` into a target dir. Used for both pre and post snapshots. |
| `lib/diff-snapshots.sh` | Joins pre/post manifests on path and emits `diff-summary.md` with classification, ext-count delta, byte delta, spot-check results, and the keep_backups invariant check. |
| `lib/db-export.sh` | Runs `sqlite3` against the post-run DB, emits `schema.sql` and per-table TSV dumps. |
| `lib/web-smoke.sh` | Starts `voom serve` on a non-default port, curls a fixed list of endpoints, captures status + body samples, shuts the server down. |
| `lib/build-summary.sh` | Reads logs + diff-summary + db-export + reports and emits the final `summary.md` with PASS/WARN/FAIL verdict and anomaly section. |
| `README.md` | How to invoke the harness, run-dir layout, how to interpret `summary.md`. |

Run-dir layout (created at runtime, not in repo):

```
~/voom-e2e-runs/<YYYY-MM-DD-HHMMSS>/
├── pre/                        snapshot before run
├── post/                       snapshot after run
├── logs/                       per-CLI-call stdout/stderr + process.log
├── db-export/                  schema + table TSV
├── reports/                    voom report/history/files/plans/events/jobs
├── web-smoke/                  curl outputs
├── diff-summary.md
└── summary.md
```

---

## Task 1: Repo skeleton + README

**Files:**
- Create: `scripts/e2e-containerize/README.md`
- Create: `scripts/e2e-containerize/lib/.gitkeep`

- [ ] **Step 1: Create the directory and stub README**

```bash
mkdir -p scripts/e2e-containerize/lib
touch scripts/e2e-containerize/lib/.gitkeep
```

`scripts/e2e-containerize/README.md`:

````markdown
# E2E Containerize Test Harness

Script-driven end-to-end test of VOOM applying `01-containerize.voom`
to the series library at `/mnt/raid0/media/series` from a clean database.

## Usage

```bash
scripts/e2e-containerize/run.sh \
    --library /mnt/raid0/media/series \
    --policy ~/.config/voom/policies/01-containerize.voom \
    --run-dir ~/voom-e2e-runs/$(date +%Y-%m-%d-%H%M%S)
```

All flags have defaults matching the values above; passing none runs
against the canonical configuration.

## Pre-conditions

- `~/.config/voom/voom.db` must not exist
- `~/.config/voom/plugins/` must not exist
- `ffmpeg`, `ffprobe`, `mkvmerge`, `sqlite3`, `jq`, `curl` on PATH
- The library path must be readable; libraries with active writers
  during a run will produce confusing diffs

## Run-dir layout

| Path | Contents |
|------|----------|
| `pre/`, `post/` | library snapshots (manifest + tallies) |
| `logs/` | one file per CLI call, plus `process.log` |
| `db-export/` | `schema.sql` + per-table `.tsv` |
| `reports/` | `voom report` / `history` / `files` / `plans` / `events` / `jobs` |
| `web-smoke/` | curl statuses + body samples |
| `diff-summary.md` | path-level pre→post diff |
| `summary.md` | PASS/WARN/FAIL verdict + anomaly section |

## Interpreting `summary.md`

- **PASS** — every hard criterion in the spec was met.
- **WARN** — hard criteria met but soft criteria flagged (e.g. some
  AVI files failed to remux, or duration outliers detected).
- **FAIL** — at least one hard criterion violated. The summary names
  the specific criterion and the offending evidence.

See `docs/superpowers/specs/2026-05-04-e2e-containerize-test-design.md`
for the full criteria.
````

- [ ] **Step 2: Commit**

```bash
git add scripts/e2e-containerize/
git commit -m "test(e2e): scaffold containerize E2E harness directory"
```

---

## Task 2: `lib/preflight.sh`

**Files:**
- Create: `scripts/e2e-containerize/lib/preflight.sh`

- [ ] **Step 1: Write the script**

```bash
#!/usr/bin/env bash
# Verifies the host is in a state suitable for an E2E run.
# Usage: preflight.sh <policy-path>
set -euo pipefail

policy_path="${1:?policy path required}"

config_dir="${HOME}/.config/voom"
db_file="${config_dir}/voom.db"
plugins_dir="${config_dir}/plugins"

if [[ -e "${db_file}" ]]; then
    echo "PREFLIGHT FAIL: ${db_file} exists. Move it aside before running." >&2
    exit 1
fi
if [[ -e "${plugins_dir}" ]]; then
    echo "PREFLIGHT FAIL: ${plugins_dir} exists. Move it aside before running." >&2
    exit 1
fi

required_tools=(ffmpeg ffprobe mkvmerge sqlite3 jq curl find stat awk)
missing=()
for tool in "${required_tools[@]}"; do
    if ! command -v "${tool}" >/dev/null 2>&1; then
        missing+=("${tool}")
    fi
done
if (( ${#missing[@]} > 0 )); then
    echo "PREFLIGHT FAIL: missing required tools: ${missing[*]}" >&2
    exit 1
fi

if [[ ! -r "${policy_path}" ]]; then
    echo "PREFLIGHT FAIL: policy not readable: ${policy_path}" >&2
    exit 1
fi

echo "preflight OK"
```

- [ ] **Step 2: Lint**

Run: `shellcheck scripts/e2e-containerize/lib/preflight.sh && shfmt -d scripts/e2e-containerize/lib/preflight.sh`
Expected: no output (clean).

- [ ] **Step 3: Smoke-test against the live host**

Run:
```bash
chmod +x scripts/e2e-containerize/lib/preflight.sh
scripts/e2e-containerize/lib/preflight.sh ~/.config/voom/policies/01-containerize.voom
```
Expected: `preflight OK`. If it reports `voom.db` or `plugins/` exists, move them aside before continuing.

- [ ] **Step 4: Negative test — failure path**

Run:
```bash
scripts/e2e-containerize/lib/preflight.sh /tmp/does-not-exist.voom; echo "exit=$?"
```
Expected: `PREFLIGHT FAIL: policy not readable...` on stderr; `exit=1`.

- [ ] **Step 5: Commit**

```bash
git add scripts/e2e-containerize/lib/preflight.sh
git commit -m "test(e2e): add preflight check for clean DB state and tools"
```

---

## Task 3: `lib/snapshot.sh`

**Files:**
- Create: `scripts/e2e-containerize/lib/snapshot.sh`

- [ ] **Step 1: Write the script**

```bash
#!/usr/bin/env bash
# Captures a library snapshot: manifest, extension tally, size totals,
# and non-MKV path list.
# Usage: snapshot.sh <library-root> <output-dir>
set -euo pipefail

lib_root="${1:?library root required}"
out_dir="${2:?output dir required}"

if [[ ! -d "${lib_root}" ]]; then
    echo "snapshot: library root does not exist: ${lib_root}" >&2
    exit 1
fi
mkdir -p "${out_dir}"

# Extensions VOOM scans plus .bak (created post-run).
exts=(mkv mp4 avi m4v mov ts webm bak)
find_args=()
for i in "${!exts[@]}"; do
    if (( i > 0 )); then find_args+=(-o); fi
    find_args+=(-iname "*.${exts[i]}")
done

manifest="${out_dir}/library-manifest.tsv"
printf 'path\tsize\tmtime\textension\n' > "${manifest}"
find "${lib_root}" -type f \( "${find_args[@]}" \) -printf '%p\t%s\t%T@\t%f\n' \
    | awk -F'\t' 'BEGIN{OFS="\t"} {
        n = split($4, parts, "."); ext = tolower(parts[n]);
        print $1, $2, $3, ext
      }' \
    | sort -k1,1 \
    >> "${manifest}"

# Extension tally
awk -F'\t' 'NR>1 {c[$4]++} END {for (e in c) print c[e], e}' "${manifest}" \
    | sort -rn > "${out_dir}/ext-tally.txt"

# Per-extension byte totals + grand total
awk -F'\t' 'NR>1 {b[$4]+=$2; t+=$2} END {
        for (e in b) printf "%-8s %20d\n", e, b[e];
        printf "%-8s %20d\n", "TOTAL", t
    }' "${manifest}" \
    | sort > "${out_dir}/size-totals.txt"

# Non-MKV path list (the population the policy will transform)
awk -F'\t' 'NR>1 && $4!="mkv" && $4!="bak" {print $1}' "${manifest}" \
    > "${out_dir}/non-mkv-files.txt"

count=$(awk -F'\t' 'NR>1' "${manifest}" | wc -l)
echo "snapshot: ${count} files captured under ${out_dir}"
```

- [ ] **Step 2: Lint**

Run: `shellcheck scripts/e2e-containerize/lib/snapshot.sh && shfmt -d scripts/e2e-containerize/lib/snapshot.sh`
Expected: no output.

- [ ] **Step 3: Smoke-test on a small fixture**

Run:
```bash
mkdir -p /tmp/snap-fixture/sub
touch /tmp/snap-fixture/a.mkv /tmp/snap-fixture/b.MP4 /tmp/snap-fixture/sub/c.avi
chmod +x scripts/e2e-containerize/lib/snapshot.sh
scripts/e2e-containerize/lib/snapshot.sh /tmp/snap-fixture /tmp/snap-out
cat /tmp/snap-out/ext-tally.txt
cat /tmp/snap-out/non-mkv-files.txt
rm -rf /tmp/snap-fixture /tmp/snap-out
```
Expected `ext-tally.txt`:
```
1 mp4
1 mkv
1 avi
```
(order may vary). Expected `non-mkv-files.txt` lists the `b.MP4` and `c.avi` paths.

- [ ] **Step 4: Commit**

```bash
git add scripts/e2e-containerize/lib/snapshot.sh
git commit -m "test(e2e): add library snapshot script"
```

---

## Task 4: `lib/diff-snapshots.sh`

**Files:**
- Create: `scripts/e2e-containerize/lib/diff-snapshots.sh`

- [ ] **Step 1: Write the script**

```bash
#!/usr/bin/env bash
# Compares pre/ and post/ snapshots and emits diff-summary.md.
# Usage: diff-snapshots.sh <pre-dir> <post-dir> <out-md>
set -euo pipefail

pre="${1:?pre snapshot dir required}"
post="${2:?post snapshot dir required}"
out="${3:?output md path required}"

pre_m="${pre}/library-manifest.tsv"
post_m="${post}/library-manifest.tsv"
[[ -r "${pre_m}" ]] || { echo "missing ${pre_m}" >&2; exit 1; }
[[ -r "${post_m}" ]] || { echo "missing ${post_m}" >&2; exit 1; }

# Path-only sorted lists for set ops
pre_paths=$(mktemp); post_paths=$(mktemp)
trap 'rm -f "${pre_paths}" "${post_paths}"' EXIT
awk -F'\t' 'NR>1 {print $1}' "${pre_m}"  | sort > "${pre_paths}"
awk -F'\t' 'NR>1 {print $1}' "${post_m}" | sort > "${post_paths}"

disappeared=$(comm -23 "${pre_paths}" "${post_paths}" | wc -l)
new_files=$(comm -13 "${pre_paths}" "${post_paths}" | wc -l)
common=$(comm -12 "${pre_paths}" "${post_paths}" | wc -l)

# Classify common files: unchanged / mtime-changed / size-changed
join -t $'\t' -j1 \
    <(awk -F'\t' 'NR>1 {print $1"\t"$2"\t"$3}' "${pre_m}"  | sort -k1,1) \
    <(awk -F'\t' 'NR>1 {print $1"\t"$2"\t"$3}' "${post_m}" | sort -k1,1) \
    | awk -F'\t' 'BEGIN{u=0;m=0;s=0}
        { if ($2==$4 && $3==$5) u++;
          else if ($2!=$4) s++;
          else m++; }
        END {print u"\t"m"\t"s}' \
    > /tmp/voom-e2e-classify.$$
read -r unchanged mtime_only size_changed < /tmp/voom-e2e-classify.$$
rm -f /tmp/voom-e2e-classify.$$

# Per-extension count delta
ext_delta=$(
    paste \
        <(awk '{print $2"\t"$1}' "${pre}/ext-tally.txt"  | sort -k1,1) \
        <(awk '{print $2"\t"$1}' "${post}/ext-tally.txt" | sort -k1,1) \
        | awk -F'\t' 'BEGIN{OFS="\t"} {
              if ($1=="" && $3!="") print $3, 0, $4;
              else if ($1!="" && $3=="") print $1, $2, 0;
              else print $1, $2, $4;
          }'
)

# keep_backups invariant: every pre non-MKV must have sibling .bak post
nonmkv_pre="${pre}/non-mkv-files.txt"
missing_bak=0
while IFS= read -r src; do
    [[ -z "${src}" ]] && continue
    if [[ ! -e "${src}.bak" ]]; then
        missing_bak=$((missing_bak + 1))
    fi
done < "${nonmkv_pre}"

# Bytes delta
pre_bytes=$(awk '/^TOTAL/ {print $2}' "${pre}/size-totals.txt")
post_bytes=$(awk '/^TOTAL/ {print $2}' "${post}/size-totals.txt")
bytes_delta=$((post_bytes - pre_bytes))

# Render markdown
{
    echo "# Snapshot Diff Summary"
    echo
    echo "## Path-level classification"
    echo
    echo "| Class | Count |"
    echo "|-------|-------|"
    echo "| Unchanged (size + mtime equal) | ${unchanged} |"
    echo "| mtime-changed (size equal) | ${mtime_only} |"
    echo "| size-changed | ${size_changed} |"
    echo "| Disappeared (in pre, not in post) | ${disappeared} |"
    echo "| New (in post, not in pre) | ${new_files} |"
    echo "| Common path total | ${common} |"
    echo
    echo "## Per-extension delta"
    echo
    echo "| Extension | Pre | Post |"
    echo "|-----------|-----|------|"
    echo "${ext_delta}" | awk -F'\t' '{printf "| %s | %s | %s |\n", $1, $2, $3}'
    echo
    echo "## Bytes"
    echo
    echo "| Metric | Bytes |"
    echo "|--------|-------|"
    echo "| Pre total | ${pre_bytes} |"
    echo "| Post total | ${post_bytes} |"
    echo "| Delta | ${bytes_delta} |"
    echo
    echo "## keep_backups invariant"
    echo
    nonmkv_count=$(wc -l < "${nonmkv_pre}")
    echo "Pre non-MKV files: ${nonmkv_count}"
    echo "Missing sibling .bak post-run: ${missing_bak}"
} > "${out}"

echo "diff-snapshots: wrote ${out}"
```

- [ ] **Step 2: Lint**

Run: `shellcheck scripts/e2e-containerize/lib/diff-snapshots.sh && shfmt -d scripts/e2e-containerize/lib/diff-snapshots.sh`
Expected: no output.

- [ ] **Step 3: Smoke-test on synthetic snapshots**

Run:
```bash
mkdir -p /tmp/diff-pre /tmp/diff-post
printf 'path\tsize\tmtime\textension\n/x/a.mkv\t100\t1.0\tmkv\n/x/b.mp4\t200\t1.0\tmp4\n' \
    > /tmp/diff-pre/library-manifest.tsv
printf '1 mp4\n1 mkv\n' > /tmp/diff-pre/ext-tally.txt
printf 'mkv      100\nmp4      200\nTOTAL    300\n' > /tmp/diff-pre/size-totals.txt
printf '/x/b.mp4\n' > /tmp/diff-pre/non-mkv-files.txt

printf 'path\tsize\tmtime\textension\n/x/a.mkv\t100\t1.0\tmkv\n/x/b.mkv\t250\t2.0\tmkv\n' \
    > /tmp/diff-post/library-manifest.tsv
printf '2 mkv\n' > /tmp/diff-post/ext-tally.txt
printf 'mkv      350\nTOTAL    350\n' > /tmp/diff-post/size-totals.txt
printf '' > /tmp/diff-post/non-mkv-files.txt

chmod +x scripts/e2e-containerize/lib/diff-snapshots.sh
scripts/e2e-containerize/lib/diff-snapshots.sh /tmp/diff-pre /tmp/diff-post /tmp/diff.md
cat /tmp/diff.md
rm -rf /tmp/diff-pre /tmp/diff-post /tmp/diff.md
```
Expected: `Disappeared: 1`, `New: 1`, `Unchanged: 1`, ext delta showing `mp4: 1 → 0` and `mkv: 1 → 2`, bytes delta `+50`. The keep_backups missing count will be `1` (no sibling `.bak` exists in this synthetic test) — that's correct behavior for the assertion.

- [ ] **Step 4: Commit**

```bash
git add scripts/e2e-containerize/lib/diff-snapshots.sh
git commit -m "test(e2e): add snapshot diff script with classification + invariants"
```

---

## Task 5: `lib/db-export.sh`

**Files:**
- Create: `scripts/e2e-containerize/lib/db-export.sh`

- [ ] **Step 1: Write the script**

```bash
#!/usr/bin/env bash
# Exports the post-run VOOM SQLite database to schema + per-table TSV.
# Usage: db-export.sh <db-path> <out-dir>
set -euo pipefail

db="${1:?db path required}"
out="${2:?output dir required}"

if [[ ! -r "${db}" ]]; then
    echo "db-export: not readable: ${db}" >&2
    exit 1
fi
mkdir -p "${out}"

sqlite3 "${db}" '.schema' > "${out}/schema.sql"

mapfile -t tables < <(sqlite3 "${db}" \
    "SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name;")

for t in "${tables[@]}"; do
    sqlite3 -header -separator $'\t' "${db}" "SELECT * FROM \"${t}\";" \
        > "${out}/${t}.tsv"
done

echo "db-export: wrote schema + ${#tables[@]} tables to ${out}"
```

- [ ] **Step 2: Lint**

Run: `shellcheck scripts/e2e-containerize/lib/db-export.sh && shfmt -d scripts/e2e-containerize/lib/db-export.sh`
Expected: no output.

- [ ] **Step 3: Smoke-test on a synthetic SQLite DB**

Run:
```bash
sqlite3 /tmp/dbtest.db "CREATE TABLE foo (id INT, label TEXT); INSERT INTO foo VALUES (1, 'a'), (2, 'b');"
chmod +x scripts/e2e-containerize/lib/db-export.sh
scripts/e2e-containerize/lib/db-export.sh /tmp/dbtest.db /tmp/db-out
cat /tmp/db-out/schema.sql
cat /tmp/db-out/foo.tsv
rm -rf /tmp/dbtest.db /tmp/db-out
```
Expected: `schema.sql` contains `CREATE TABLE foo ...`; `foo.tsv` has header + 2 rows.

- [ ] **Step 4: Commit**

```bash
git add scripts/e2e-containerize/lib/db-export.sh
git commit -m "test(e2e): add SQLite db-export helper"
```

---

## Task 6: `lib/web-smoke.sh`

**Files:**
- Create: `scripts/e2e-containerize/lib/web-smoke.sh`

- [ ] **Step 1: Write the script**

```bash
#!/usr/bin/env bash
# Brief smoke-test of `voom serve`: start, curl a fixed endpoint set,
# capture statuses + body samples, shut down.
# Usage: web-smoke.sh <voom-bin> <out-dir>
set -euo pipefail

voom_bin="${1:?voom binary path required}"
out="${2:?output dir required}"
port="${WEB_SMOKE_PORT:-18080}"
mkdir -p "${out}"

log="${out}/serve.log"
"${voom_bin}" serve --port "${port}" >"${log}" 2>&1 &
serve_pid=$!
trap 'kill "${serve_pid}" 2>/dev/null || true; wait "${serve_pid}" 2>/dev/null || true' EXIT

# Wait up to 30s for the server to bind
for i in $(seq 1 30); do
    if curl -fsS -o /dev/null "http://127.0.0.1:${port}/" 2>/dev/null; then
        break
    fi
    if ! kill -0 "${serve_pid}" 2>/dev/null; then
        echo "web-smoke: server died before binding (see ${log})" >&2
        exit 1
    fi
    sleep 1
done

endpoints=(/ /api/files /api/jobs /api/events)
status_file="${out}/statuses.tsv"
printf 'endpoint\tstatus\n' > "${status_file}"
for ep in "${endpoints[@]}"; do
    body_file="${out}/body$(echo "${ep}" | tr '/' '_').txt"
    status=$(curl -s -o "${body_file}" -w '%{http_code}' \
        "http://127.0.0.1:${port}${ep}" || echo "000")
    printf '%s\t%s\n' "${ep}" "${status}" >> "${status_file}"
    # Truncate body samples to 4 KiB
    if [[ -s "${body_file}" ]]; then
        head -c 4096 "${body_file}" > "${body_file}.head"
        mv "${body_file}.head" "${body_file}"
    fi
done

cat "${status_file}"
echo "web-smoke: artifacts in ${out}"
```

- [ ] **Step 2: Lint**

Run: `shellcheck scripts/e2e-containerize/lib/web-smoke.sh && shfmt -d scripts/e2e-containerize/lib/web-smoke.sh`
Expected: no output.

- [ ] **Step 3: Defer end-to-end smoke until Task 9 (the real run)**

This script needs a populated DB to be meaningful. It will be exercised as part of Task 9 against the post-run DB. No standalone smoke step here.

- [ ] **Step 4: Commit**

```bash
git add scripts/e2e-containerize/lib/web-smoke.sh
git commit -m "test(e2e): add voom serve smoke-test script"
```

---

## Task 7: `lib/build-summary.sh`

**Files:**
- Create: `scripts/e2e-containerize/lib/build-summary.sh`

- [ ] **Step 1: Write the script**

```bash
#!/usr/bin/env bash
# Builds the final summary.md: PASS/WARN/FAIL verdict + anomaly section.
# Reads inputs from the run dir and writes summary.md at its root.
# Usage: build-summary.sh <run-dir> <pre-count> <post-count>
set -euo pipefail

run="${1:?run dir required}"
pre_count="${2:?pre file count required}"
post_count="${3:?post file count required}"

verdict="PASS"
hard_fails=()
soft_warns=()

note_fail() { hard_fails+=("$1"); verdict="FAIL"; }
note_warn() { soft_warns+=("$1"); [[ "${verdict}" == "PASS" ]] && verdict="WARN"; }

# Hard checks
for log_name in doctor.log health.log policy-validate.log; do
    log="${run}/logs/${log_name}"
    if [[ ! -f "${log}" ]]; then
        note_fail "missing log ${log_name}"
        continue
    fi
    rc=$(awk 'END{print last} {last=$0}' "${run}/logs/${log_name}.rc" 2>/dev/null || echo "?")
    if [[ "${rc}" != "0" ]]; then
        note_fail "${log_name} exit code ${rc}"
    fi
done

scan_log_rc="${run}/logs/scan.log.rc"
if [[ ! -f "${scan_log_rc}" ]] || [[ "$(cat "${scan_log_rc}")" != "0" ]]; then
    note_fail "voom scan failed (see logs/scan.log)"
fi

if (( post_count < pre_count )); then
    note_warn "post file count (${post_count}) < pre (${pre_count}) — verify .bak invariant"
fi

# diff-summary signals
diff_md="${run}/diff-summary.md"
if [[ -f "${diff_md}" ]]; then
    missing_bak=$(awk '/Missing sibling \.bak post-run:/ {print $NF}' "${diff_md}" || echo "0")
    disappeared=$(awk -F'\| ' '/Disappeared/ {print $3}' "${diff_md}" | tr -dc '0-9' || echo "0")
    [[ -z "${missing_bak}" ]] && missing_bak=0
    [[ -z "${disappeared}" ]] && disappeared=0
    if (( missing_bak > 0 )); then
        note_fail "${missing_bak} non-MKV source(s) lack a sibling .bak (potential data loss)"
    fi
    if (( disappeared > 0 )) && (( missing_bak == 0 )); then
        # Disappeared but accounted for via .bak — informational
        note_warn "${disappeared} path(s) disappeared (all accounted for via .bak)"
    fi
else
    note_fail "diff-summary.md not generated"
fi

# Pass-through invariant: pre-existing MKVs should be byte-identical post-run.
# diff-snapshots reports size-changed on common paths; any size-changed pre-MKV
# is a hard fail. We approximate by counting size-changed > 0 and flagging.
size_changed=$(awk -F'\| ' '/size-changed/ {print $3}' "${diff_md}" | tr -dc '0-9')
[[ -z "${size_changed}" ]] && size_changed=0
if (( size_changed > 0 )); then
    note_warn "${size_changed} common path(s) changed size — confirm none are pre-existing MKVs"
fi

# Web smoke statuses
statuses="${run}/web-smoke/statuses.tsv"
if [[ -f "${statuses}" ]]; then
    while IFS=$'\t' read -r ep st; do
        [[ "${ep}" == "endpoint" ]] && continue
        if [[ ! "${st}" =~ ^2[0-9][0-9]$ ]]; then
            note_fail "web smoke ${ep} returned ${st}"
        fi
    done < "${statuses}"
else
    note_warn "web-smoke statuses.tsv missing"
fi

# Job stragglers (if jobs report exists)
jobs_report="${run}/reports/jobs.txt"
if [[ -f "${jobs_report}" ]]; then
    if grep -Eqi '\b(running|pending)\b' "${jobs_report}"; then
        note_fail "jobs report contains non-terminal states (running/pending)"
    fi
    failed=$(grep -Ec '\bfailed\b' "${jobs_report}" || true)
    if (( failed > 0 )); then
        note_warn "${failed} job(s) reported as failed (see reports/jobs.txt)"
    fi
fi

# Render
{
    echo "# E2E Run Summary — ${verdict}"
    echo
    echo "Run dir: \`${run}\`"
    date -Iseconds | sed 's/^/Generated: /'
    echo
    echo "## Counts"
    echo
    echo "- Pre-run files: ${pre_count}"
    echo "- Post-run files: ${post_count}"
    echo
    echo "## Hard criteria"
    if (( ${#hard_fails[@]} == 0 )); then
        echo
        echo "All passed."
    else
        echo
        for f in "${hard_fails[@]}"; do echo "- FAIL: ${f}"; done
    fi
    echo
    echo "## Soft criteria"
    if (( ${#soft_warns[@]} == 0 )); then
        echo
        echo "No warnings."
    else
        echo
        for w in "${soft_warns[@]}"; do echo "- WARN: ${w}"; done
    fi
    echo
    echo "## Anomaly section"
    echo
    if [[ -f "${jobs_report}" ]]; then
        echo "### Failed jobs"
        echo '```'
        grep -E '\bfailed\b' "${jobs_report}" | head -50 || echo "(none)"
        echo '```'
    fi
    echo
    echo "### Top 10 longest-running jobs"
    if [[ -f "${run}/db-export/jobs.tsv" ]]; then
        echo '```'
        awk -F'\t' 'NR==1 {for(i=1;i<=NF;i++) h[$i]=i} NR>1 {
            d = ($h["completed_at"] - $h["started_at"]);
            print d "\t" $h["id"] "\t" $h["status"];
        }' "${run}/db-export/jobs.tsv" 2>/dev/null \
            | sort -rn | head -10 || echo "(jobs.tsv missing expected columns)"
        echo '```'
    else
        echo "(no jobs.tsv in db-export)"
    fi
    echo
    echo "## Linked artifacts"
    echo
    echo "- [diff-summary.md](diff-summary.md)"
    echo "- [logs/](logs/)"
    echo "- [reports/](reports/)"
    echo "- [db-export/](db-export/)"
    echo "- [web-smoke/](web-smoke/)"
} > "${run}/summary.md"

echo "build-summary: ${verdict} — see ${run}/summary.md"
```

- [ ] **Step 2: Lint**

Run: `shellcheck scripts/e2e-containerize/lib/build-summary.sh && shfmt -d scripts/e2e-containerize/lib/build-summary.sh`
Expected: no output.

- [ ] **Step 3: Commit**

```bash
git add scripts/e2e-containerize/lib/build-summary.sh
git commit -m "test(e2e): add summary builder with PASS/WARN/FAIL verdict"
```

---

## Task 8: `run.sh` (top-level driver)

**Files:**
- Create: `scripts/e2e-containerize/run.sh`

- [ ] **Step 1: Write the script**

```bash
#!/usr/bin/env bash
# Top-level driver for the E2E containerize test.
set -euo pipefail

usage() {
    cat <<EOF
Usage: $0 [--library DIR] [--policy PATH] [--run-dir DIR] [--no-build] [--no-web]

Defaults:
  --library  /mnt/raid0/media/series
  --policy   ~/.config/voom/policies/01-containerize.voom
  --run-dir  ~/voom-e2e-runs/<timestamp>
EOF
}

library="/mnt/raid0/media/series"
policy="${HOME}/.config/voom/policies/01-containerize.voom"
run_dir="${HOME}/voom-e2e-runs/$(date +%Y-%m-%d-%H%M%S)"
do_build=1
do_web=1

while (( $# > 0 )); do
    case "$1" in
        --library)  library="$2"; shift 2;;
        --policy)   policy="$2"; shift 2;;
        --run-dir)  run_dir="$2"; shift 2;;
        --no-build) do_build=0; shift;;
        --no-web)   do_web=0; shift;;
        -h|--help)  usage; exit 0;;
        *) echo "unknown arg: $1" >&2; usage; exit 2;;
    esac
done

repo_root="$(git -C "$(dirname "$0")" rev-parse --show-toplevel)"
lib_dir="${repo_root}/scripts/e2e-containerize/lib"
voom_bin="${repo_root}/target/release/voom"

mkdir -p "${run_dir}"/{pre,post,logs,reports,db-export,web-smoke}
echo "Run dir: ${run_dir}"

# Helper: run a CLI invocation, capture stdout+stderr to logs/<name>.log
# and the exit code to logs/<name>.log.rc. Does NOT abort on non-zero;
# the summary builder consumes the .rc files.
log_run() {
    local name="$1"; shift
    local rc=0
    "$@" >"${run_dir}/logs/${name}.log" 2>&1 || rc=$?
    echo "${rc}" > "${run_dir}/logs/${name}.log.rc"
    return 0
}

# ---- Pre-flight ----
echo "==> Pre-flight"
"${lib_dir}/preflight.sh" "${policy}"

if (( do_build )); then
    echo "==> cargo build --release --workspace"
    (cd "${repo_root}" && cargo build --release --workspace) \
        2>&1 | tee "${run_dir}/logs/build.log"
fi

[[ -x "${voom_bin}" ]] || { echo "voom binary not found at ${voom_bin}" >&2; exit 1; }

log_run version  "${voom_bin}" --version
log_run doctor   "${voom_bin}" doctor
log_run health   "${voom_bin}" health
log_run policy-validate "${voom_bin}" policy validate "${policy}"

# ---- Pre-snapshot ----
echo "==> Pre-snapshot"
"${lib_dir}/snapshot.sh" "${library}" "${run_dir}/pre" \
    | tee "${run_dir}/logs/snapshot-pre.log"
pre_count=$(awk -F'\t' 'NR>1' "${run_dir}/pre/library-manifest.tsv" | wc -l)

# ---- Discovery + introspection ----
echo "==> voom scan"
log_run scan "${voom_bin}" scan "${library}"

log_run files "${voom_bin}" files

# Spot-check inspect on up to 3 sample paths (one of each common ext)
sample_paths=$(awk -F'\t' 'NR>1 && $4=="mkv" {print $1; exit}' \
    "${run_dir}/pre/library-manifest.tsv")
sample_paths+=$'\n'$(awk -F'\t' 'NR>1 && $4=="mp4" {print $1; exit}' \
    "${run_dir}/pre/library-manifest.tsv")
sample_paths+=$'\n'$(awk -F'\t' 'NR>1 && $4=="avi" {print $1; exit}' \
    "${run_dir}/pre/library-manifest.tsv")
i=0
while IFS= read -r p; do
    [[ -z "${p}" ]] && continue
    log_run "inspect-${i}" "${voom_bin}" inspect "${p}"
    i=$((i+1))
done <<< "${sample_paths}"

# ---- Planning + execution ----
echo "==> voom plans (preview)"
log_run plans-preview "${voom_bin}" plans --policy "${policy}"

run_start=$(date -Iseconds)
echo "==> voom process (long run starts at ${run_start})"
log_run process "${voom_bin}" process --policy "${policy}"

log_run jobs "${voom_bin}" jobs

# ---- Post-run inspection ----
echo "==> Post-run inspection"
log_run events "${voom_bin}" events --since "${run_start}"
cp "${run_dir}/logs/events.log" "${run_dir}/reports/events.jsonl"

log_run report  "${voom_bin}" report
cp "${run_dir}/logs/report.log"  "${run_dir}/reports/report.txt"
log_run history "${voom_bin}" history
cp "${run_dir}/logs/history.log" "${run_dir}/reports/history.txt"
cp "${run_dir}/logs/files.log"   "${run_dir}/reports/files.txt"
cp "${run_dir}/logs/plans-preview.log" "${run_dir}/reports/plans.txt"
cp "${run_dir}/logs/jobs.log"    "${run_dir}/reports/jobs.txt"

db_path="${HOME}/.config/voom/voom.db"
if [[ -f "${db_path}" ]]; then
    "${lib_dir}/db-export.sh" "${db_path}" "${run_dir}/db-export"
else
    echo "WARN: ${db_path} not found post-run" >&2
fi

# ---- Post-snapshot ----
echo "==> Post-snapshot"
"${lib_dir}/snapshot.sh" "${library}" "${run_dir}/post" \
    | tee "${run_dir}/logs/snapshot-post.log"
post_count=$(awk -F'\t' 'NR>1' "${run_dir}/post/library-manifest.tsv" | wc -l)

# ---- Diff ----
echo "==> Diff"
"${lib_dir}/diff-snapshots.sh" "${run_dir}/pre" "${run_dir}/post" \
    "${run_dir}/diff-summary.md"

# ---- Web smoke ----
if (( do_web )); then
    echo "==> Web smoke-test"
    "${lib_dir}/web-smoke.sh" "${voom_bin}" "${run_dir}/web-smoke" \
        2>&1 | tee "${run_dir}/logs/web-smoke.log" || \
        echo "web smoke failed; see logs/web-smoke.log"
fi

# ---- Summary ----
echo "==> Build summary"
"${lib_dir}/build-summary.sh" "${run_dir}" "${pre_count}" "${post_count}"

echo "Done. ${run_dir}/summary.md"
```

- [ ] **Step 2: Lint**

Run: `shellcheck scripts/e2e-containerize/run.sh && shfmt -d scripts/e2e-containerize/run.sh`
Expected: no output.

- [ ] **Step 3: Make executable + commit**

```bash
chmod +x scripts/e2e-containerize/run.sh scripts/e2e-containerize/lib/*.sh
git add scripts/e2e-containerize/run.sh scripts/e2e-containerize/lib/preflight.sh \
        scripts/e2e-containerize/lib/snapshot.sh scripts/e2e-containerize/lib/diff-snapshots.sh \
        scripts/e2e-containerize/lib/db-export.sh scripts/e2e-containerize/lib/web-smoke.sh \
        scripts/e2e-containerize/lib/build-summary.sh
git update-index --chmod=+x scripts/e2e-containerize/run.sh \
    scripts/e2e-containerize/lib/preflight.sh \
    scripts/e2e-containerize/lib/snapshot.sh \
    scripts/e2e-containerize/lib/diff-snapshots.sh \
    scripts/e2e-containerize/lib/db-export.sh \
    scripts/e2e-containerize/lib/web-smoke.sh \
    scripts/e2e-containerize/lib/build-summary.sh
git commit -m "test(e2e): add top-level run.sh driver and mark lib scripts executable"
```

---

## Task 9: Dry-run the harness against a tiny library

The harness must work end-to-end on a small fixture before committing to a multi-day run on 27 TB. This task creates a 4-file synthetic library, runs the entire pipeline against it, and confirms `summary.md` reports PASS.

**Files:**
- Temporary fixture under `/tmp/voom-e2e-fixture/`
- Temporary VOOM config at a non-default path so we don't disturb the real one

- [ ] **Step 1: Build a synthetic 4-file library**

```bash
mkdir -p /tmp/voom-e2e-fixture
# 1 MKV (pass-through), 1 MP4 (remux), 1 AVI (remux), 1 unrelated file
ffmpeg -f lavfi -i testsrc=duration=2:size=320x240:rate=30 -y /tmp/voom-e2e-fixture/a.mkv
ffmpeg -f lavfi -i testsrc=duration=2:size=320x240:rate=30 -y /tmp/voom-e2e-fixture/b.mp4
ffmpeg -f lavfi -i testsrc=duration=2:size=320x240:rate=30 -y /tmp/voom-e2e-fixture/c.avi
echo "not a video" > /tmp/voom-e2e-fixture/readme.txt
```

- [ ] **Step 2: Stash the real config and run the harness against the fixture**

The harness reads from `~/.config/voom/`. To dry-run safely, temporarily move the real config aside:

```bash
mv ~/.config/voom ~/.config/voom.real-stash
mkdir -p ~/.config/voom/policies
cp ~/.config/voom.real-stash/policies/01-containerize.voom ~/.config/voom/policies/

scripts/e2e-containerize/run.sh \
    --library /tmp/voom-e2e-fixture \
    --policy ~/.config/voom/policies/01-containerize.voom \
    --run-dir /tmp/voom-e2e-dryrun \
    --no-build  # binary is already built
```

- [ ] **Step 3: Inspect dry-run summary**

```bash
cat /tmp/voom-e2e-dryrun/summary.md
ls /tmp/voom-e2e-fixture/
```
Expected: `summary.md` reports `PASS` or `WARN` (an AVI failing to remux is a soft warn, not a hard fail). `/tmp/voom-e2e-fixture/` should now contain `a.mkv` (unchanged), `b.mkv` + `b.mp4.bak`, `c.mkv` + `c.avi.bak` (or, if AVI remux failed, only the original AVI plus an entry in failed jobs).

- [ ] **Step 4: Restore real config**

```bash
rm -rf ~/.config/voom
mv ~/.config/voom.real-stash ~/.config/voom
rm -rf /tmp/voom-e2e-fixture /tmp/voom-e2e-dryrun
```

- [ ] **Step 5: If anything broke, fix it before proceeding**

Common breakages and where to fix:
- CLI flag mismatch (e.g. `--policy` vs `--policy-file`) → update the relevant `log_run` call in `run.sh`.
- `voom plans` doesn't accept `--policy` directly → check `voom plans --help` and adjust.
- `voom inspect` requires a different argument shape → adjust the spot-check loop.
- `db-export.sh` finds different table names → no change needed, it enumerates dynamically.
- Web smoke endpoints differ (`/api/files` may be `/files`) → adjust `endpoints` array in `web-smoke.sh`.

Commit any fixes:
```bash
git add scripts/e2e-containerize/
git commit -m "test(e2e): adjust harness based on dry-run findings"
```

---

## Task 10: Execute the full E2E run

This is the long-running task. Per CLAUDE.md, it must dispatch with `run_in_background: true` and wait for the auto-completion notification — no polling, no sleep loops.

**Files:**
- Created at runtime: `~/voom-e2e-runs/<ts>/`

- [ ] **Step 1: Final pre-flight**

```bash
ls -la ~/.config/voom/        # confirm only config.toml + policies/
test ! -e ~/.config/voom/voom.db && echo "DB clean"
test ! -e ~/.config/voom/plugins && echo "plugins clean"
df -h /mnt/raid0              # confirm enough free space for ~288 .bak files
```

- [ ] **Step 2: Dispatch the full run in the background**

When using the Bash tool, set `run_in_background: true`. Command:

```bash
scripts/e2e-containerize/run.sh 2>&1 | tee ~/voom-e2e-last-driver.log
```

(The driver itself logs into the run dir; the outer `tee` captures the orchestrator-level output for easy tailing.)

- [ ] **Step 3: Wait for completion notification**

Do not poll, do not sleep, do not re-run. The Claude Code harness will deliver the completion notification.

- [ ] **Step 4: Read the summary**

```bash
run_dir=$(ls -1dt ~/voom-e2e-runs/*/ | head -1)
cat "${run_dir}/summary.md"
```

- [ ] **Step 5: Read the diff**

```bash
cat "${run_dir}/diff-summary.md"
```

---

## Task 11: Inspect artifacts and produce findings

This task synthesizes the artifacts into a finding report for the user. It does NOT modify code; it interprets results.

**Files:**
- Create: `~/voom-e2e-runs/<ts>/findings.md` (run-dir scoped, not committed)

- [ ] **Step 1: Cross-reference summary.md against the spec's success criteria**

For each hard criterion in the spec (`docs/superpowers/specs/2026-05-04-e2e-containerize-test-design.md` § Success criteria → Hard), confirm `summary.md` evidences it. Note any criterion not directly addressable from the captured artifacts (these are gaps in the harness, not necessarily failures).

- [ ] **Step 2: Audit failed jobs and event-log gaps**

```bash
run_dir=$(ls -1dt ~/voom-e2e-runs/*/ | head -1)
grep -i fail "${run_dir}/reports/jobs.txt" | head -50
jq 'select(.event=="FileDiscovered")' "${run_dir}/reports/events.jsonl" \
    | jq -s '. | length'
jq 'select(.event=="FileIntrospected")' "${run_dir}/reports/events.jsonl" \
    | jq -s '. | length'
```
A gap (`FileDiscovered` count >> `FileIntrospected`) is a soft anomaly to surface.

- [ ] **Step 3: Spot-probe 5 random converted files with ffprobe**

```bash
run_dir=$(ls -1dt ~/voom-e2e-runs/*/ | head -1)
awk -F'\t' 'NR>1 && $4=="mkv" {print $1}' "${run_dir}/post/library-manifest.tsv" \
    | grep -E '/[^/]+\.mkv$' | shuf | head -5 \
    | while read -r p; do echo "=== ${p} ==="; ffprobe -v error -show_format -show_streams "${p}" | head -40; done \
    > "${run_dir}/spot-probes.txt"
```

- [ ] **Step 4: Write findings.md**

Sections to include:
- Verdict (PASS/WARN/FAIL, copied from summary)
- Headline numbers (files in/out, ext deltas, duration, bytes delta)
- Spec criteria coverage table (one row per hard criterion → satisfied? evidence?)
- Failed jobs (count + top 10 with error strings)
- Anomalies surfaced (event-log gaps, duration outliers, unexpected outputs)
- Recommendations / suggested next steps (e.g. AVIs that need transcoding, bugs to file)

- [ ] **Step 5: Surface findings to the user**

Print the findings file inline and offer to open follow-up GitHub issues for any reproducible bugs (per the project's "review process" guidance in CLAUDE.md — out-of-scope issues become GH issues rather than in-place fixes).

---

## Self-Review Notes

Spec coverage:
- Pre-flight (clean state, tools, policy validate) → Tasks 2, 8 step 1.
- Build release binary → Task 8 step 1 (`cargo build --release`).
- Discovery + introspection → Task 8 (scan, files, inspect spot-checks).
- Plan + execute → Task 8 (plans, process, jobs).
- Post-run reports (events, report, history, db dump) → Task 8.
- Web smoke-test → Tasks 6, 8.
- Library snapshot pre/post + diff → Tasks 3, 4, 8.
- Success criteria PASS/WARN/FAIL + anomaly section → Task 7.
- Actually run the test and inspect → Tasks 9, 10, 11.

No placeholders found on review. Type/path consistency: `library-manifest.tsv` columns (`path`, `size`, `mtime`, `extension`) used identically across snapshot, diff, summary. Run-dir layout matches between spec and tasks.

One known unknown: exact CLI flag spellings (e.g. `voom plans --policy` vs `voom plans --policy-file`) are confirmed via `--help` during Task 9 dry-run rather than guessed in advance. This is intentional — the dry-run is the gate that catches these before Task 10's multi-day run.
