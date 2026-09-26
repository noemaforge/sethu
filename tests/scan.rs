//! Integration tests for `sethu scan`.
//!
//! Tests build small git repositories with tempfile, tag three spec
//! revisions, and run the built binary over them. Live tests need the
//! released diff binary on PATH and skip with a named reason without it.
//! The gate provides the binary. One test reruns the scan to prove the
//! ranking is byte identical. Other tests delete or move the spec to
//! prove missing revisions report by tag instead of vanishing quietly.

use std::path::Path;

use assert_cmd::Command;

/// First spec revision: shared Item with a nick, widgets with a name,
///
/// and a search body that requires only the query.
const SPEC_V1: &str = r#"openapi: 3.0.0
info:
  title: Demo
  version: 1.0.0
paths:
  /a:
    get:
      operationId: getA
      responses:
        '200':
          description: ok
          content:
            application/json:
              schema:
                $ref: '#/components/schemas/Item'
  /b:
    get:
      operationId: getB
      responses:
        '200':
          description: ok
          content:
            application/json:
              schema:
                $ref: '#/components/schemas/Item'
  /widgets:
    get:
      operationId: getWidgets
      responses:
        '200':
          description: ok
          content:
            application/json:
              schema:
                type: object
                properties:
                  id:
                    type: string
                  name:
                    type: string
                required:
                  - id
  /search:
    post:
      operationId: search
      requestBody:
        content:
          application/json:
            schema:
              type: object
              properties:
                q:
                  type: string
              required:
                - q
      responses:
        '200':
          description: ok
components:
  schemas:
    Item:
      type: object
      properties:
        id:
          type: string
        nick:
          type: string
      required:
        - id
"#;

/// Second revision: Item loses its nick, widgets lose their name, and
/// search newly requires a limit.
const SPEC_V2: &str = r#"openapi: 3.0.0
info:
  title: Demo
  version: 1.1.0
paths:
  /a:
    get:
      operationId: getA
      responses:
        '200':
          description: ok
          content:
            application/json:
              schema:
                $ref: '#/components/schemas/Item'
  /b:
    get:
      operationId: getB
      responses:
        '200':
          description: ok
          content:
            application/json:
              schema:
                $ref: '#/components/schemas/Item'
  /widgets:
    get:
      operationId: getWidgets
      responses:
        '200':
          description: ok
          content:
            application/json:
              schema:
                type: object
                properties:
                  id:
                    type: string
                required:
                  - id
  /search:
    post:
      operationId: search
      requestBody:
        content:
          application/json:
            schema:
              type: object
              properties:
                q:
                  type: string
                limit:
                  type: integer
              required:
                - q
                - limit
      responses:
        '200':
          description: ok
components:
  schemas:
    Item:
      type: object
      properties:
        id:
          type: string
      required:
        - id
"#;

/// Third revision: the second revision without the `/b` operation.
const SPEC_V3: &str = r#"openapi: 3.0.0
info:
  title: Demo
  version: 2.0.0
paths:
  /a:
    get:
      operationId: getA
      responses:
        '200':
          description: ok
          content:
            application/json:
              schema:
                $ref: '#/components/schemas/Item'
  /widgets:
    get:
      operationId: getWidgets
      responses:
        '200':
          description: ok
          content:
            application/json:
              schema:
                type: object
                properties:
                  id:
                    type: string
                required:
                  - id
  /search:
    post:
      operationId: search
      requestBody:
        content:
          application/json:
            schema:
              type: object
              properties:
                q:
                  type: string
                limit:
                  type: integer
              required:
                - q
                - limit
      responses:
        '200':
          description: ok
components:
  schemas:
    Item:
      type: object
      properties:
        id:
          type: string
      required:
        - id
"#;

/// Report whether the released diff binary answers on PATH.
fn vimanam_available() -> bool {
    std::process::Command::new("vimanam")
        .arg("--version")
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

/// Skip the calling test with a named reason when the binary is absent.
fn need_vimanam(test_name: &str) -> bool {
    if vimanam_available() {
        true
    } else {
        eprintln!("SKIP {test_name}: `vimanam` is not on PATH");
        false
    }
}

/// Run one git invocation inside a scratch repository.
fn git(repo: &Path, args: &[&str]) {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Start a scratch repository with an initial commit.
fn init_repo(dir: &Path) {
    git(dir, &["init", "-q"]);
    git(dir, &["config", "user.email", "test@example.com"]);
    git(dir, &["config", "user.name", "Test"]);
}

/// Write one spec revision, commit it, and tag the commit.
fn commit_revision(dir: &Path, name: &str, contents: &str, tag: &str) {
    let path = dir.join(name);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(&path, contents).unwrap();
    git(dir, &["add", "."]);
    git(
        dir,
        &[
            "-c",
            "user.email=test@example.com",
            "-c",
            "user.name=Test",
            "commit",
            "-q",
            "-m",
            tag,
        ],
    );
    git(dir, &["tag", tag]);
}

/// Remove the spec, commit the removal, and tag the commit.
fn commit_removal(dir: &Path, name: &str, tag: &str) {
    git(dir, &["rm", "-q", name]);
    git(
        dir,
        &[
            "-c",
            "user.email=test@example.com",
            "-c",
            "user.name=Test",
            "commit",
            "-q",
            "-m",
            tag,
        ],
    );
    git(dir, &["tag", tag]);
}

/// Run `sethu scan` over one scratch repository.
fn scan_cmd(repo: &Path, spec: &str) -> Command {
    let mut cmd = Command::cargo_bin("sethu").unwrap();
    cmd.arg("scan").arg(repo).arg("--spec").arg(spec);
    cmd
}

/// Read stdout bytes of a successful scan invocation.
fn scan_stdout(repo: &Path, spec: &str) -> Vec<u8> {
    let assert = scan_cmd(repo, spec).assert().success();
    assert.get_output().stdout.clone()
}

#[test]
fn ranks_tag_pairs_deterministically() {
    if !need_vimanam("ranks_tag_pairs_deterministically") {
        return;
    }
    let scratch = tempfile::tempdir().unwrap();
    let repo = scratch.path();
    init_repo(repo);
    commit_revision(repo, "api.yaml", SPEC_V1, "v1.0.0");
    commit_revision(repo, "api.yaml", SPEC_V2, "v1.1.0");
    commit_revision(repo, "api.yaml", SPEC_V3, "v2.0.0");

    let first = scan_stdout(repo, "api.yaml");
    let second = scan_stdout(repo, "api.yaml");
    assert_eq!(first, second, "reruns must agree byte for byte");

    let text = String::from_utf8(first).unwrap();
    assert!(
        text.contains("candidate 1 of 2: v1.0.0 -> v1.1.0"),
        "rich pair stands first: {text}"
    );
    assert!(
        text.contains("candidate 2 of 2: v1.1.0 -> v2.0.0"),
        "endpoint removal stands second: {text}"
    );
    assert!(
        text.contains("fan-out 2 via Item"),
        "shared component fan-out shows: {text}"
    );
    assert!(
        text.contains("1 removed endpoint"),
        "removed endpoint shows: {text}"
    );
    assert!(
        text.contains("ranking is a heuristic"),
        "heuristic caveat shows: {text}"
    );
    assert!(
        text.contains("proves no real consumer impact"),
        "impact caveat shows: {text}"
    );
    assert!(
        text.contains("tool vimanam 1.3.0"),
        "tool version shows: {text}"
    );
}

#[test]
fn reports_missing_spec_by_tag() {
    if !need_vimanam("reports_missing_spec_by_tag") {
        return;
    }
    let scratch = tempfile::tempdir().unwrap();
    let repo = scratch.path();
    init_repo(repo);
    commit_revision(repo, "api.yaml", SPEC_V1, "v1.0.0");
    commit_revision(repo, "api.yaml", SPEC_V2, "v1.1.0");
    commit_removal(repo, "api.yaml", "v2.0.0");

    let assert = scan_cmd(repo, "api.yaml").assert().success();
    let text = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    assert!(
        text.contains("candidate 1 of 1: v1.0.0 -> v1.1.0"),
        "readable pair still ranks: {text}"
    );
    assert!(
        text.contains("pair v1.1.0 -> v2.0.0 not diffed: spec missing at v2.0.0"),
        "undiffed pair names its cause: {text}"
    );
    assert!(
        text.contains("missing v2.0.0"),
        "missing tag reports by name: {text}"
    );
    assert!(
        text.contains("not present at tag"),
        "missing spec says so explicitly: {text}"
    );
}

#[test]
fn reports_moved_spec_by_tag() {
    if !need_vimanam("reports_moved_spec_by_tag") {
        return;
    }
    let scratch = tempfile::tempdir().unwrap();
    let repo = scratch.path();
    init_repo(repo);
    commit_revision(repo, "api.yaml", SPEC_V1, "v1.0.0");
    std::fs::create_dir(repo.join("specs")).unwrap();
    git(repo, &["mv", "api.yaml", "specs/api.yaml"]);
    git(
        repo,
        &[
            "-c",
            "user.email=test@example.com",
            "-c",
            "user.name=Test",
            "commit",
            "-q",
            "-m",
            "move",
        ],
    );
    git(repo, &["tag", "v1.1.0"]);

    let assert = scan_cmd(repo, "api.yaml").assert().success();
    let text = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    assert!(
        text.contains("same name found at specs/api.yaml"),
        "moved spec names its new path: {text}"
    );

    let assert = scan_cmd(repo, "*api.yaml").assert().success();
    let text = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    assert!(
        text.contains("spec path moved from api.yaml to specs/api.yaml"),
        "glob pair names the move: {text}"
    );
}
