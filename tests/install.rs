//! Integration tests for `sethu install`.
//!
//! The tests build consumer repositories in temporary directories and run
//! the built binary there. They never touch the real home directory. Each
//! test proves one installer promise by reading files back from disk:
//! unrelated content stays byte identical, repeats change nothing, user
//! edits survive beside their new copies, and every installed pack file
//! passes the documented syntax rules.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use assert_cmd::Command;
use predicates::prelude::*;
use serde::Deserialize;
use sethu::install::{self, FileOutcome};

/// Build the test command for the sethu binary.
fn sethu() -> Command {
    Command::cargo_bin("sethu").unwrap()
}

/// Run git with arguments inside a directory.
///
/// The call must succeed. Output stays captured for failure messages.
fn git(dir: &Path, args: &[&str]) {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Create a consumer git repository with one commit.
fn git_repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    git(dir.path(), &["init"]);
    git(dir.path(), &["config", "user.email", "test@example.com"]);
    git(dir.path(), &["config", "user.name", "Test"]);
    std::fs::write(dir.path().join("README.md"), "# Consumer\n").unwrap();
    git(dir.path(), &["add", "-A"]);
    git(dir.path(), &["commit", "-m", "start"]);
    dir
}

/// Install into a repository and return stdout.
///
/// The install must succeed. Callers assert on the returned text.
fn install_ok(repo: &Path) -> String {
    let assert = sethu().arg("install").arg(repo).assert().success();
    String::from_utf8(assert.get_output().stdout.clone()).unwrap()
}

/// Create a repository with a fresh install inside.
fn installed_repo() -> tempfile::TempDir {
    let dir = git_repo();
    install_ok(dir.path());
    dir
}

/// Snapshot every file below a root, keyed by slash relative path.
///
/// The `.git` tree stays excluded. Callers compare snapshots to prove that
/// unrelated files never change.
fn snapshot(root: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut map = BTreeMap::new();
    walk(root, root, &mut map);
    map
}

/// Walk one directory depth first into a snapshot map.
fn walk(root: &Path, dir: &Path, map: &mut BTreeMap<String, Vec<u8>>) {
    let entries = std::fs::read_dir(dir).unwrap();
    for entry in entries {
        let path = entry.unwrap().path();
        let rel = path.strip_prefix(root).unwrap();
        if rel.components().any(|part| part.as_os_str() == ".git") {
            continue;
        }
        if path.is_dir() {
            walk(root, &path, map);
        } else {
            let key = rel
                .components()
                .map(|part| part.as_os_str().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("/");
            map.insert(key, std::fs::read(&path).unwrap());
        }
    }
}

/// Expected owned targets from the documented pack layout.
fn expected_owned_targets() -> Vec<String> {
    vec![
        ".bob/commands/api-upgrade.md",
        ".bob/skills/api-upgrade/SKILL.md",
        ".bob/skills/api-upgrade/references/commands.md",
        ".bob/skills/api-upgrade/references/evidence.md",
        ".bob/skills/api-upgrade/references/outcomes.md",
    ]
    .into_iter()
    .map(str::to_string)
    .collect()
}

/// Check whether the released nextest runner answers on PATH.
fn nextest_present() -> bool {
    std::process::Command::new("cargo")
        .arg("nextest")
        .arg("--version")
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .is_ok_and(|output| output.status.success())
}

/// PATH with the cargo bin directory first, so the runner resolves.
///
/// Returns None when the home directory is unknown. Callers skip the
/// runner probe instead of guessing paths.
fn path_with_cargo_bin() -> Option<std::ffi::OsString> {
    let home = std::env::var_os("HOME")?;
    let mut parts = vec![PathBuf::from(home).join(".cargo").join("bin")];
    if let Some(path) = std::env::var_os("PATH") {
        parts.extend(std::env::split_paths(&path));
    }
    std::env::join_paths(parts).ok()
}

#[test]
fn fresh_install_writes_owned_files_and_record() {
    let dir = git_repo();
    let stdout = install_ok(dir.path());

    let pack = install::pack_version().unwrap();
    assert!(stdout.contains(&pack), "stdout names the pack version");

    let owned = install::owned_files().unwrap();
    let targets: Vec<String> = owned.iter().map(|file| file.target.clone()).collect();
    assert_eq!(targets, expected_owned_targets());
    for file in &owned {
        let bytes = std::fs::read(dir.path().join(&file.target)).unwrap();
        assert_eq!(bytes, file.bytes, "{} differs from the pack", file.target);
        assert!(
            stdout.contains(&format!("created {}", file.target)),
            "stdout reports {}",
            file.target
        );
    }

    let record = install::read_installation(dir.path())
        .unwrap()
        .expect("install writes a record");
    assert_eq!(record.pack, pack);
    assert_eq!(record.sethu, env!("CARGO_PKG_VERSION"));
    assert_eq!(record.files.len(), owned.len());
    for file in &owned {
        let digest = sethu::state::layout::sha256_hex(&file.bytes);
        assert_eq!(
            record.files.get(&file.target).map(String::as_str),
            Some(digest.as_str())
        );
    }
    assert_eq!(record.owned_modes, vec!["sethu-migrator".to_string()]);

    let modes = std::fs::read_to_string(dir.path().join(".bob/custom_modes.yaml")).unwrap();
    assert!(modes.contains("sethu-migrator"));

    let exclude = std::fs::read_to_string(dir.path().join(".git/info/exclude")).unwrap();
    assert_eq!(
        exclude
            .lines()
            .filter(|line| line.trim() == ".sethu/")
            .count(),
        1
    );

    let nextest = std::fs::read_to_string(dir.path().join(".config/nextest.toml")).unwrap();
    assert!(nextest.contains("[profile.sethu.junit]"));
}

#[test]
fn unrelated_content_survives_byte_identical() {
    let dir = git_repo();
    std::fs::create_dir_all(dir.path().join(".bob/commands")).unwrap();
    std::fs::create_dir_all(dir.path().join(".bob/skills/other")).unwrap();
    std::fs::write(
        dir.path().join(".bob/commands/other.md"),
        "# Other command\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join(".bob/skills/other/SKILL.md"),
        "---\nname: other\ndescription: Unrelated skill\n---\n\nOther work.\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join(".bob/skills/api-upgrade-note.md"),
        "Local note.\n",
    )
    .unwrap();
    let reviewer = "customModes:\n  - slug: reviewer\n    name: Reviewer\n    roleDefinition: Review only.\n    groups:\n      - read\n";
    std::fs::write(dir.path().join(".bob/custom_modes.yaml"), reviewer).unwrap();
    std::fs::create_dir_all(dir.path().join(".config")).unwrap();
    std::fs::write(
        dir.path().join(".config/nextest.toml"),
        "[profile.default]\nretries = 2\n",
    )
    .unwrap();
    std::fs::create_dir_all(dir.path().join(".git/info")).unwrap();
    std::fs::write(dir.path().join(".git/info/exclude"), "*.log\n").unwrap();
    std::fs::write(dir.path().join("src.txt"), "consumer source\n").unwrap();

    let before = snapshot(dir.path());
    install_ok(dir.path());
    let after = snapshot(dir.path());

    let owned: std::collections::HashSet<String> = expected_owned_targets().into_iter().collect();
    let merged = [
        ".bob/custom_modes.yaml",
        ".sethu/installation.json",
        ".config/nextest.toml",
        ".git/info/exclude",
    ];
    for (path, bytes) in &before {
        if owned.contains(path) || merged.contains(&path.as_str()) {
            continue;
        }
        assert_eq!(
            after.get(path),
            Some(bytes),
            "unrelated file {path} changed"
        );
    }
    for path in after.keys() {
        if before.contains_key(path) || owned.contains(path) || merged.contains(&path.as_str()) {
            continue;
        }
        panic!("install created an unexpected file {path}");
    }

    assert_eq!(
        after.get(".bob/commands/other.md").unwrap(),
        b"# Other command\n"
    );
    assert_eq!(after.get("src.txt").unwrap(), b"consumer source\n");

    let modes: serde_norway::Value = serde_norway::from_str(
        std::str::from_utf8(after.get(".bob/custom_modes.yaml").unwrap()).unwrap(),
    )
    .unwrap();
    let items = modes
        .get("customModes")
        .and_then(|value| value.as_sequence())
        .unwrap();
    let slugs: Vec<&str> = items
        .iter()
        .filter_map(|mode| mode.get("slug").and_then(|slug| slug.as_str()))
        .collect();
    assert!(slugs.contains(&"reviewer"));
    assert!(slugs.contains(&"sethu-migrator"));
    let reviewer_mode = items
        .iter()
        .find(|mode| mode.get("slug").and_then(|slug| slug.as_str()) == Some("reviewer"))
        .unwrap();
    assert_eq!(
        reviewer_mode.get("name").and_then(|name| name.as_str()),
        Some("Reviewer")
    );

    let nextest = std::str::from_utf8(after.get(".config/nextest.toml").unwrap()).unwrap();
    assert!(nextest.contains("[profile.default]"));
    assert!(nextest.contains("retries = 2"));
    assert!(nextest.contains("[profile.sethu.junit]"));

    let exclude = std::fs::read_to_string(dir.path().join(".git/info/exclude")).unwrap();
    assert!(exclude.contains("*.log"));
    assert_eq!(
        exclude
            .lines()
            .filter(|line| line.trim() == ".sethu/")
            .count(),
        1
    );
}

#[test]
fn second_install_changes_nothing() {
    let dir = installed_repo();
    let before = snapshot(dir.path());
    let stdout = install_ok(dir.path());
    let after = snapshot(dir.path());

    assert_eq!(before, after, "a repeat install must change no file bytes");
    assert!(
        !after.keys().any(|path| path.ends_with(".sethu-new")),
        "a repeat install writes no companion files"
    );
    for file in install::owned_files().unwrap() {
        assert!(
            stdout.contains(&format!("unchanged {}", file.target)),
            "stdout reports {} as unchanged",
            file.target
        );
    }
}

#[test]
fn edited_owned_file_survives_beside_new_copy() {
    let dir = installed_repo();
    let target = ".bob/commands/api-upgrade.md";
    let path = dir.path().join(target);
    let mut edited = std::fs::read(&path).unwrap();
    edited.extend_from_slice(b"\n# Local note.\n");
    std::fs::write(&path, &edited).unwrap();

    let stdout = install_ok(dir.path());

    assert_eq!(
        std::fs::read(&path).unwrap(),
        edited,
        "user edits stay in place"
    );
    let owned = install::owned_files().unwrap();
    let pack_bytes = owned
        .iter()
        .find(|file| file.target == target)
        .expect("command file stays owned")
        .bytes
        .clone();
    let beside = dir.path().join(format!("{target}.sethu-new"));
    assert_eq!(std::fs::read(&beside).unwrap(), pack_bytes);
    assert!(
        stdout.contains(&format!("kept {target}")),
        "stdout reports the kept file"
    );
    assert!(
        stdout.contains(".sethu-new"),
        "stdout names the companion file"
    );

    let record = install::read_installation(dir.path()).unwrap().unwrap();
    let digest = sethu::state::layout::sha256_hex(&pack_bytes);
    assert_eq!(
        record.files.get(target).map(String::as_str),
        Some(digest.as_str())
    );

    let again = install_ok(dir.path());
    assert_eq!(
        std::fs::read(&path).unwrap(),
        edited,
        "edits survive a third run"
    );
    assert_eq!(std::fs::read(&beside).unwrap(), pack_bytes);
    assert!(again.contains(&format!("kept {target}")));
}

#[test]
fn slug_collision_is_reported_and_left_alone() {
    let dir = git_repo();
    std::fs::create_dir_all(dir.path().join(".bob")).unwrap();
    let user_modes = "customModes:\n  - slug: sethu-migrator\n    name: Mine\n    roleDefinition: Mine.\n    groups:\n      - read\n";
    std::fs::write(dir.path().join(".bob/custom_modes.yaml"), user_modes).unwrap();

    let stdout = install_ok(dir.path());
    assert!(stdout.contains("collides"), "stdout reports the collision");

    let modes = std::fs::read_to_string(dir.path().join(".bob/custom_modes.yaml")).unwrap();
    let value: serde_norway::Value = serde_norway::from_str(&modes).unwrap();
    let items = value
        .get("customModes")
        .and_then(|entry| entry.as_sequence())
        .unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(
        items[0].get("name").and_then(|name| name.as_str()),
        Some("Mine")
    );

    let record = install::read_installation(dir.path()).unwrap().unwrap();
    assert!(
        !record.owned_modes.contains(&"sethu-migrator".to_string()),
        "a colliding slug never counts as owned"
    );

    let again = install_ok(dir.path());
    assert!(again.contains("collides"));
    let modes = std::fs::read_to_string(dir.path().join(".bob/custom_modes.yaml")).unwrap();
    let value: serde_norway::Value = serde_norway::from_str(&modes).unwrap();
    let items = value
        .get("customModes")
        .and_then(|entry| entry.as_sequence())
        .unwrap();
    assert_eq!(
        items[0].get("name").and_then(|name| name.as_str()),
        Some("Mine")
    );
}

#[test]
fn owned_mode_updates_on_reinstall() {
    let dir = installed_repo();
    let path = dir.path().join(".bob/custom_modes.yaml");
    let text = std::fs::read_to_string(&path).unwrap();
    let edited = text.replace(
        "Upgrades this application across an OpenAPI change with evidence.",
        "Edited description.",
    );
    assert_ne!(edited, text, "the test edit must land");
    std::fs::write(&path, &edited).unwrap();

    let stdout = install_ok(dir.path());
    assert!(stdout.contains("mode updated sethu-migrator"));

    let modes = std::fs::read_to_string(&path).unwrap();
    assert!(modes.contains("Upgrades this application across an OpenAPI change with evidence."));
    assert!(!modes.contains("Edited description."));
}

#[test]
fn non_repository_target_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    sethu()
        .arg("install")
        .arg(dir.path())
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains("not a git repository"))
        .stdout(predicate::str::is_empty());
    assert!(
        snapshot(dir.path()).is_empty(),
        "a refused install writes nothing"
    );
}

#[test]
fn missing_target_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("no-such-repo");
    sethu()
        .arg("install")
        .arg(&missing)
        .assert()
        .failure()
        .code(1)
        .stdout(predicate::str::is_empty());
    assert!(!missing.exists(), "a refused install creates nothing");
}

#[test]
fn exclude_entry_is_idempotent() {
    let dir = installed_repo();
    install_ok(dir.path());
    let exclude = std::fs::read_to_string(dir.path().join(".git/info/exclude")).unwrap();
    assert_eq!(
        exclude
            .lines()
            .filter(|line| line.trim() == ".sethu/")
            .count(),
        1,
        "the exclude line appears exactly once"
    );
}

#[test]
fn nextest_profile_merges_and_repeats_stably() {
    let dir = git_repo();
    std::fs::create_dir_all(dir.path().join(".config")).unwrap();
    let path = dir.path().join(".config/nextest.toml");
    std::fs::write(&path, "[profile.ci.junit]\npath = \"ci.xml\"\n").unwrap();

    install_ok(dir.path());
    let first = std::fs::read(&path).unwrap();
    let text = std::str::from_utf8(&first).unwrap();
    assert!(text.contains("[profile.ci.junit]"));
    assert!(text.contains("path = \"ci.xml\""));
    assert!(text.contains("[profile.sethu.junit]"));
    assert!(text.contains("path = \"junit.xml\""));

    install_ok(dir.path());
    assert_eq!(
        std::fs::read(&path).unwrap(),
        first,
        "a repeat install keeps every byte"
    );
}

#[test]
fn nextest_profile_produces_junit_output() {
    if !nextest_present() {
        eprintln!("SKIP junit probe: `cargo-nextest` is not on PATH");
        return;
    }
    let dir = git_repo();
    std::fs::write(
        dir.path().join("Cargo.toml"),
        "[package]\nname = \"consumer\"\nversion = \"0.0.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(
        dir.path().join("src/lib.rs"),
        "#[test]\nfn sample() {\n    assert_eq!(1 + 1, 2);\n}\n",
    )
    .unwrap();
    install_ok(dir.path());

    let path_value = match path_with_cargo_bin() {
        Some(value) => value,
        None => {
            eprintln!("SKIP junit probe: home directory is unknown");
            return;
        }
    };
    let output = std::process::Command::new("cargo")
        .arg("nextest")
        .arg("run")
        .arg("--profile")
        .arg("sethu")
        .current_dir(dir.path())
        .env("PATH", path_value)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "nextest with the sethu profile failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let junit = std::fs::read_to_string(
        dir.path()
            .join("target")
            .join("nextest")
            .join("sethu")
            .join("junit.xml"),
    )
    .expect("the sethu profile writes its JUnit report below target");
    assert!(junit.contains("sample"), "the JUnit report names the test");
}

#[test]
fn installed_paths_match_documented_layout() {
    let dir = installed_repo();
    let owned = install::owned_files().unwrap();
    let targets: Vec<String> = owned.iter().map(|file| file.target.clone()).collect();
    assert_eq!(targets, expected_owned_targets());
    for target in &targets {
        assert!(dir.path().join(target).is_file(), "{target} is installed");
    }
    assert!(dir.path().join(".bob/custom_modes.yaml").is_file());
    assert!(dir.path().join(".sethu/installation.json").is_file());
}

/// Front matter of a slash command file.
#[derive(Debug, Deserialize)]
struct CommandFront {
    /// Text shown beside the command in the menu.
    description: Option<String>,
    /// Grey hint of expected arguments.
    #[serde(rename = "argument-hint")]
    argument_hint: Option<String>,
}

/// Front matter of a skill file.
#[derive(Debug, Deserialize)]
struct SkillFront {
    /// Display name shown in the interface.
    name: Option<String>,
    /// Summary that drives skill activation.
    description: Option<String>,
}

/// Split YAML front matter off a Markdown file.
///
/// The file must open with a `---` line and close the block with another
/// `---` line. Anything else reads as missing front matter.
fn front_matter(text: &str) -> Option<String> {
    let body = text.strip_prefix("---\n")?;
    let end = body.find("\n---\n")?;
    Some(body[..end].to_string())
}

/// Parse front matter into a typed shape.
///
/// A missing block or an unparsable block fails the test outright.
fn parse_front_matter<T: serde::de::DeserializeOwned>(text: &str, path: &str) -> T {
    let raw = front_matter(text).unwrap_or_else(|| panic!("{path} needs front matter"));
    serde_norway::from_str(&raw).unwrap_or_else(|_| panic!("{path} front matter must parse"))
}

#[test]
fn pack_command_file_has_valid_shape() {
    let dir = installed_repo();
    let rel = ".bob/commands/api-upgrade.md";
    assert!(
        rel.ends_with(".md"),
        "commands load only with the right extension"
    );
    let text = std::fs::read_to_string(dir.path().join(rel)).unwrap();
    let front: CommandFront = parse_front_matter(&text, rel);
    assert!(!front.description.unwrap_or_default().is_empty());
    let hint = front.argument_hint.unwrap_or_default();
    assert!(
        hint.contains("old-spec") && hint.contains("new-spec"),
        "the hint names both specs"
    );
    let stem = Path::new(rel)
        .file_stem()
        .unwrap()
        .to_string_lossy()
        .to_string();
    let slug = regex::Regex::new("^[a-z0-9-]+$").unwrap();
    assert!(
        slug.is_match(&stem),
        "the command name stays a lowercase slug"
    );
    assert_ne!(stem, "help", "the command avoids built in names");
    assert!(
        text.contains("$1") && text.contains("$2"),
        "the body takes both arguments"
    );
    assert!(text.contains("api-upgrade"), "the body names its skill");
    assert!(
        text.contains("capabilities"),
        "the body checks capabilities first"
    );
}

#[test]
fn pack_skill_file_has_required_front_matter() {
    let dir = installed_repo();
    let rel = ".bob/skills/api-upgrade/SKILL.md";
    let text = std::fs::read_to_string(dir.path().join(rel)).unwrap();
    let front: SkillFront = parse_front_matter(&text, rel);
    assert_eq!(front.name.as_deref(), Some("api-upgrade"));
    assert!(
        front
            .description
            .map(|note| !note.is_empty())
            .unwrap_or(false),
        "a skill without a description stays ignored"
    );
    for companion in [
        "references/outcomes.md",
        "references/evidence.md",
        "references/commands.md",
    ] {
        assert!(text.contains(companion), "the skill names {companion}");
        let path = dir.path().join(".bob/skills/api-upgrade").join(companion);
        assert!(path.is_file(), "{companion} exists beside the skill");
        assert!(
            !std::fs::read(&path).unwrap().is_empty(),
            "{companion} is not empty"
        );
    }
    assert!(
        text.contains("available"),
        "the skill refuses unavailable commands"
    );
}

/// One mode entry in the merged project mode file.
#[derive(Debug, Deserialize)]
struct ModeEntry {
    /// Unique identifier of the mode.
    slug: Option<String>,
    /// Display name shown in the interface.
    name: Option<String>,
    /// Core identity and expertise of the mode.
    #[serde(rename = "roleDefinition")]
    role_definition: Option<String>,
    /// Tool groups the mode may use.
    groups: Option<Vec<String>>,
    /// Subagent presets the mode may spawn.
    #[serde(rename = "allowedSubagents")]
    allowed_subagents: Option<Vec<String>>,
}

/// Top level shape of the merged project mode file.
#[derive(Debug, Deserialize)]
struct ModesFile {
    /// Every mode known to the project.
    #[serde(rename = "customModes")]
    custom_modes: Vec<ModeEntry>,
}

#[test]
fn pack_modes_file_passes_mode_rules() {
    let dir = installed_repo();
    let rel = ".bob/custom_modes.yaml";
    let text = std::fs::read_to_string(dir.path().join(rel)).unwrap();
    let parsed: ModesFile = serde_norway::from_str(&text).expect("modes parse as YAML");
    let slug = regex::Regex::new("^[A-Za-z0-9-]+$").unwrap();
    let allowed = [
        "read", "edit", "execute", "mcp", "skill", "workflow", "todo", "subtask", "subagent",
        "mode",
    ];
    let mut seen = std::collections::HashSet::new();
    for mode in &parsed.custom_modes {
        let name = mode.slug.as_deref().expect("every mode carries a slug");
        assert!(slug.is_match(name), "slug {name} keeps the allowed shape");
        assert!(
            seen.insert(name.to_string()),
            "slug {name} appears exactly once"
        );
        assert!(mode.name.as_ref().is_some_and(|value| !value.is_empty()));
        assert!(
            mode.role_definition
                .as_ref()
                .is_some_and(|value| !value.is_empty())
        );
        let groups = mode.groups.as_ref().expect("every mode carries groups");
        for group in groups {
            assert!(
                allowed.contains(&group.as_str()),
                "group {group} is documented"
            );
        }
        if name != "sethu-migrator" {
            assert!(
                !groups.iter().any(|group| group == "execute"),
                "only the migrator holds the execute group"
            );
        }
    }
    let migrator = parsed
        .custom_modes
        .iter()
        .find(|mode| mode.slug.as_deref() == Some("sethu-migrator"))
        .expect("the pack ships its migrator mode");
    assert!(
        migrator
            .groups
            .as_ref()
            .unwrap()
            .contains(&"execute".to_string())
    );
    assert_eq!(
        migrator.allowed_subagents,
        Some(vec!["explore".to_string()]),
        "tracers stay read only through the explore preset"
    );

    let stems = ["api-upgrade"];
    for stem in stems {
        assert!(
            !parsed
                .custom_modes
                .iter()
                .any(|mode| mode.slug.as_deref() == Some(stem)),
            "no command filename collides with a mode slug"
        );
    }
}

#[test]
fn pack_files_carry_no_forbidden_references() {
    let dir = installed_repo();
    let task_id = regex::Regex::new(&["T", "[0-9]+", "\\.", "[0-9]+", "b?"].concat()).unwrap();
    let review_round = regex::Regex::new(&["round", "[0-9]"].concat()).unwrap();
    let section_mark = '\u{a7}';
    let design_name = ["design", "md"].join(".");
    let diary_name = ["dev", "diary"].join("-");
    let remedial = ["remedi", "ation"].concat();
    let mut scanned = 0;
    for file in install::owned_files().unwrap() {
        let bytes = std::fs::read(dir.path().join(&file.target)).unwrap();
        let text = std::str::from_utf8(&bytes).expect("pack files stay text");
        assert!(
            !task_id.is_match(text),
            "{} cites no plan task",
            file.target
        );
        assert!(
            !text.contains(section_mark),
            "{} cites no plan section",
            file.target
        );
        assert!(
            !text.contains(&design_name),
            "{} names no plan file",
            file.target
        );
        assert!(
            !text.contains(&diary_name),
            "{} names no plan file",
            file.target
        );
        assert!(
            !review_round.is_match(text),
            "{} cites no review round",
            file.target
        );
        assert!(
            !text.contains(&remedial),
            "{} cites no later phase",
            file.target
        );
        scanned += 1;
    }
    let modes = std::fs::read(dir.path().join(".bob/custom_modes.yaml")).unwrap();
    let text = std::str::from_utf8(&modes).unwrap();
    assert!(!task_id.is_match(text));
    assert!(!text.contains(section_mark));
    assert!(!text.contains(&design_name));
    assert!(!text.contains(&diary_name));
    assert!(!review_round.is_match(text));
    assert!(!text.contains(&remedial));
    scanned += 1;
    assert!(scanned >= 6, "every installed pack file passes the scan");
}

#[test]
fn install_reports_tools_and_capabilities() {
    let dir = installed_repo();
    let stdout = install_ok(dir.path());
    assert!(stdout.contains("vimanam"), "the report names the diff tool");
    assert!(stdout.contains("git"), "the report names git");
    assert!(
        stdout.contains("install"),
        "the capabilities table follows the summary"
    );
}

#[test]
fn install_into_linked_worktree_completes_and_repeats() {
    let main = git_repo();
    let holder = tempfile::tempdir().unwrap();
    let linked = holder.path().join("linked");
    let linked_arg = linked.to_string_lossy().into_owned();
    git(main.path(), &["worktree", "add", linked_arg.as_str()]);

    sethu()
        .arg("install")
        .arg(&linked)
        .assert()
        .success()
        .stdout(predicate::str::contains("installed into"));
    assert!(
        linked.join(".bob/commands/api-upgrade.md").is_file(),
        "owned files land in the linked checkout"
    );
    assert!(
        linked.join(".sethu/installation.json").is_file(),
        "the record lands in the linked checkout"
    );

    let output = std::process::Command::new("git")
        .args(["rev-parse", "--git-dir"])
        .current_dir(&linked)
        .output()
        .unwrap();
    assert!(output.status.success(), "the linked checkout stays a repo");
    let raw = String::from_utf8(output.stdout).unwrap();
    let git_dir = raw.trim();
    let git_dir = if Path::new(git_dir).is_absolute() {
        PathBuf::from(git_dir)
    } else {
        linked.join(git_dir)
    };
    let exclude = std::fs::read_to_string(git_dir.join("info").join("exclude")).unwrap();
    assert_eq!(
        exclude
            .lines()
            .filter(|line| line.trim() == ".sethu/")
            .count(),
        1,
        "the exclude line lands once in the resolved git directory"
    );

    let before = snapshot(&linked);
    install_ok(&linked);
    let after = snapshot(&linked);
    assert_eq!(
        before, after,
        "a repeat install in the linked checkout changes nothing"
    );
    assert!(
        install::read_installation(&linked).unwrap().is_some(),
        "the record survives the rerun"
    );
}

#[test]
fn install_into_reports_structured_outcomes() {
    let dir = git_repo();
    let first = install::install_into(dir.path()).unwrap();
    assert_eq!(first.pack, install::pack_version().unwrap());
    assert!(
        first
            .files
            .iter()
            .all(|file| file.outcome == FileOutcome::Created)
    );
    assert_eq!(first.modes_added, vec!["sethu-migrator".to_string()]);
    assert!(first.modes_updated.is_empty());
    assert!(first.mode_collisions.is_empty());
    assert!(first.exclude_updated);
    assert!(first.nextest_updated);
    assert!(!first.summary().is_empty());

    let second = install::install_into(dir.path()).unwrap();
    assert!(
        second
            .files
            .iter()
            .all(|file| file.outcome == FileOutcome::Unchanged)
    );
    assert!(second.modes_added.is_empty());
    assert!(second.modes_updated.is_empty());
    assert!(second.mode_collisions.is_empty());
    assert!(!second.exclude_updated);
    assert!(!second.nextest_updated);
}
