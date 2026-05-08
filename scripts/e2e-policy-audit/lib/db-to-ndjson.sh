#!/usr/bin/env bash
# Reads a directory of per-table TSV exports (from db-export.sh) and emits
# canonical NDJSON (one line per file) with embedded track arrays.
# Usage: db-to-ndjson.sh <tables-dir> <out-ndjson>
set -euo pipefail

tables_dir="${1:?tables dir required}"
out_ndjson="${2:?output ndjson path required}"

files_tsv="${tables_dir}/files.tsv"
tracks_tsv="${tables_dir}/tracks.tsv"
[[ -r "${files_tsv}" ]] || {
  echo "missing ${files_tsv}" >&2
  exit 1
}
[[ -r "${tracks_tsv}" ]] || {
  echo "missing ${tracks_tsv}" >&2
  exit 1
}

python3 - "${files_tsv}" "${tracks_tsv}" "${out_ndjson}" <<'PY'
import csv, json, sys

files_path, tracks_path, out_path = sys.argv[1:4]

def truthy(s): return s == "1"

def numeric(s, kind):
    if s == "" or s is None: return None
    try: return kind(s)
    except ValueError: return None

def canonical_title(value):
    if value is None:
        return ""
    text = value.strip()
    if len(text) >= 2 and text[0] == '"' and text[-1] == '"':
        inner = text[1:-1]
        if inner in {"1.0", "2.0", "5.1", "6.1", "7.1"}:
            return inner
    return text

with open(tracks_path, newline="") as f:
    reader = csv.DictReader(f, delimiter="\t")
    tracks_by_file = {}
    for row in reader:
        fid = row["file_id"]
        tracks_by_file.setdefault(fid, []).append(row)

def container_norm(c):
    return (c or "Other").lower()

def map_track(row):
    tt = row["track_type"]
    base = {
        "index": int(row["stream_index"]),
        "codec": row["codec"],
        "language": row["language"] or "und",
        "title": canonical_title(row["title"]),
        "is_default": truthy(row["is_default"]),
        "is_forced": truthy(row["is_forced"]),
        "track_type": tt,
    }
    if tt == "video":
        base.update({
            "width": numeric(row["width"], int),
            "height": numeric(row["height"], int),
            "frame_rate": numeric(row["frame_rate"], float),
            "is_vfr": truthy(row["is_vfr"]),
            "is_hdr": truthy(row["is_hdr"]),
            "hdr_format": row["hdr_format"] or None,
            "pixel_format": row["pixel_format"] or None,
        })
    elif tt.startswith("audio"):
        base.update({
            "channels": numeric(row["channels"], int),
            "channel_layout": row["channel_layout"] or None,
            "sample_rate": numeric(row["sample_rate"], int),
            "bit_depth": numeric(row["bit_depth"], int),
        })
    return base

with open(files_path, newline="") as f, open(out_path, "w") as out:
    reader = csv.DictReader(f, delimiter="\t")
    for row in reader:
        fid = row["id"]
        all_tracks = [map_track(t) for t in tracks_by_file.get(fid, [])]
        bucketed = {"video": [], "audio": [], "subtitle": [], "attachment": []}
        for t in all_tracks:
            tt = t["track_type"]
            if tt == "video": bucketed["video"].append(t)
            elif tt.startswith("audio"): bucketed["audio"].append(t)
            elif tt.startswith("subtitle"): bucketed["subtitle"].append(t)
            elif tt == "attachment": bucketed["attachment"].append(t)
        for arr in bucketed.values():
            arr.sort(key=lambda t: t["index"])
        record = {
            "path": row["path"],
            "size": int(row["size"]),
            "mtime": None,
            "container": container_norm(row["container"]),
            "duration": numeric(row["duration"], float) or 0.0,
            "bitrate": numeric(row["bitrate"], int),
            "content_hash": row["content_hash"] or None,
            "video": bucketed["video"],
            "audio": bucketed["audio"],
            "subtitle": bucketed["subtitle"],
            "attachment": bucketed["attachment"],
        }
        out.write(json.dumps(record, separators=(",", ":")) + "\n")
PY

count=$(wc -l <"${out_ndjson}")
echo "db-to-ndjson: ${count} files written to ${out_ndjson}"
