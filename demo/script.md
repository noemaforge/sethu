# Sethu demo film

Target: about three minutes. Keep the final MP4 under five minutes.

## Voiceover

### 1. The pain

An API upgrade can look small in a changelog and still break an application. A response changes shape. One caller fails. Other callers may share the same parser. The developer must find every affected use, make a safe repair, and prove the upgrade works.

### 2. The tool

Start with Vimanam. It compares the old and new API descriptions and identifies breaking changes. Sethu brings those changes into IBM Bob IDE. Bob finds affected code and repairs it. Sethu tracks the evidence.

### 3. The real change

Here we upgrade a small photo picker between two Immich API versions. Vimanam finds twenty-seven changes. One affects random search. It used to return an object. Now it returns an array. The old parser fails on the new response.

### 4. The repair

Bob explores the code and finds the shared parser. A broad change would risk smart search and metadata search. Bob gives random search its own response path and leaves the other callers alone.

### 5. The proof

Sethu runs three checks. The old API passes. The new API breaks the original code in the expected way. The repaired code passes against the new API. The other searches still work.

### 6. The honest result

Bob records what happened for all twenty-seven changes. One removed paging parameter still needs a product decision. Tests cannot decide what a next-page button should do when the API no longer supports paging. Sethu keeps that question visible.

### 7. Closing

The live Bob session reached the event's credit limit before the final report. We generated that report from the saved evidence. Sethu is the bridge for API breaking changes, powered by IBM Bob.

## Picture

| Beat | Picture |
|---|---|
| Pain | A title card with the Sethu tagline, then the picker and the changed response shape. |
| Tool | Vimanam compares API versions, then Sethu and Bob start the upgrade. |
| Change | The 27-change table and random search result. |
| Repair | Bob tracing the parser and editing the random search path. |
| Proof | Old pass, new expected failure, patched pass. |
| Result | The ledger and corrected report with one open paging decision. |
| Close | Bob task summary, Sethu tagline, Narayan SS, documentation URL, and repository URL. |

Use the original Bob footage for every live interaction. Show a clear label when the film switches to the report generated after Bob stopped.
