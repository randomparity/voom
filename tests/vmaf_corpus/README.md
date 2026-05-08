# VMAF Integration Corpus

This corpus is generated from `ffmpeg` lavfi sources, so it is reproducible and
does not contain copyrighted media.

Generate the default corpus:

```bash
tests/vmaf_corpus/generate.sh tests/vmaf_corpus/generated
```

The integration test uses a temporary short corpus so normal checkouts do not
need to carry binary video fixtures.
