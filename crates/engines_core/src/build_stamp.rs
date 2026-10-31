//! What `build.rs` asks git, shared with its tests (the build script compiles this file as a module
//! of its own; the library compiles it only under `cfg(test)`, so the core still runs no processes).
//!
//! - The engine's own commit, for `ENGINES_BUILD_ID` (ADR 0054 P10).
//! - **The host build**: when this repo is checked out as a submodule of another one,
//!   the other repo's commit, so a render's sidecar records both revisions and stays re-creatable
//!   from the two (CE-R7.3). Outside a submodule there is none.

use std::path::{Path, PathBuf};
use std::process::Command;

/// `git <args>` in `dir`: its trimmed stdout, or `None` when git is missing or the command fails.
pub(crate) fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).current_dir(dir).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// The working tree of the repo that has `dir`'s repo as a submodule, if there is one.
pub(crate) fn superproject(dir: &Path) -> Option<PathBuf> {
    git(dir, &["rev-parse", "--show-superproject-working-tree"]).filter(|s| !s.is_empty()).map(PathBuf::from)
}

/// The host build: `g<12-hex commit>` of the superproject of `dir`, plus `.dirty` when its tracked
/// files differ from that commit when the build script runs. A submodule checked out at another commit than the one recorded
/// counts as a difference; edits inside a submodule don't (the engine's own build id marks those).
pub(crate) fn host_build(dir: &Path) -> Option<String> {
    let host = superproject(dir)?;
    let sha = git(&host, &["rev-parse", "--short=12", "HEAD"])?;
    let status = git(&host, &["status", "--porcelain", "--untracked-files=no", "--ignore-submodules=dirty"]);
    let dirty = status.is_none_or(|s| !s.is_empty());
    Some(format!("g{sha}{}", if dirty { ".dirty" } else { "" }))
}

/// The files whose change moves `dir`'s commit: HEAD (in a worktree it lives in its own git dir), the
/// packed refs and the checked-out branch's ref (both in the common dir). For `rerun-if-changed`.
pub(crate) fn head_paths(dir: &Path) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let (Some(git_dir), Some(common)) = (git(dir, &["rev-parse", "--absolute-git-dir"]), git(dir, &["rev-parse", "--path-format=absolute", "--git-common-dir"])) {
        paths.push(Path::new(&git_dir).join("HEAD"));
        paths.push(Path::new(&common).join("packed-refs"));
        if let Some(head_ref) = git(dir, &["symbolic-ref", "-q", "HEAD"]) {
            paths.push(Path::new(&common).join(head_ref));
        }
    }
    paths
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// A fresh folder under the system temp dir, removed when dropped.
    struct Scratch(PathBuf);
    impl Scratch {
        fn new() -> Self {
            static N: AtomicU32 = AtomicU32::new(0);
            let p = std::env::temp_dir().join(format!("engines-build-stamp-{}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
            let _ = std::fs::remove_dir_all(&p);
            std::fs::create_dir_all(&p).unwrap();
            Scratch(p)
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// `git` that must succeed, with an identity and local submodules allowed.
    fn run(dir: &Path, args: &[&str]) -> String {
        let mut all = vec!["-c", "user.name=test", "-c", "user.email=test@example.invalid", "-c", "protocol.file.allow=always", "-c", "init.defaultBranch=main"];
        all.extend(args);
        git(dir, &all).unwrap_or_else(|| panic!("git {args:?} failed in {}", dir.display()))
    }

    /// A repo with one committed file.
    fn repo(dir: &Path, file: &str) {
        std::fs::create_dir_all(dir).unwrap();
        run(dir, &["init", "-q"]);
        std::fs::write(dir.join(file), "x\n").unwrap();
        run(dir, &["add", file]);
        run(dir, &["commit", "-q", "-m", "first"]);
    }

    #[test]
    fn a_repo_on_its_own_has_no_host_build() {
        if git(Path::new("."), &["--version"]).is_none() {
            return; // no git here: build.rs falls back the same way
        }
        let s = Scratch::new();
        let open = s.0.join("open");
        repo(&open, "engine.txt");
        assert_eq!(superproject(&open), None);
        assert_eq!(host_build(&open), None);
    }

    #[test]
    fn inside_a_submodule_the_host_build_is_the_superprojects_commit() {
        if git(Path::new("."), &["--version"]).is_none() {
            return;
        }
        let s = Scratch::new();
        let upstream = s.0.join("upstream");
        repo(&upstream, "engine.txt");
        let host = s.0.join("host");
        repo(&host, "closed.txt");
        run(&host, &["submodule", "add", "-q", upstream.to_str().unwrap(), "open"]);
        run(&host, &["commit", "-q", "-m", "add the open repo"]);
        let sub = host.join("open");
        let sha = run(&host, &["rev-parse", "--short=12", "HEAD"]);
        assert_eq!(superproject(&sub).map(|p| p.canonicalize().unwrap()), Some(host.canonicalize().unwrap()));
        assert_eq!(host_build(&sub), Some(format!("g{sha}")), "a clean host");

        // An edit inside the submodule is the engine's dirt, not the host's.
        std::fs::write(sub.join("engine.txt"), "edited\n").unwrap();
        assert_eq!(host_build(&sub), Some(format!("g{sha}")));
        run(&sub, &["checkout", "-q", "--", "engine.txt"]);

        // A tracked edit in the host marks it dirty.
        std::fs::write(host.join("closed.txt"), "edited\n").unwrap();
        assert_eq!(host_build(&sub), Some(format!("g{sha}.dirty")));
        run(&host, &["checkout", "-q", "--", "closed.txt"]);

        // So does the submodule checked out at another commit than the one the host records.
        std::fs::write(sub.join("engine.txt"), "next\n").unwrap();
        run(&sub, &["commit", "-q", "-am", "next"]);
        assert_eq!(host_build(&sub), Some(format!("g{sha}.dirty")));
    }

    #[test]
    fn the_head_paths_are_where_git_keeps_them() {
        if git(Path::new("."), &["--version"]).is_none() {
            return;
        }
        let s = Scratch::new();
        let r = s.0.join("r");
        repo(&r, "a.txt");
        let paths = head_paths(&r);
        assert!(paths.iter().any(|p| p.ends_with("HEAD") && p.exists()), "{paths:?}");
        assert!(paths.iter().any(|p| p.ends_with("refs/heads/main") && p.exists()), "{paths:?}");
    }
}
