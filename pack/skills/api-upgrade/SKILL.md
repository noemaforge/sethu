---
name: api-upgrade
description: Trace an OpenAPI change into this application, repair callers, and record evidence
---

Upgrade this application across one OpenAPI change, from an old spec to a new spec.

Start by loading repository instructions (`AGENTS.md` and project rules). Then run `sethu capabilities --json` and read its `commands` map. The map reports one of three states per command. `available` means the command runs in this environment. `planned` means this release declares the command without implementing it yet. `unavailable` means the command cannot run here, and its `reason` names what is missing.

Work through the steps below in order. Before each step, look up its command in the capabilities document. If the status is not `available`, stop there. Tell the developer which command stopped the run, quote its status and reason, and explain what would unblock it. Never skip a stopped step. Never run a later step after an earlier one stops.

1. If the document reports `init` as `available`, run `sethu init OLD NEW` for the spec pair, or resume the matching migration. Otherwise stop and report what is missing.
2. If the document reports `changes` as `available`, run `sethu changes` and split the required set into bounded groups. Otherwise stop and report what is missing.
3. If the document reports `context` as `available`, run `sethu context --prepare` for each group. Launch one `explore` tracer per group with its change IDs, scope paths, and prepared context paths. Otherwise stop and report what is missing.
4. Tracers stay read-only. They read prepared context and application source. They run no commands and write no files. They return evidence references and uncertainties, never bare conclusions.
5. Merge shared symbols and overlapping findings before editing. Give each change ID its own disposition even when one repair covers several IDs.
6. If the document reports `stub` and `verify` as `available`, serve fixtures and run the verification checks for each repair. Otherwise stop and report what is missing.
7. If the document reports `record` as `available`, record each change ID with its outcome and evidence. Otherwise stop and report what is missing.
8. If the document reports `check` as `available`, run `sethu check --require-ready` and keep working through actionable failures. Surface real product decisions and unresolved cases to the developer. Otherwise stop and report what is missing.
9. If the document reports `report` as `available`, run `sethu report` and explain the results within the declared scope only. Otherwise stop and report what is missing.

Only this workflow edits code, runs commands, and writes the ledger. A tracer that needs unprepared context says so instead of running commands. The migrator prepares the context and relaunches the tracer.

Outcome definitions live in `references/outcomes.md`. Evidence rules live in `references/evidence.md`. Command flags live in `references/commands.md`.
