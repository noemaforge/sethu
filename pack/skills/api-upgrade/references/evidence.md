# Evidence rules

A claim is only as strong as its evidence. Free text never verifies a fix.

## What counts as evidence

- A recorded verification run for the current patched commit, with its test results and the stub request trace that shows the expected exchange happened.
- Code locations as path and line, naming the affected symbols.
- Trace summaries that name the repository, the inspected revision, the searched paths, and the search bounds.
- Named checks from the verification manifest, matched to the change IDs they cover.

`fixed_and_verified` needs a real passing run for the current patched state. A prose summary of the repair is not enough on its own.

## What goes stale

- Verification binds to the commit that ran and to the harness hash. A new commit on the upgrade branch marks affected evidence stale.
- A changed harness supersedes every run made with the old one. All stages rerun against the new harness.
- A green run is never reused for a different commit or a different harness, even when the spec hashes are unchanged.
- Search findings bind to the inspected scope and commit. Wider claims need wider searches.

## What blocks a claim

A scenario that touches an unsupported construct cannot support a verification claim. Its stages count as unsupported. `record` refuses `fixed_and_verified` for any ID that relies only on such a scenario. Unexpected stub requests fail visibly instead of passing quietly.
