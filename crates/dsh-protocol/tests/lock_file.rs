//! Integration coverage against the real `runtime/upstream.lock.toml`.

use std::path::PathBuf;

use dsh_protocol::lock::{DEFAULT_LOCK_PATH, RuntimeLock};

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("workspace root")
        .to_path_buf()
}

#[test]
fn repository_lock_file_is_consistent() {
    let path = workspace_root().join(DEFAULT_LOCK_PATH);
    let lock = RuntimeLock::load(&path).expect("lock file must parse");
    lock.verify().expect("lock file must verify");
    assert_eq!(lock.product_version(), env!("CARGO_PKG_VERSION"));
}

#[test]
fn repository_lock_matches_workspace_version() {
    let path = workspace_root().join(DEFAULT_LOCK_PATH);
    let lock = RuntimeLock::load(&path).unwrap();
    let workspace = RuntimeLock::parse(lock.to_toml().unwrap().as_str()).unwrap();
    assert_eq!(workspace.runtime.version, env!("CARGO_PKG_VERSION"));
}
