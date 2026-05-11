# CLI Agent-Friendliness Audit

Issue: <https://github.com/randomparity/voom/issues/345>

This audit captures the starting point for the CLI tree before the
agent-friendly output work. It should be updated as phases land.

## Output Rules Under Review

- Machine-readable stdout must be valid for the requested format.
- Human status, progress, warnings, and deprecation notices belong on stderr.
- Destructive commands need a tested non-interactive path.
- JSON output should prefer stable command-specific objects or natural domain
  data over prose strings.

## Command Inventory

| Command | Mutates state | Format flag | JSON support | Empty JSON shape | Prompts | Progress/status notes |
|---|---:|---:|---:|---|---:|---|
| `scan` | yes | partial | yes | `[]` | no | progress/status should be quiet for machine formats |
| `inspect` | no | yes | yes | n/a | no | may introspect missing DB entries |
| `process` | yes | no | `--plan-only` only | `[]` for no plans | optional approval | long-running progress/status |
| `estimate` | yes, calibration data | no | no | n/a | no | summary is human-only |
| `policy list` | no | no | no | n/a | no | human-only |
| `policy validate` | no | no | no | n/a | no | result split between stdout/stderr |
| `policy show` | no | no | no | n/a | no | prints human summary plus JSON block |
| `policy format` | yes | no | no | n/a | no | file-editing command |
| `policy diff` | no | no | no | n/a | no | human diff only |
| `policy fixture extract` | no | no | yes | n/a | no | always JSON today |
| `policy test` | no | yes | yes | summary object | no | uses `--format json` |
| `plugin list` | no | no | no | n/a | no | human-only |
| `plugin info` | no | no | no | n/a | no | human-only |
| `plugin enable` | config | no | no | n/a | no | status text |
| `plugin disable` | config | no | no | n/a | no | status text |
| `plugin install` | config/files | no | no | n/a | no | status text |
| `jobs list` | no | no | no | n/a | no | human table |
| `jobs status` | no | no | no | n/a | no | human details |
| `jobs cancel` | yes | no | no | n/a | no | status text |
| `jobs retry` | yes | no | no | n/a | no | status text |
| `jobs clear` | yes | no | no | n/a | yes | command/global yes paths need coverage |
| `report` | no, except `--snapshot` | yes | yes | object/arrays by section | no | mixed section output |
| `files list` | no | yes | yes | `[]` | no | query command |
| `files show` | no | yes | yes | n/a | no | query command |
| `files delete` | yes | no | no | n/a | yes | global yes path should be covered |
| `plans show` | no | yes | yes | `[]` | no | query command |
| `events` | no | yes | yes | `[]` | no | follow mode streams JSON lines |
| `env check` | yes, stores checks | yes | yes | status object | no | uses `--format json` |
| `env history` | no | yes | yes | `[]` | no | query command |
| `health` | yes, deprecated alias | yes | yes | status object | no | deprecation warning on stderr |
| `doctor` | yes, deprecated alias | no | no | n/a | no | deprecation warning on stderr |
| `serve` | no | no | no | n/a | no | long-running server command |
| `db prune` | yes | no | no | n/a | no | has dry-run |
| `db vacuum` | yes | no | no | n/a | no | status text |
| `db reset` | yes | no | no | n/a | yes | command/global yes paths need coverage |
| `db list-bad` | no | yes | yes | `[]` | no | query command |
| `db purge-bad` | yes | no | no | n/a | no | status text |
| `db clean-bad` | yes | no | no | n/a | yes | command/global yes paths need coverage |
| `config show` | no | no | no | n/a | no | redacted human TOML |
| `config edit` | yes | no | no | n/a | external editor | interactive by design |
| `config get` | no | no | no | n/a | no | value text |
| `config set` | yes | no | no | n/a | no | status text |
| `tools list` | no | yes | yes | `[]` | no | query command |
| `tools info` | no | yes | yes | n/a | no | query command |
| `verify run` | yes | partial | yes | summary object | no | progress/status |
| `verify report` | no | yes | yes | `[]` | no | query command |
| `history` | no | yes | yes | `[]` | no | query command |
| `backup list` | no | yes | yes | `[]` | no | query command |
| `backup restore` | yes | no | no | n/a | yes | command/global yes paths need coverage |
| `backup verify` | no | yes | yes | `[]` | no | query command |
| `backup cleanup` | yes | no | no | n/a | yes | command/global yes paths need coverage |
| `bug-report generate` | writes report dir | no | file output | n/a | no | local-first report generation |
| `bug-report upload` | external GitHub write | no | no | n/a | no | uses `gh` |
| `init` | yes | no | no | n/a | no | human setup wizard output |
| `completions` | no | no | shell text | n/a | no | machine output by shell format |

## Phase 1 Test Coverage

The initial contract tests cover:

- Existing JSON stdout parses for `scan --format json`, `tools list --format
  json`, and `env check --format json`.
- JSON stdout excludes representative human status text.
- Human scan status is emitted on stderr for table/human output.
