# Sethu

Sethu traces OpenAPI changes into a consumer application, repairs the affected code, and records evidence that an independent checker can validate.

Vimanam diffs two OpenAPI specs and reports stable change records. Sethu stores those records unchanged, links each one to the code it touches, and refuses to close a change without evidence. Every ledger entry points at a run, a file, or a recorded decision.

The workflow runs inside Bob IDE. The developer invokes `/api-upgrade OLD NEW` there, and Bob traces usage, repairs code, and explains what remains. The Sethu CLI supplies the facts and the checks that Bob works with.

## Commands

| Command | What it does |
|---|---|
| `install` | Install the Bob pack into a consumer repository |
| `init` | Start a migration from a pair of OpenAPI specs |
| `changes` | List the changes that need investigating |
| `context` | Show focused contract context for one change ID |
| `record` | Record one change outcome with its evidence |
| `check` | Report accounting completeness and upgrade readiness |
| `stub` | Serve versioned fixtures for one spec version |
| `verify` | Run checks against the stub and record the results |
| `report` | Generate Markdown and HTML migration reports |
| `scan` | Walk a spec history and find candidate upgrade pairs |
| `capabilities` | Report what Sethu can do in this environment |

Run `sethu <command> --help` for flags. Run `sethu capabilities` to confirm the tool setup before a migration.

## Installation

Sethu requires Vimanam 1.3.0 or later on PATH. Verification runs also need cargo-nextest. Install both tools separately.

Until the first Sethu release appears, build it from source with Rust 1.96 or later.

```bash
git clone https://github.com/noemaforge/sethu
cd sethu
cargo build --release
```

The binary lands at `target/release/sethu`. Copy it onto PATH. Install the helpers from crates.io.

```bash
cargo install vimanam
cargo install cargo-nextest
```

After publication, install Sethu with `cargo install sethu --locked`. Versioned GitHub Releases will also provide Linux, macOS, and Windows binary archives with SHA-256 checksums. The binary archives include the Bob pack. They do not include Vimanam or cargo-nextest.

## Workflow

Install the pack, capture the contract change, then work through the required set.

```bash
sethu install ./consumer
sethu init --repo ./consumer old.json new.json
sethu changes
sethu context <change-id>
sethu record <change-id> --outcome unaffected_in_application --evidence src/search.rs:42
sethu check --require-ready
sethu report --out ./report
```

State lives under `.sethu/` in the consumer repository. It holds the spec pair, the immutable Vimanam capture, the migration manifest, the outcome ledger, and the verification runs. The installer also writes the Bob command, skill, and mode files under `.bob/`, and adds `.sethu/` to `.git/info/exclude`.

`check` separates two questions. Accounting asks whether every required change carries a valid disposition with evidence. Readiness asks whether every change stands resolved, with no open decision and no stale run. An incomplete ledger fails the command.

## Demo

The demo upgrades a small photo picker from Immich v1.116.2 to v1.117.0. The picker reads random, smart, and metadata search through one shared response parser and collects asset IDs.

POST `/search/random` changes its success response from `SearchResponseDto` to an array of `AssetResponseDto`. The typed decode of the new array into the old shape fails, so the random picker test fails against the new fixtures with its expected message. The correct repair gives random search its own path and leaves the shared parser alone. Guard checks cover smart and metadata search, and they pass in every stage.

The old random search request sends `page`. The new contract drops paging for random search, so the picker keeps an open product decision about its next-page behaviour. That item stays `decision_required` and blocks readiness.

The demo consumer lives in its own repository at `noemaforge/sethu-demo-picker`. The pinned specs stay unmodified under `tests/fixtures/immich/`.

## Prior work

Vimanam through version 1.1.0 predates this project. The JSON diff extension (version 1.2.0) and exact operation selection (version 1.3.0) were built during the event and released on 26 September 2026. Sethu, its Bob pack, its fixtures, and the demo consumer were built on 26 and 27 September 2026. The repository log carries those dates.

Sethu was built with coding agents outside Bob, as the event permits. The workflow itself exists only inside Bob IDE. Bob performs the trace and the repair, and the CLI provides facts and checks.

## License

Sethu carries Apache-2.0. The pinned Immich specs carry AGPL-3.0-only and stay unmodified.
