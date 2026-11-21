//! CI entry point for the runtime pin.
//!
//! Every check the repository promises about its bundled Agent lives here so
//! that the same code runs locally, in `ci.yml` and in the sync workflow:
//!
//! ```text
//! dsh-runtime-lock check      # lock + workspace version + submodule + wire contract
//! dsh-runtime-lock verify     # lock file and workspace version only
//! dsh-runtime-lock contract   # reviewed upstream SDK sources and method names
//! dsh-runtime-lock show       # print the pinned tag, version and commit
//! dsh-runtime-lock select --releases <json> [--tag-only]
//! dsh-runtime-lock set --tag dsh-v0.2.3 --commit <sha>
//! ```
//!
//! Exit codes: 0 = the pin is consistent, 1 = a check failed, 2 = bad usage.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use dsh_protocol::lock::RuntimeLock;
use dsh_protocol::releases::{ReleaseCandidate, Selection, select_newer};
use dsh_protocol::sync::{
    apply_pin, expected_wire_names, missing_wire_names, rewrite_workspace_version,
    workspace_version,
};

const USAGE: &str = "usage: dsh-runtime-lock <command> [--root <dir>]\n\ncommands:\n  check                 lock, workspace version, submodule commit and wire contract\n  verify                lock file internals and workspace version\n  contract              reviewed upstream SDK sources and wire names\n  show                  print the pinned tag, version and commit\n  select --releases <json> [--tag-only]\n                        pick the newest non-draft upstream release\n  set --tag <tag> --commit <sha>\n                        move the pin (rewrites the lock and Cargo.toml)\n";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(outcome) => {
            println!("{outcome}");
            ExitCode::SUCCESS
        }
        Err(failure) => {
            eprintln!("{failure}");
            ExitCode::from(failure.code)
        }
    }
}

/// A check failure with the process exit code it maps to.
#[derive(Debug, Clone)]
struct Failure {
    /// 1 = check failed, 2 = bad usage.
    code: u8,
    /// Human-readable explanation printed to stderr.
    message: String,
}

impl Failure {
    fn check(message: impl Into<String>) -> Self {
        Self {
            code: 1,
            message: message.into(),
        }
    }

    fn usage(message: impl Into<String>) -> Self {
        Self {
            code: 2,
            message: format!("{}\n\n{USAGE}", message.into()),
        }
    }
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

fn submodule_dir(root: &Path) -> PathBuf {
    root.join("runtime/upstream")
}

fn lock_path(root: &Path) -> PathBuf {
    root.join(dsh_protocol::lock::DEFAULT_LOCK_PATH)
}

fn parse_root(args: &[String]) -> Result<(PathBuf, Vec<String>), Failure> {
    let mut root = std::env::current_dir()
        .map_err(|error| Failure::usage(format!("cannot read current directory: {error}")))?;
    let mut rest = Vec::new();
    let mut index = 0;
    while index < args.len() {
        if args[index] == "--root" {
            let value = args
                .get(index + 1)
                .ok_or_else(|| Failure::usage("--root needs a directory"))?;
            root = PathBuf::from(value);
            index += 2;
            continue;
        }
        rest.push(args[index].clone());
        index += 1;
    }
    Ok((root, rest))
}

fn run(args: &[String]) -> Result<String, Failure> {
    let (root, rest) = parse_root(args)?;
    let Some(command) = rest.first() else {
        return Err(Failure::usage("missing command"));
    };
    match command.as_str() {
        "check" => check(&root),
        "verify" => verify(&root),
        "contract" => contract(&root),
        "show" => show(&root),
        "select" => select(&root, &rest[1..]),
        "set" => set(&root, &rest[1..]),
        other => Err(Failure::usage(format!("unknown command {other:?}"))),
    }
}

fn load_lock(root: &Path) -> Result<RuntimeLock, Failure> {
    let path = lock_path(root);
    let lock = RuntimeLock::load(&path).map_err(|error| Failure::check(error.to_string()))?;
    lock.verify()
        .map_err(|error| Failure::check(error.to_string()))?;
    Ok(lock)
}

fn verify(root: &Path) -> Result<String, Failure> {
    let lock = load_lock(root)?;
    let manifest_path = root.join("Cargo.toml");
    let manifest = std::fs::read_to_string(&manifest_path).map_err(|error| {
        Failure::check(format!("cannot read {}: {error}", manifest_path.display()))
    })?;
    let version =
        workspace_version(&manifest).map_err(|error| Failure::check(error.to_string()))?;
    if version != lock.product_version() {
        return Err(Failure::check(format!(
            "workspace version {version} does not mirror the bundled runtime {} ({}); bump [workspace.package] in the same commit that moves the pin",
            lock.product_version(),
            lock.runtime.tag
        )));
    }
    Ok(format!(
        "lock ok: {} at {} (workspace version {version})",
        lock.runtime.tag, lock.runtime.commit
    ))
}

fn submodule_commit(root: &Path) -> Result<String, Failure> {
    let dir = submodule_dir(root);
    if !dir.join(".git").exists() {
        return Err(Failure::check(format!(
            "submodule {} is not checked out; run `git submodule update --init --depth 1`",
            dir.display()
        )));
    }
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(&dir)
        .args(["rev-parse", "HEAD"])
        .output()
        .map_err(|error| Failure::check(format!("cannot run git: {error}")))?;
    if !output.status.success() {
        return Err(Failure::check(format!(
            "git rev-parse failed in {}: {}",
            dir.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn contract(root: &Path) -> Result<String, Failure> {
    let source_dir = submodule_dir(root).join("packages/sdk/protocol/src");
    if !source_dir.is_dir() {
        return Err(Failure::check(format!(
            "upstream protocol source missing at {}; is the submodule checked out?",
            source_dir.display()
        )));
    }
    let mut files = Vec::new();
    collect_typescript(&source_dir, &mut files).map_err(|error| {
        Failure::check(format!("cannot read {}: {error}", source_dir.display()))
    })?;
    let snapshot = protocol_snapshot(&source_dir, &files)?;
    let reviewed =
        std::fs::read_to_string(root.join("runtime/protocol-source.lock")).map_err(|error| {
            Failure::check(format!("cannot read protocol source snapshot: {error}"))
        })?;
    if snapshot != reviewed {
        return Err(Failure::check(format!(
            "upstream SDK protocol sources changed; review Rust wire types and tests before updating runtime/protocol-source.lock\nobserved snapshot:\n{snapshot}"
        )));
    }
    // UNCOVERED: imported SessionEvent and other domain types need real SDK tests in round two.
    let mut source = String::new();
    for path in &files {
        source.push_str(
            &std::fs::read_to_string(path).map_err(|error| {
                Failure::check(format!("cannot read {}: {error}", path.display()))
            })?,
        );
        source.push('\n');
    }
    let expected = expected_wire_names();
    let missing = missing_wire_names(&source, &expected);
    if !missing.is_empty() {
        return Err(Failure::check(format!(
            "upstream no longer declares these wire names: {}; review packages/sdk/protocol before syncing",
            missing.join(", ")
        )));
    }
    Ok(format!(
        "wire contract ok: {} names present",
        expected.len()
    ))
}

fn collect_typescript(dir: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    let mut entries: Vec<_> = std::fs::read_dir(dir)?.collect::<Result<_, _>>()?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let path = entry.path();
        let metadata = entry.metadata()?;
        if metadata.is_dir() {
            collect_typescript(&path, out)?;
        } else if path.extension().is_some_and(|extension| extension == "ts") {
            out.push(path);
        }
    }
    Ok(())
}

fn protocol_snapshot(dir: &Path, files: &[PathBuf]) -> Result<String, Failure> {
    let mut snapshot = String::new();
    for path in files {
        let output = std::process::Command::new("git")
            .args(["hash-object", "--no-filters", "--"])
            .arg(path)
            .output()
            .map_err(|error| Failure::check(format!("cannot hash protocol source: {error}")))?;
        if !output.status.success() {
            return Err(Failure::check(format!(
                "cannot hash {}: {}",
                path.display(),
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
        let relative = path
            .strip_prefix(dir)
            .map_err(|error| Failure::check(error.to_string()))?;
        snapshot.push_str(&format!(
            "{} {}\n",
            String::from_utf8_lossy(&output.stdout).trim(),
            relative.to_string_lossy().replace('\\', "/")
        ));
    }
    Ok(snapshot)
}

fn check(root: &Path) -> Result<String, Failure> {
    let verified = verify(root)?;
    let lock = load_lock(root)?;
    let commit = submodule_commit(root)?;
    if commit != lock.runtime.commit {
        return Err(Failure::check(format!(
            "submodule HEAD {commit} does not match the lock commit {}; the pin and the checkout drifted apart",
            lock.runtime.commit
        )));
    }
    let contract_summary = contract(root)?;
    Ok(format!(
        "{verified}\nsubmodule ok: {commit}\n{contract_summary}"
    ))
}

fn show(root: &Path) -> Result<String, Failure> {
    let lock = load_lock(root)?;
    let mut lines = vec![
        format!("tag      {}", lock.runtime.tag),
        format!("version  {}", lock.product_version()),
        format!("commit   {}", lock.runtime.commit),
        format!("repo     {}", lock.runtime.repository),
        format!("submodule {}", lock.runtime.submodule),
    ];
    if let Some(entry) = lock.packaging.runtime_entry.as_deref() {
        lines.push(format!("entry    {entry}"));
    }
    Ok(lines.join("\n"))
}

/// Decide whether a newer upstream release exists. Never writes anything.
///
/// The sync workflow calls this with the raw GitHub releases payload and splices
/// the printed `key=value` lines into `$GITHUB_OUTPUT`.
fn select(root: &Path, args: &[String]) -> Result<String, Failure> {
    let mut releases = None;
    let mut tag_only = false;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--releases" => {
                releases = args.get(index + 1).cloned();
                index += 2;
            }
            "--tag-only" => {
                tag_only = true;
                index += 1;
            }
            other => return Err(Failure::usage(format!("unknown option {other:?}"))),
        }
    }
    let releases = releases.ok_or_else(|| Failure::usage("select needs --releases <path>"))?;
    let payload = std::fs::read_to_string(&releases)
        .map_err(|error| Failure::check(format!("cannot read {releases}: {error}")))?;
    let candidates: Vec<ReleaseCandidate> = serde_json::from_str(&payload)
        .map_err(|error| Failure::check(format!("cannot parse releases payload: {error}")))?;
    let lock = load_lock(root)?;
    let current = lock
        .release()
        .map_err(|error| Failure::check(error.to_string()))?;
    match select_newer(&candidates, &current) {
        Selection::UpToDate { highest } => Ok(format!(
            "updated=false\ncurrent={}\nhighest={highest}",
            current.tag
        )),
        Selection::Newer {
            release,
            prerelease,
            html_url,
        } => {
            if tag_only {
                return Ok(release.tag);
            }
            Ok(format!(
                "updated=true\ncurrent={}\ntag={}\nversion={}\nprerelease={prerelease}\nrelease_url={}",
                current.tag,
                release.tag,
                release.product_version(),
                html_url.unwrap_or_default()
            ))
        }
    }
}

fn set(root: &Path, args: &[String]) -> Result<String, Failure> {
    let mut tag = None;
    let mut commit = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--tag" => {
                tag = args.get(index + 1).cloned();
                index += 2;
            }
            "--commit" => {
                commit = args.get(index + 1).cloned();
                index += 2;
            }
            other => return Err(Failure::usage(format!("unknown option {other:?}"))),
        }
    }
    let tag = tag.ok_or_else(|| Failure::usage("set needs --tag"))?;
    let commit = commit.ok_or_else(|| Failure::usage("set needs --commit"))?;
    let mut lock = load_lock(root)?;
    let moved =
        apply_pin(&mut lock, &tag, &commit).map_err(|error| Failure::check(error.to_string()))?;
    let lock_text = lock
        .to_toml()
        .map_err(|error| Failure::check(error.to_string()))?;
    let manifest_path = root.join("Cargo.toml");
    let manifest = std::fs::read_to_string(&manifest_path).map_err(|error| {
        Failure::check(format!("cannot read {}: {error}", manifest_path.display()))
    })?;
    let rewritten = rewrite_workspace_version(&manifest, lock.product_version())
        .map_err(|error| Failure::check(error.to_string()))?;
    std::fs::write(lock_path(root), lock_text)
        .map_err(|error| Failure::check(format!("cannot write lock file: {error}")))?;
    std::fs::write(&manifest_path, rewritten)
        .map_err(|error| Failure::check(format!("cannot write Cargo.toml: {error}")))?;
    Ok(format!(
        "pin moved: {} {} -> {} {}\nworkspace version set to {}; re-run `cargo build` to refresh Cargo.lock",
        moved.from_tag,
        moved.from_commit,
        moved.to_tag,
        moved.to_commit,
        lock.product_version()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_flag_is_split_out_of_positional_args() {
        let args = vec!["--root".to_owned(), "/tmp".to_owned(), "check".to_owned()];
        let (root, rest) = parse_root(&args).unwrap();
        assert_eq!(root, PathBuf::from("/tmp"));
        assert_eq!(rest, vec!["check".to_owned()]);
    }

    #[test]
    fn missing_root_value_is_usage_error() {
        let args = vec!["--root".to_owned()];
        let failure = parse_root(&args).unwrap_err();
        assert_eq!(failure.code, 2);
    }

    #[test]
    fn empty_command_is_usage_error() {
        let failure = run(&[]).unwrap_err();
        assert_eq!(failure.code, 2);
    }

    #[test]
    fn unknown_command_is_usage_error() {
        let failure = run(&["frobnicate".to_owned()]).unwrap_err();
        assert_eq!(failure.code, 2);
    }

    #[test]
    fn contract_rejects_field_changes_and_added_or_removed_sources() {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "dsh-protocol-contract-{}-{unique}",
            std::process::id()
        ));
        let source_dir = submodule_dir(&root).join("packages/sdk/protocol/src");
        std::fs::create_dir_all(&source_dir).unwrap();
        let path = source_dir.join("types.ts");
        let original = format!(
            "const methods = {};\nexport type Result = {{ value: string }};\n",
            serde_json::to_string(&expected_wire_names()).unwrap()
        );
        std::fs::write(&path, &original).unwrap();
        let snapshot = protocol_snapshot(&source_dir, std::slice::from_ref(&path)).unwrap();
        std::fs::write(root.join("runtime/protocol-source.lock"), snapshot).unwrap();
        assert!(contract(&root).is_ok());

        std::fs::write(&path, original.replace("value: string", "value: number")).unwrap();
        let failure = contract(&root).unwrap_err();
        assert!(
            failure
                .message
                .contains("upstream SDK protocol sources changed")
        );
        std::fs::write(&path, &original).unwrap();
        std::fs::write(source_dir.join("new.ts"), "export type Added = string;").unwrap();
        assert!(contract(&root).is_err());
        std::fs::remove_file(source_dir.join("new.ts")).unwrap();
        assert!(contract(&root).is_ok());
        std::fs::remove_file(path).unwrap();
        assert!(contract(&root).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }
}
