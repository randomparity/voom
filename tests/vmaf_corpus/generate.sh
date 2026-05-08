#!/usr/bin/env bash
set -euo pipefail

usage() {
	cat <<'USAGE'
usage: generate.sh [--duration SECONDS] [--size WIDTHxHEIGHT] OUT_DIR

Generate the synthetic VMAF integration corpus. The default duration is 30s.
USAGE
}

duration=30
size=320x180

while [[ $# -gt 0 ]]; do
	case "$1" in
	--duration)
		duration="${2:?missing duration}"
		shift 2
		;;
	--size)
		size="${2:?missing size}"
		shift 2
		;;
	-h | --help)
		usage
		exit 0
		;;
	-*)
		echo "unknown option: $1" >&2
		usage >&2
		exit 2
		;;
	*)
		out_dir="$1"
		shift
		;;
	esac
done

if [[ -z "${out_dir:-}" ]]; then
	usage >&2
	exit 2
fi

mkdir -p "$out_dir"

ffmpeg_base=(
	ffmpeg
	-hide_banner
	-loglevel error
	-y
)

encode() {
	local name="$1"
	local filter="$2"
	"${ffmpeg_base[@]}" \
		-f lavfi \
		-i "$filter" \
		-an \
		-c:v libx264 \
		-preset veryfast \
		-crf 18 \
		-pix_fmt yuv420p \
		"$out_dir/$name.mkv"
}

encode clean "testsrc2=size=$size:rate=24:duration=$duration"
encode noisy "testsrc2=size=$size:rate=24:duration=$duration,noise=alls=20:allf=t"
encode animated "testsrc=size=$size:rate=12:duration=$duration,format=yuv420p"
encode mixed-motion "testsrc2=size=$size:rate=24:duration=$duration,setpts=PTS/1.5"
encode low-motion "testsrc2=size=$size:rate=12:duration=$duration,tmix=frames=8"

"${ffmpeg_base[@]}" \
	-f lavfi \
	-i "testsrc2=size=3840x2160:rate=24:duration=$duration,scale=$size" \
	-an \
	-c:v libx264 \
	-preset veryfast \
	-crf 18 \
	-pix_fmt yuv420p \
	"$out_dir/4k-downscale.mkv"
