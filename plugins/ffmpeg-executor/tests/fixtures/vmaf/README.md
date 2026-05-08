# VMAF Fixtures

This directory holds synthetic fixture videos for live libvmaf tests.

Regenerate them with:

```sh
ffmpeg -y -f lavfi -i testsrc2=size=320x180:rate=24:duration=1 \
  -c:v libx264 -crf 10 -pix_fmt yuv420p reference.mkv
ffmpeg -y -i reference.mkv -c:v libx264 -crf 35 -pix_fmt yuv420p distorted.mkv
```

The source is generated from FFmpeg's `testsrc2` filter, so the fixtures do not
contain copyrighted source media.
