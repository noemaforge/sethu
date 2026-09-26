---
description: Upgrade this application across an OpenAPI spec change
argument-hint: <old-spec> <new-spec>
---

Upgrade this application from spec `$1` to spec `$2`.

1. Load the `api-upgrade` skill and follow it exactly.
2. Run `sethu capabilities --json` first and refuse unavailable commands.
3. Treat OLD as `$1` and NEW as `$2`. Never swap them.
4. Report evidence for every change ID, or mark it unresolved.
