# Functional Test Plan: Issue 337 Bug Report CLI

Issue: <https://github.com/randomparity/voom/issues/337>

## Setup

Generate a small corpus with a real-looking filename and create a policy that
references that filename plus a fake token value:

```sh
mkdir -p /tmp/voom-issue-337-corpus
touch "/tmp/voom-issue-337-corpus/The Movie (2026).mkv"
cat > /tmp/voom-issue-337-policy.voom <<'EOF'
rule bug_report_redaction_check {
  when file.name == "The Movie (2026).mkv"
  set metadata.api_key = "sk-issue337"
}
EOF
```

Run a VOOM command that records database context for the corpus:

```sh
voom scan /tmp/voom-issue-337-corpus --no-hash
```

## Generate

Run:

```sh
voom bug-report generate \
  --out /tmp/voom-issue-337-report \
  --policy /tmp/voom-issue-337-policy.voom \
  --library /tmp/voom-issue-337-corpus
```

Expected files:

- `/tmp/voom-issue-337-report/report.md`
- `/tmp/voom-issue-337-report/report.json`
- `/tmp/voom-issue-337-report/redactions.public.json`
- `/tmp/voom-issue-337-report/redactions.local.json`
- `/tmp/voom-issue-337-report/metadata.json`
- `/tmp/voom-issue-337-report/README.txt`

Expected redaction behavior:

```sh
rg "video000\\.mkv|<api-key-001>|<secret-001>|<token-001>" \
  /tmp/voom-issue-337-report/report.md \
  /tmp/voom-issue-337-report/report.json

! rg "The Movie \\(2026\\)|sk-issue337|voom-issue-337-corpus" \
  /tmp/voom-issue-337-report/report.md \
  /tmp/voom-issue-337-report/report.json

rg "The Movie \\(2026\\)|sk-issue337" \
  /tmp/voom-issue-337-report/redactions.local.json
```

The first command should find sanitized placeholders. The second command should
find no private values in shareable files. The third command should show that
the private mapping exists only in `redactions.local.json`.

## Upload

With `gh` installed and authenticated, upload to a test issue:

```sh
voom bug-report upload /tmp/voom-issue-337-report \
  --issue 337 \
  --repo randomparity/voom
```

Expected:

- The issue comment contains the `report.md` contents.
- The issue comment contains sanitized placeholders such as `video000.mkv`.
- The issue comment does not contain `The Movie (2026).mkv`,
  `/tmp/voom-issue-337-corpus`, or `sk-issue337`.
- `redactions.local.json` is not referenced or uploaded by the command.
