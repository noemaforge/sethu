# Vimanam 1.3.0 diff fixtures

Real output from the released 1.3.0 binary. Tests read these files. Nothing here is hand written.

- `immich-v1.116.2-v1.117.0.json`: `vimanam diff` over the Immich specs in `../immich/`. Generator name `vimanam`, version `1.3.0`. 46 changes.
- `vimanam-pair.json`: `vimanam diff` over the old and new OpenAPI 3 fixtures that ship with Vimanam. Generator name `vimanam`, version `1.3.0`. 13 changes.
- `operation-id-change.json`: `vimanam diff` over two tiny specs that differ only by an added operation id. Generator name `vimanam`, version `1.3.0`. 1 change with an explicit null old id.

Both runs used `vimanam diff OLD NEW --format json`. Output stays byte stable across runs.
