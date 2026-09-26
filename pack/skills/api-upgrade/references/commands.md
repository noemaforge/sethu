# Sethu command reference

Every flag below comes from the built binary `--help` output. Every command accepts `--attempt <ID>` to select a migration attempt by ID or unique prefix. Run `sethu capabilities --json` before any other command. Never run a command whose reported status is not `available`.

## `sethu capabilities`

Report what sethu can do in this environment.

```text
Usage: sethu capabilities [OPTIONS]

Options:
      --attempt <ID>  Select a migration attempt by ID or unique prefix
      --json          Output as JSON
```

The JSON document carries `schema_version`, `sethu`, and `pack` version strings. Its `vimanam` object carries `found` and `supported`, plus `version` when the probe reads one. Its `git` and `nextest` objects carry `found`, plus `version` when the probe reads one. Its `commands` object lists `install`, `init`, `changes`, `context`, `record`, `check`, `stub`, `verify`, `report`, and `scan`. Each entry carries `status` as `available`, `planned`, or `unavailable`. An `unavailable` entry always carries a `reason`.

## `sethu install <REPO>`

Install the Bob pack into a consumer repository.

```text
Usage: sethu install [OPTIONS] <REPO>

Arguments:
  <REPO>  Path to the consumer repository
```

## `sethu init <OLD> <NEW>`

Initialise a migration from a pair of OpenAPI specs.

```text
Usage: sethu init [OPTIONS] <OLD> <NEW>

Arguments:
  <OLD>  Path to the old OpenAPI spec
  <NEW>  Path to the new OpenAPI spec

Options:
      --repo <PATH>   Path to the consumer repository (defaults to the current directory) [default: .]
      --scope <PATH>  Restrict the investigation to this path (repeatable)
      --list          List existing attempts for this spec pair instead of initialising
```

## `sethu changes`

List the changes that need investigating.

```text
Usage: sethu changes [OPTIONS]

Options:
      --all           Include non-breaking changes in the output (shown separately)
      --group <N>     Split output into groups of this size
      --json          Output as JSON
```

## `sethu context <ID>`

Show context for one change ID.

```text
Usage: sethu context [OPTIONS] <ID>

Arguments:
  <ID>  The change ID to retrieve context for

Options:
      --level <LEVEL>   Detail level to request from vimanam [default: endpoint]
                        Possible values:
                        - overview: High-level summary of all endpoints
                        - endpoint: Standard endpoint detail
                        - schema:   Full schema detail
      --prepare <GROUP> Write context for a whole group of IDs instead of printing one
```

## `sethu record <ID>`

Record the outcome and evidence for one change ID.

```text
Usage: sethu record [OPTIONS] --outcome <OUTCOME> <ID>

Arguments:
  <ID>  The change ID to record an outcome for

Options:
      --outcome <OUTCOME>  The outcome to record
                           Possible values:
                           - fixed_and_verified:        The change was fixed and the fix was verified by a passing run
                           - unaffected_in_application: The application is not affected within the inspected scope
                           - no_usage_found:            No usage was found within the inspected scope
                           - decision_required:         A product decision is needed before this can be resolved
                           - unresolved:                The outcome is still unknown or a fix is still failing
      --evidence <REF>     An evidence reference (repeatable)
      --note <TEXT>        A free-text note to attach to this record
```

## `sethu check`

Report accounting completeness and upgrade readiness.

```text
Usage: sethu check [OPTIONS]

Options:
      --require-ready  Fail unless every required change is also ready (not just accounted for)
      --json           Output as JSON
```

## `sethu stub`

Start a local fixture server for one version of the spec.

```text
Usage: sethu stub [OPTIONS] --version <VERSION>

Options:
      --version <VERSION>  Which spec version to serve fixtures for
                           Possible values:
                           - old: The old (pre-upgrade) spec
                           - new: The new (post-upgrade) spec
      --port <PORT>        TCP port to bind to (0 means any free port) [default: 0]
      --scenarios <DIR>    Directory containing fixture scenarios
```

## `sethu verify`

Run verification checks against the stub and record the results.

```text
Usage: sethu verify [OPTIONS]

Options:
      --freeze           Record the harness hash before repair starts
      --manifest <PATH>  Path to the verification manifest
      --check <NAME>     Run only these named checks (repeatable)
```

## `sethu report`

Generate a migration report.

```text
Usage: sethu report [OPTIONS]

Options:
      --format <FORMAT>  Output format(s) to generate [default: both]
                         Possible values:
                         - md:   Markdown only
                         - html: HTML only
                         - both: Both Markdown and HTML
      --out <DIR>        Directory to write the report into
```

## `sethu scan <REPO>`

Walk a spec repository tag history and find candidate upgrade pairs.

```text
Usage: sethu scan [OPTIONS] --spec <PATH> <REPO>

Arguments:
  <REPO>  Path to the git repository to scan

Options:
      --spec <PATH>     Path to the spec file within the repository
      --tags <PATTERN>  Tag pattern to walk (default matches all tags) [default: *]
```
