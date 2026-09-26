# Demo guide

The demo migrates a small photo picker across one real contract change. This page names the inputs, the break, the repair, and the open decision.

## Inputs

The old spec is Immich v1.116.2 and the new spec is Immich v1.117.0. Both are OpenAPI 3.0.0 documents. Unmodified copies live under `tests/fixtures/immich/`, with release commits and hashes in the NOTICE beside them. The Vimanam capture of the pair reports 27 required changes across 4 operations.

The consumer is a small Rust crate using blocking requests and typed JSON. It runs random, smart, and metadata search through one shared response parser and collects asset IDs for the picker. It reads the server address from the environment or a flag, so tests can point at the stub.

## The break

POST `/search/random` changes its success response from `SearchResponseDto` (with `assets.items`) to an array of `AssetResponseDto`. The operation switched its reference while the old component stayed in use by smart and metadata search, so the change carries an operation origin.

Against the new fixtures, the typed decode of the array into the old shape fails. The random picker test fails with its expected assertion message. The failure lands after the stub served, validated, and answered the new array response, which the request trace confirms.

## The repair

The naive repair changes the shared parser to expect an array. Random search passes, and smart and metadata search break, because they still receive the old shape. The guard checks catch this in the patched stage.

The correct repair gives random search its own path and leaves the shared parser alone. The regression check and both guards then pass against the new fixtures.

## The open decision

The old random search request sends `page`. The new contract drops paging for random search. The pinned server accepts the extra field and strips it, so the wire stays quiet while the meaning is gone.

The picker offers a next page of random assets, so pagination matters to it. The migration therefore keeps one `decision_required` item: drop paging, repeat calls with deduplication, or move stable paging to another endpoint. That item blocks readiness until a human decides.

## Running it

Install the pack into a checkout of the demo consumer, initialise the pair, and follow the workflow in the README. Drive the repair from Bob IDE through `/api-upgrade`, and let `verify`, `check`, and `report` confirm each stage.
