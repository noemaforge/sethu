# Upgrade outcomes

Five outcomes describe what happened to one change ID. Every record carries exactly one outcome. Vimanam severity stays unchanged. Outcomes form a separate layer above it.

| Outcome | Meaning | Minimum evidence | Readiness |
|---|---|---|---|
| `fixed_and_verified` | The application was affected and now passes verification | Affected symbols and paths, a repair reference, a named relevant check, and a passing run on the current patched commit | Resolved within the verified scope |
| `unaffected_in_application` | The application was inspected and its behaviour stays compatible | The inspected usage, why the behaviour stays compatible, and supporting code or test evidence | Resolved within the inspected scope |
| `no_usage_found` | No usage was found within the inspected scope | Repository and commit, inspected paths, search and tracing methods including wrappers and generated clients, and explicit exclusions | A scoped finding only, never a claim of universal absence |
| `decision_required` | A product decision is needed before this change can close | The specific ambiguity, the options, their consequences, and the decision needed | Blocks readiness |
| `unresolved` | The outcome is still unknown or a fix is still failing | What is still unknown or failing, what was tried, and the next action | Blocks readiness |

Both breaking and review IDs need accounting. A review item never closes on informal judgement alone. One shared repair can cover several IDs, but each ID keeps its own disposition.
