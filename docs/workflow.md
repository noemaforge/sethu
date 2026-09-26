# Workflow reference

This page describes each Sethu command as the built binary behaves. Run `sethu <command> --help` to confirm flags on any release.

## Start

`install REPO` copies the Bob pack into a consumer repository. It creates the `/api-upgrade` command, the skill with its references, and the migrator mode under `.bob/`. It merges owned modes by slug, records owned file hashes in `.sethu/installation.json`, adds `.sethu/` to `.git/info/exclude`, and writes a nextest profile into `.config/nextest.toml`. It never overwrites a user-edited owned file silently.

`init OLD NEW` runs the pinned Vimanam over the spec pair and stores the output unchanged as an immutable capture. It copies both specs into `.sethu/pairs/`, binds the attempt to the consumer baseline commit and the declared scope, and resumes an existing attempt only on an exact match.

## Understand

`changes` lists the required set by ID, method, path, kind, and severity. It groups records by shared origin, so one changed component reaching several operations reads as fan-out. `--all` adds non-breaking changes in a separate section. `--group N` splits the output for parallel tracing. `--json` emits machine-readable output.

`context ID` returns the unmodified change record plus readable old and new endpoint detail rendered through exact operation selection. `--level` selects overview, endpoint, or schema detail. `--prepare GROUP` writes context artifacts for a whole group under `.sethu/`, so read-only tracers can work without running commands.

## Account

`record ID --outcome OUTCOME --evidence REF` writes one disposition with its evidence references. Re-recording replaces the current disposition and appends the old one to a per-ID history. Free text alone never satisfies verification. A `fixed_and_verified` outcome needs a passing run on the current patched commit.

`check` recomputes the required set from the attempt capture and validates the ledger against it. It reports accounting and readiness as separate results. It exits nonzero on an incomplete ledger, and `--require-ready` additionally fails on unresolved items.

## Reproduce and verify

`stub --version old|new` serves fixed fixture scenarios from a directory on `127.0.0.1`. It validates incoming requests against the selected spec and answers from declared fixtures. An unexpected route returns status 599 and lands in the request trace, so it can never pass as an ordinary 404.

`verify` overlays a frozen test harness onto worktrees of the original and patched commits and runs each named check against its own stub instance. `verify --freeze` records the harness hash before repair starts. A regression check passes only when the test result and the stub trace agree: the expected failure message appears, and the expected exchange arrived, validated, and answered. Anything else is invalid.

## Explain and discover

`report --out DIR` writes Markdown and HTML from validated state. It covers provenance, tool versions, accounting and readiness, every required change with its evidence, and the remaining blockers. Missing or stale evidence appears marked, never silently dropped.

`scan REPO --spec PATH` walks release tags in version order, diffs adjacent spec pairs with Vimanam, and ranks them by demo value. Candidate pairs carry provenance and explicit limits. Ranking stays a heuristic.

`capabilities` reports the Sethu and pack versions, the Vimanam, git, and nextest versions found, and the status of every command. The skill consults it before invoking anything.
