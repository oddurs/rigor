//! Shared by the integration suites.

use std::path::Path;
use std::process::Command;

/// git configuration through the environment, so it reaches every git a test
/// runs — its own, and any started by `scripts/agent`, a hook, or rigor. Modern
/// git starts background maintenance after writes, and it can outlive the
/// command that started it; tests turn it off.
pub const QUIET_GIT: [(&str, &str); 5] = [
    ("GIT_CONFIG_COUNT", "2"),
    ("GIT_CONFIG_KEY_0", "maintenance.auto"),
    ("GIT_CONFIG_VALUE_0", "false"),
    ("GIT_CONFIG_KEY_1", "gc.auto"),
    ("GIT_CONFIG_VALUE_1", "0"),
];

/// The `GIT_*` variables this process inherited. The pre-push hook runs these
/// suites, and git can hand a hook `GIT_DIR` or `GIT_INDEX_FILE`; passed on,
/// they would aim a throwaway test at the real repository.
pub fn inherited_git_vars() -> Vec<String> {
    std::env::vars()
        .map(|(k, _)| k)
        .filter(|k| k.starts_with("GIT_"))
        .collect()
}

/// `program` in `cwd`, cut off from the calling repository and with git quiet.
pub fn isolated(program: &str, cwd: &Path) -> Command {
    let mut c = Command::new(program);
    c.current_dir(cwd);
    for k in inherited_git_vars() {
        c.env_remove(k);
    }
    c.envs(QUIET_GIT);
    c
}

/// Write an executable script.
pub fn write_exe(path: &Path, body: &str) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(path, body).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}
