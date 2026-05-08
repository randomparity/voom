# VOOM Troubleshooting Guide

## FFmpeg Codec Issues

### "no decoder found" or "Error opening output files: Invalid argument"

These errors typically mean ffmpeg cannot find the codec needed to decode or encode the source file.

**Root cause:** On Fedora (and some other distributions), the default `ffmpeg-free` package excludes patent-encumbered codecs like HEVC (H.265), H.264, and AAC. Files using these codecs will fail with cryptic ffmpeg errors.

**Diagnose:**

```bash
# Check if your ffmpeg has the required decoders
ffmpeg -decoders 2>/dev/null | grep -E 'hevc|h264|aac'

# Check what voom detects
voom plugin info ffmpeg-executor
```

If the `ffmpeg -decoders` output is empty or missing expected codecs, your ffmpeg build lacks them.

**Fix (Fedora with RPM Fusion):**

```bash
# Enable RPM Fusion repositories
sudo dnf install \
  https://mirrors.rpmfusion.org/free/fedora/rpmfusion-free-release-$(rpm -E %fedora).noarch.rpm \
  https://mirrors.rpmfusion.org/nonfree/fedora/rpmfusion-nonfree-release-$(rpm -E %fedora).noarch.rpm

# Replace ffmpeg-free with full ffmpeg
sudo dnf swap ffmpeg-free ffmpeg --allowerasing
```

**Verify:**

```bash
# Decoders should now appear
ffmpeg -decoders 2>/dev/null | grep -E 'hevc|h264|aac'

# voom should detect the codecs
voom env check
```

### Confirming HW encoder/decoder availability

```bash
# Show what voom detects (look for HW Encoders / HW Decoders sections)
voom plugin info ffmpeg-executor

# Query ffmpeg directly
ffmpeg -encoders 2>/dev/null | grep -E 'nvenc|qsv|vaapi'
ffmpeg -decoders 2>/dev/null | grep -E 'cuvid|qsv|vaapi'
```

If `voom plugin info` shows no HW Encoders or HW Decoders sections, your ffmpeg was built without hardware acceleration support, or the required drivers are not installed.

## Hardware Acceleration

### HW encoding works but is slower than expected

VOOM uses software decode with hardware encode (it does not pass `-hwaccel` to ffmpeg). This is intentional: software decoding is broadly compatible and avoids driver-specific decode failures. Performance is dominated by encode time, so the decode path has minimal impact on overall throughput.

### CRF values behave differently with HW encoders

Hardware encoders use different quality parameters than software encoders:

| Encoder | Flag | Notes |
|---------|------|-------|
| x264/x265 (software) | `-crf` | 0-51 scale, lower is better |
| NVENC | `-cq` | Similar scale but quality differs at same value |
| QSV | `-global_quality` | Different scale than software CRF |
| VAAPI | `-qp` | Quantization parameter, not rate factor |

A CRF value that produces good results with x265 will not necessarily produce equivalent quality with a hardware encoder. Test with short clips and adjust.

## General

### Long pause after "Applying policy..."

The discovery and hashing phase can take time on large libraries. A progress spinner shows file count during discovery. If you do not need backup checksums, skip hashing to speed up discovery:

```bash
voom process --no-backup <library> <policy>
```

### Known-bad files blocking processing

Files that previously failed introspection are recorded in the database and skipped on subsequent runs. To manage them:

```bash
# List files marked as bad
voom db list-bad

# Remove bad-file entries so they are retried
voom db purge-bad

# Remove bad-file entries for files that no longer exist on disk
voom db clean-bad

# Force reprocessing of bad files without purging
voom process --force-rescan <library> --policy <policy>
```
