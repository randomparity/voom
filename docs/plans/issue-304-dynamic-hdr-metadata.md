# Issue 304: HDR10+ and Dolby Vision dynamic metadata reinjection

## Objective

Preserve dynamic HDR metadata when VOOM re-encodes supported HEVC HDR
sources. HDR10+ uses `hdr10plus_tool`; Dolby Vision profiles 5, 7, and 8 use
`dovi_tool` RPU extraction and injection.

## External tool behavior

- `hdr10plus_tool extract` writes HDR10+ JSON metadata, and
  `hdr10plus_tool inject -i <input.hevc> -j <metadata.json> -o <output.hevc>`
  injects that metadata into a raw HEVC stream.
- `dovi_tool extract-rpu` writes an RPU sidecar, and
  `dovi_tool inject-rpu -i <input.hevc> --rpu-in <rpu.bin> -o <output.hevc>`
  injects it into a raw HEVC stream.
- VOOM will create temporary raw HEVC sidecars because the current executor
  builds muxed ffmpeg outputs first.

## Implementation Plan

1. Add executor-side dynamic HDR planning.
   - Detect eligible `TranscodeVideo` actions whose source track is HDR10+ or
     Dolby Vision and whose settings preserve HDR.
   - Support only HEVC/H.265 targets and MKV/MP4/Other source containers that
     ffmpeg can demux/remux.
   - Reject Dolby Vision profiles outside 5, 7, and 8 with an actionable
     `ToolExecution` error.
   - Reject missing `hdr10plus_tool` or `dovi_tool` when preservation is
     required.

2. Add the sidecar workflow after the existing ffmpeg muxed transcode succeeds.
   - Extract the source video track to a raw HEVC sidecar for metadata
     extraction.
   - Extract HDR10+ JSON or Dolby Vision RPU from that source sidecar.
   - Extract the encoded output video track to another raw HEVC sidecar.
   - Inject the metadata into the encoded sidecar.
   - Remux the injected stream over the temporary ffmpeg output while copying
     non-video streams and metadata.
   - Clean all sidecars on success and on each failure path.

3. Keep policy/DSL behavior stable.
   - Use existing `preserve_hdr`, `hdr_mode`, and `dolby_vision: copy_rpu`
     settings.
   - Do not add new syntax unless implementation proves it is needed.

4. Add focused tests.
   - Unit-test planning and error handling without real external tools.
   - Unit-test command orchestration using a fake runner.
   - Cover HDR10+, Dolby Vision profiles 5/7/8, unsupported Dolby Vision
     profiles, missing tools, HEVC target enforcement, and cleanup.

5. Add functional test documentation.
   - Use `scripts/generate-test-corpus` for the deterministic HDR10 static
     baseline and policy wiring.
   - Gate real HDR10+/Dolby Vision validation on external sample fixtures and
     tool availability because ffmpeg does not generate representative Dolby
     Vision RPU fixtures by itself.
   - Verify outputs with `hdr10plus_tool extract` / `dovi_tool info` or
     `ffprobe` side-data, not only VOOM plan text.

6. Add user-facing docs and examples.
   - Update `docs/hdr-transcoding.md` with requirements, supported profiles,
     limitations, and verification commands.
   - Add example policy files for HDR10+ and Dolby Vision preservation.
   - Update `docs/examples/README.md`.

## Functional Test Plan

1. Generate the normal corpus:

   ```sh
   scripts/generate-test-corpus /tmp/voom-issue-304-corpus
   ```

2. Run the HDR archival example against the generated HDR10 static fixture to
   prove baseline preserve/tonemap behavior still works.

3. When `hdr10plus_tool` is installed and a small HDR10+ fixture is available:
   - Run a policy that transcodes the fixture to HEVC with HDR preservation.
   - Extract HDR10+ JSON from the output.
   - Assert extraction succeeds and includes dynamic metadata frames.

4. When `dovi_tool` is installed and profile 5, 7, and 8 fixtures are
   available:
   - Run a policy that transcodes each fixture to HEVC with
     `dolby_vision: copy_rpu`.
   - Run `dovi_tool info -i <output-rpu.bin> --summary` or equivalent after
     extracting output RPU.
   - Assert profile metadata is present and the tool reports valid RPU data.

5. Negative functional cases:
   - Temporarily hide `hdr10plus_tool`/`dovi_tool` from `PATH` and assert VOOM
     reports the missing tool instead of producing degraded output.
   - Use a Dolby Vision fixture with an unsupported profile and assert the
     error names the profile and supported profiles.

## Adversarial Review Checklist

- Could VOOM silently drop dynamic metadata when preservation is expected?
- Do errors name the missing tool, unsupported profile, unsupported codec, or
  unsupported container clearly enough for a user to fix the policy?
- Are sidecar files removed on every success and failure path?
- Does the workflow work for both software `libx265` and hardware HEVC encoders?
- Does remuxing preserve non-video streams and container metadata from the
  ffmpeg transcode output?
- Do tests assert behavior and observable commands, not private helper details?
- Do docs avoid claiming ffmpeg can generate real Dolby Vision/HDR10+ fixtures?

