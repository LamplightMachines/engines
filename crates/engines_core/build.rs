//! Build-time steps for the core (both non-fatal):
//!
//! 1. **The build id** (ADR 0054 P10): `ENGINES_BUILD_ID` = `<crate version>+g<12-hex commit>`, plus `.dirty`
//!    when the engine's sources differ from that commit, or `<crate version>+unknown` without git (a
//!    source tarball, iOS packaging). Every render's sidecar records it as `engine_build`, so a render can
//!    be re-created with the build that made it (CE-R7.3). A build-time value only: the core still reads
//!    no files and no clock at runtime.
//!    **The host build**: built inside a submodule, `ENGINES_HOST_BUILD` is the embedding
//!    repo's `g<12-hex commit>` (plus `.dirty`), and sidecars record it as `host_build`; empty otherwise.
//!    The git questions live in `src/build_stamp.rs`, so they're unit-tested.
//! 2. **The C header** for the FFI surface (CE-R3.6) from `src/ffi.rs` via cbindgen. If generation fails
//!    (e.g. cbindgen can't parse something), we emit a cargo warning and leave the committed
//!    `include/engines_core.h` in place — the committed header is the source of truth the Swift shell links
//!    against, so a generation hiccup never breaks the build.

use std::path::{Path, PathBuf};

#[path = "src/build_stamp.rs"]
mod build_stamp;

fn main() {
    let crate_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    println!("cargo:rerun-if-changed=src/ffi.rs");
    println!("cargo:rerun-if-changed=cbindgen.toml");

    build_id(&crate_dir);

    let config = cbindgen::Config::from_file(crate_dir.join("cbindgen.toml")).unwrap_or_default();
    let out = crate_dir.join("include").join("engines_core.h");
    match cbindgen::Builder::new()
        .with_crate(&crate_dir)
        .with_config(config)
        .generate()
    {
        Ok(bindings) => {
            let _ = std::fs::create_dir_all(out.parent().unwrap());
            bindings.write_to_file(&out);
        }
        Err(e) => println!("cargo:warning=cbindgen header generation skipped: {e}"),
    }
}

/// What the engine's output depends on, for the `.dirty` mark: this crate (its sources, the bins, the
/// assets it embeds) and the workspace's manifest + lockfile (the dependency versions).
const ENGINE_PATHS: [&str; 3] = [".", "../../Cargo.toml", "../../Cargo.lock"];

fn build_id(crate_dir: &Path) {
    let version = std::env::var("CARGO_PKG_VERSION").unwrap();
    let git = |args: &[&str]| build_stamp::git(crate_dir, args);
    let id = match git(&["rev-parse", "--short=12", "HEAD"]) {
        Some(sha) => {
            let mut status = vec!["status", "--porcelain", "--untracked-files=no", "--"];
            status.extend(ENGINE_PATHS);
            let dirty = git(&status).is_none_or(|s| !s.is_empty());
            format!("{version}+g{sha}{}", if dirty { ".dirty" } else { "" })
        }
        None => format!("{version}+unknown"),
    };
    println!("cargo:rustc-env=ENGINES_BUILD_ID={id}");
    println!("cargo:rustc-env=ENGINES_HOST_BUILD={}", build_stamp::host_build(crate_dir).unwrap_or_default());

    // Re-run when the commit moves (HEAD, the branch's ref, packed refs — in a worktree HEAD lives in
    // its own git dir and the refs in the common one) or the engine's sources change (the dirty mark);
    // inside a submodule, also when the host's commit moves.
    // Only paths that exist: cargo re-runs a script on every build while one of its paths is missing.
    let mut watch: Vec<PathBuf> = ["src", "assets", "Cargo.toml", "../../Cargo.toml", "../../Cargo.lock"].iter().map(|p| crate_dir.join(p)).collect();
    watch.extend(build_stamp::head_paths(crate_dir));
    if let Some(host) = build_stamp::superproject(crate_dir) {
        watch.extend(build_stamp::head_paths(&host));
    }
    for p in watch.iter().filter(|p| p.exists()) {
        println!("cargo:rerun-if-changed={}", p.display());
    }
}
