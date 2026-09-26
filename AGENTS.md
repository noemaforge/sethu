# Sethu: agent protocol

Binding for every human and coding agent working in this repository. It governs how work runs.
The design lives in [`dev-diary/design.md`](dev-diary/design.md).

Read this file in full before you change anything. **This is the only file that says how work runs.**
If another document repeats a rule, this file wins and the copy gets deleted.

**`dev-diary/` is gitignored for now.** It exists only in the main checkout at
`/home/nryn/work/sethu/dev-diary/`. A worktree has no copy, so every agent reads the plan there.

## What Sethu is

A Rust CLI and a Bob pack. In Bob IDE, `/api-upgrade OLD NEW` takes an OpenAPI change from Vimanam's
diff, traces it into a consumer application, repairs it, and records evidence that an independent
checker validates. The ten commands, their contracts and the state layout are in the design.

## The loop

Every task runs the same cycle. Implement, review, remediate, re-review. Repeat until a round returns
APPROVE with zero findings across all severities.

1. **Implement.** The orchestrator opens a worktree and dispatches an implementer. The implementer
   edits only the task's `owns` paths, commits, and hands back the commit hash.
2. **Review.** A fresh reviewer reviews that hash and nothing else, in its own detached worktree. It
   writes its review file.
3. **Remediate.** On REMEDIATE, a remediator fixes every finding as new commits on the reviewed
   commit. It writes its remediation file and hands back the new hash.
4. **Re-review.** Steps 2 and 3 repeat with fresh agents until APPROVE. The approving round claims
   zero residue against every prior round.
5. **Land.** The orchestrator rebases onto `main`, reruns the gate if anything moved, fast-forwards
   `main`, and removes the task's worktrees and branches.

| Level | Meaning |
|---|---|
| **C** | A false verification claim, a lost or overwritten ledger, a destroyed user file, or a spent Bobcoin nobody asked for. |
| **H** | A real defect the demo survives. |
| **M** | A real defect with a workaround. |
| **L** | Polish. |

**Every severity gets fixed.** A reviewer may record a finding as a false positive. That decides
whether the defect is real, never whether a real defect deserves a fix.

A real defect outside the task's `owns` paths is recorded as `OUT_OF_SCOPE`, with its severity and
pin. It never blocks APPROVE. The orchestrator turns it into a task.

## Round files

Round files live in `/home/nryn/work/sethu/dev-diary/adversarial-review/`.

| File | Written by |
|---|---|
| `<task>-impl-handoff.md` | The implementer. What it built, what surprised it, any contract change it needs. |
| `<task>-round<K>.md` | The round K reviewer. Hash, worktree, verdict, counts, one row per finding. |
| `<task>-remediation-round<K>.md` | The round K remediator. One row per finding it fixed. |

Every finding carries five columns.

| Column | Meaning |
|---|---|
| **Severity** | C, H, M or L. |
| **Where** | File path and line number at the reviewed hash. |
| **What** | The observable defect or false claim. |
| **Pin** | A failing test or probe that measures the defect independently. |
| **Mutation** | The one or two line edit that reintroduces the defect. It proves the pin is load-bearing. |

A finding without a pin is not actionable. A finding without a mutation is not load-bearing.

Every review answers these before the verdict.

1. Can any path mark a change verified without a matching run, expected signature and expected exchange?
2. Can any path drop, rename or recompute a Vimanam change ID or severity?
3. Can any write to `.sethu/` or a consumer repository be partial, or overwrite a user's file?
4. Can a tracer path run a command or write a file?
5. Does any code, comment, test or commit cite a task, a round, or a design section?
6. Did this task rebuild something Vimanam provides?
7. Was this review run on the handed hash, in a worktree of its own?

## Worktrees and roles

**Every writer works in its own git worktree and commits.** A commit is the only handoff between
roles. Worktrees live under `/home/nryn/work/sethu-wt/`, outside the repository.

```bash
git -C /home/nryn/work/sethu worktree add -b task/<id> /home/nryn/work/sethu-wt/<id>-impl main
```

```bash
git -C /home/nryn/work/sethu worktree add --detach /home/nryn/work/sethu-wt/<id>-rev-r1 <hash>
```

| Role | Does | Never does |
|---|---|---|
| **Orchestrator** | Picks the task, opens worktrees, hands hashes to reviewers, lands. Edits `dev-diary/`. | Implement a task. Write a verdict. |
| **Implementation** | Builds inside `owns` in its worktree, and commits. | Review its own work. Touch the main checkout. |
| **Review** | Reviews the handed commit in its own worktree and writes the verdict. | Fix what it found. Review uncommitted work. |
| **Remediation** | Fixes every finding as new commits on the reviewed commit. | Change the verdict. Rewrite the reviewed commit. |

**Use a fresh agent per role per round.** Nobody but the orchestrator touches the main checkout, not
even to build. Implementation and remediation commit before they report. The report is the hash plus
one line.

The orchestrator may fix an L directly when the fix cannot change behaviour and the review file names
it. A comment that misdescribes behaviour carries the severity of that behaviour.

## Tasks

Tasks live in `dev-diary/PHASE-*.md`, one file per phase. [`dev-diary/README.md`](dev-diary/README.md)
holds the current status and the task graph. Each task carries this block. Only the orchestrator
edits it.

```yaml
requires: <task ids>
owns:     src/state/
size:     S
bob:      no
status:   not-started
```

- **requires** binds. A shared path is a dependency, so the later task names the earlier one.
- **owns** lists the paths a task writes. No two concurrent tasks own the same path.
- **size** runs XS, S, M, L.
- **bob** is `no`, `candidate` or `person` (see "Bob and Bobcoins").
- **status** is `not-started`, `in-progress:<role>:<worktree>`, `done:<hash>` or `blocked:<reason>`.
  The orchestrator writes it before each handoff, never after.

**Task ids** read `T<phase>.<n>`. A task that needs the person appends `b`, as in `T1.3b`. A number is
never reused.

## The gate

Every commit passes the gate in its own worktree before it is handed over.

```bash
cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo nextest run
```

`tools/check.sh` replaces this line once a task lands it. The gate also fails on plan references in
tracked source files.

## A pin is a measurement

Software reports its own success. That report is the thing under review, not evidence for it.

- **Verification claims.** Read the run artefacts under `runs/`, the JUnit XML and the stub's request
  trace. A green summary line proves nothing.
- **State writes.** Read the files on disk after the command. Kill the process mid-write and read them
  again.
- **Installer.** Diff the consumer repository before and after. Unrelated files must be byte-identical.
- **Vimanam contract.** Test against checked-in output of the released binary, never a hand-written
  imitation.

## Architectural invariants

Breaking one is a design change. Propose it in a handoff file first.

1. **`src/vimanam.rs` is the only code that runs Vimanam.** It reads full stdout and treats exit 3 as
   output.
2. **Vimanam's records are stored unchanged.** Sethu never recomputes a change ID or a severity.
3. **Every state write is atomic.** Write a temp file, then rename. A capture is immutable once written.
4. **Only `sethu record` writes the ledger.** `sethu check` validates it again, since files can be
   edited by hand.
5. **A claim is only as strong as its evidence.** Free text never satisfies verification. An
   unsupported construct blocks the claim instead of passing it.
6. **The stub binds to `127.0.0.1` only.** Sethu makes no other network call.
7. **Output is deterministic.** Use `IndexMap` where order is observable. Diagnostics go to stderr.
8. **The installer never overwrites a file it does not own**, and merges modes by `slug`.

## Vimanam first

Sethu builds on Vimanam `≥ 1.3.0` from crates.io, run as a separate binary. Sethu never grows its own
copy of what Vimanam provides.

- **A bug or gap in Vimanam becomes a GitHub issue** on `noemaforge/vimanam`. The implementer writes it
  up in its handoff with the version, the smallest reproduction, and what it expected. The
  orchestrator files it.
- **Reduce the reproduction to Vimanam's own terms**, with its CLI and published version.
- **Take a workaround if one exists**, and record the issue URL beside it. Otherwise the task goes
  `blocked:` with the URL.
- **Never patch Vimanam from here.** Other repositories on this machine are read-only, except a
  path a task's `owns` line names explicitly (the demo consumer).

## Stack, frozen

- **Rust**, edition 2024, MSRV 1.96, one binary crate. The dependencies are the ones the design lists.
  A new dependency is a design change.
- **Tests:** `cargo nextest`. Integration tests use `assert_cmd` and `tempfile`, never the real home
  directory.
- **External tools at runtime:** `vimanam`, `git`, and `cargo-nextest` for verification.

## Rust style

- No `unwrap` or `expect` outside tests, except on an invariant a comment proves.
- No `panic!` outside `main` and tests. Errors use `anyhow` with context naming the file or command.
- Wire and state shapes are named structs with `serde`, never loose `serde_json::Value`. The one
  exception is spec and schema content, which is data.
- Every public item has a doc comment. Every `Command` spawn sets its working directory and captures
  both streams.

## No plan references in code

Code, comments, tests, error strings and commit messages never cite the plan. That means no task ids,
no `§` marks, no `design.md` references, and no review rounds. A branch name may carry a task id.

A comment states its reason itself.

```rust
// Wrong:
// Treat exit 3 as success (design §5).

// Right:
// Vimanam exits 3 under --fail-on-breaking after writing the full report, so
// the JSON on stdout is still complete and valid.
```

## Bob and Bobcoins

The hackathon account holds 40 Bobcoins with no top-ups. **No agent spends them.** Only the person
runs Bob IDE. A task never needs Bob to pass its gate.

- **`bob: person`** tasks are checks, walkthroughs and the demo. The person runs them in Bob IDE.
- **`bob: candidate`** tasks may be implemented in Bob IDE, at the person's choice, to measure spend.
  Bob then acts as the implementer and keeps every implementer rule. It works in its own worktree,
  edits only `owns`, commits, and passes the gate. Its work goes through the same review loop.
- **Record every Bob task's spend** in the table in `dev-diary/README.md`, with before and after
  gauge readings. Keep the task-header screenshot for `bob_sessions/`.

## Documentation style

These rules cover markdown, code comments and commit messages alike.

1. No semicolons. Split the sentence.
2. No em dashes. Use a full stop, a comma, or parentheses.
3. Sentences run 30 words at most.
4. Active voice, unless passive genuinely reads clearer.

## Secrets

Sethu needs none. No token, key or credential appears in any file, including test fixtures.

## Memory

Lambo is the graph memory sessions share. If your environment has no lambo tools, skip this section.

- **Recall before you read anything else**, and again before touching shared ground.
- **Use one stable `agent_id`** naming the model you run as.
- **Derive decisions and their reasons, not activity.** Git records the activity.
- **Memory being down never blocks work.** Note it in your handoff and carry on.

## Read order

0. `lambo_recall` on your task, if you have lambo.
1. This file, in full.
2. [`dev-diary/design.md`](dev-diary/design.md) at `/home/nryn/work/sethu/dev-diary/`.
3. `dev-diary/README.md`, then your task's block in its `dev-diary/PHASE-*.md`, and any prior
   rounds for it.
4. The commit you were handed, or the worktree you were given.

Then confirm four things. I am in my own worktree, not the main checkout. I own every path I will
edit. The shapes I need already exist. I can validate this task on its own. If any is false, stop
and tell the orchestrator.
