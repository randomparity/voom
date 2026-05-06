# Maps ffprobe -show_streams -show_format -of json output to the canonical
# NDJSON schema used by the policy-audit harness.
#
# Inputs from the caller (passed via --arg):
#   path  : absolute file path (ffprobe doesn't emit it under -show_format
#           when stdin is used)
#   size  : on-disk size in bytes (canonical schema requires it; ffprobe's
#           format.size can be present but is not always reliable)
#   mtime : on-disk mtime in epoch seconds
#
# Output: one JSON object on stdout matching the canonical schema.

def codec_type_to_track_type:
    if . == "video" then "video"
    elif . == "audio" then "audio_main"
    elif . == "subtitle" then "subtitle_main"
    elif . == "attachment" then "attachment"
    else "video" end;

def ext_to_container(p):
    p | ascii_downcase | capture("\\.(?<e>[a-z0-9]+)$") | .e |
    if . == "mkv" or . == "mka" or . == "mks" then "mkv"
    elif . == "mp4" or . == "m4v" or . == "m4a" then "mp4"
    elif . == "avi" then "avi"
    elif . == "webm" then "webm"
    elif . == "flv" then "flv"
    elif . == "wmv" or . == "wma" then "wmv"
    elif . == "mov" then "mov"
    elif . == "ts" or . == "m2ts" or . == "mts" then "ts"
    else "other" end;

def parse_frame_rate:
    if . == null or . == "" then null
    else
        (try ([split("/") | .[] | tonumber] |
              if length == 2 and .[1] != 0 then .[0] / .[1] else null end)
         catch null)
    end;

def map_video_stream:
    {
        index: .index,
        codec: (.codec_name // "unknown"),
        language: (.tags.language // "und"),
        title: (.tags.title // ""),
        is_default: ((.disposition.default // 0) == 1),
        is_forced: ((.disposition.forced // 0) == 1),
        track_type: (.codec_type | codec_type_to_track_type),
        width: (.width // null),
        height: (.height // null),
        frame_rate: (.r_frame_rate | parse_frame_rate),
        is_vfr: false,
        is_hdr: ((.color_transfer // "") | test("smpte2084|arib-std-b67")),
        hdr_format: (
            if (.color_transfer // "") == "smpte2084" then "PQ"
            elif (.color_transfer // "") == "arib-std-b67" then "HLG"
            else null end
        ),
        pixel_format: (.pix_fmt // null)
    };

def map_audio_stream:
    {
        index: .index,
        codec: (.codec_name // "unknown"),
        language: (.tags.language // "und"),
        title: (.tags.title // ""),
        is_default: ((.disposition.default // 0) == 1),
        is_forced: ((.disposition.forced // 0) == 1),
        channels: (.channels // null),
        channel_layout: (.channel_layout // null),
        sample_rate: (.sample_rate | if . == null or . == "" then null else tonumber end),
        bit_depth: (.bits_per_raw_sample | if . == null or . == "" then null else tonumber end),
        track_type: (.codec_type | codec_type_to_track_type)
    };

def map_subtitle_stream:
    {
        index: .index,
        codec: (.codec_name // "unknown"),
        language: (.tags.language // "und"),
        title: (.tags.title // ""),
        is_default: ((.disposition.default // 0) == 1),
        is_forced: ((.disposition.forced // 0) == 1),
        track_type: (.codec_type | codec_type_to_track_type)
    };

def map_attachment_stream:
    {
        index: .index,
        codec: (.codec_name // "unknown"),
        language: (.tags.language // "und"),
        title: (.tags.title // ""),
        is_default: ((.disposition.default // 0) == 1),
        is_forced: ((.disposition.forced // 0) == 1),
        track_type: "attachment",
        filename: (.tags.filename // "")
    };

{
    path: $path,
    size: ($size | tonumber),
    mtime: ($mtime | tonumber),
    container: (ext_to_container($path)),
    duration: (.format.duration | if . == null or . == "" then 0 else tonumber end),
    bitrate: (.format.bit_rate | if . == null or . == "" then null else tonumber end),
    content_hash: null,
    video: [.streams[] | select(.codec_type == "video") | map_video_stream],
    audio: [.streams[] | select(.codec_type == "audio") | map_audio_stream],
    subtitle: [.streams[] | select(.codec_type == "subtitle") | map_subtitle_stream],
    attachment: [.streams[] | select(.codec_type == "attachment") | map_attachment_stream]
}
