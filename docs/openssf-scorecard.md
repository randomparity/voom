# OpenSSF Scorecard Maintenance

Issue #347 tracks improving the repository's OpenSSF Scorecard posture. The
repo includes CI changes for the checks that can be managed in version control:
Dependabot configuration, CodeQL analysis, Scorecard publishing, pinned GitHub
Actions, minimal workflow token permissions, cargo-deny, fuzzing, and security
reporting policy.

## GitHub Settings

The following settings require repository administrator access and cannot be
fully enforced from this repository:

- Enable Dependabot alerts.
- Enable Dependabot security updates.
- Enable GitHub vulnerability alerts.
- Enable private vulnerability reporting.
- Protect `main` with required pull request review, required passing CI, no
  force pushes, no deletions, and resolved conversations.

Use routine PR checks as required branch checks. Do not require scheduled fuzz
or mutation workflows, since those jobs are intentionally expensive and run on a
different cadence.

## Verification

After the workflow changes land and the repository settings are enabled, run the
Scorecard workflow manually once, then re-check the published OpenSSF Scorecard
result for `github.com/randomparity/voom`.
