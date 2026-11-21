//! Build identity: the TUI version mirrors the Agent version, the build id
//! distinguishes UI-only revisions of the same Agent.

/// Product version; equals the bundled Agent version by construction.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Build identifier for UI-only revisions.
///
/// Builds produced by CI pass `DSH_TUI_BUILD_ID` (the short git commit);
/// a plain local build reports `dev`.
pub const BUILD_ID: &str = match option_env!("DSH_TUI_BUILD_ID") {
    Some(id) => id,
    None => "dev",
};

/// Workspace root used to locate `runtime/upstream.lock.toml`.
///
/// Resolution order: `DSH_TUI_ROOT`, a packaged executable's parent bundle,
/// the compile-time manifest directory walk up, then the current directory.
pub fn workspace_root() -> std::path::PathBuf {
    if let Some(root) = std::env::var_os("DSH_TUI_ROOT") {
        return std::path::PathBuf::from(root);
    }
    let bundle = std::env::current_exe().ok().and_then(|executable| {
        executable
            .parent()?
            .parent()
            .map(std::path::Path::to_path_buf)
    });
    if let Some(root) = bundle.filter(|root| root.join("runtime.lock.toml").is_file()) {
        return root;
    }
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    if let Some(root) = manifest.parent().and_then(|p| p.parent()) {
        return root.to_path_buf();
    }
    std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_matches_workspace_version() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let lock =
            dsh_protocol::lock::RuntimeLock::load(root.join("runtime/upstream.lock.toml")).unwrap();
        assert_eq!(VERSION, lock.product_version());
    }

    #[test]
    fn build_id_is_never_empty() {
        assert!(!BUILD_ID.is_empty());
    }

    #[test]
    fn workspace_root_contains_runtime_lock() {
        let root = workspace_root();
        assert!(
            root.join("runtime/upstream.lock.toml").exists(),
            "missing lock at {root:?}"
        );
    }
}
