# Env Check Timeline

Samples: 3

| sample | status | gpus | ffmpeg | rclone |
|---:|---|---:|---|---|
| 1 | NVENC OK | 2 | OK | OK |
| 2 | NVENC FAIL | 0 | OK | OK |
| 3 | NVENC OK | 2 | OK | OK |

## State Transitions

- sample 2: NVENC OK -> FAIL; GPUs 2 -> 0
- sample 3: NVENC FAIL -> OK; GPUs 0 -> 2
