//! A debounced file watcher by polling (no platform notify APIs): a save is delivered once the file
//! has stopped changing for `quiet`, and only when its content differs from the last delivery —
//! an editor's autosave burst or a `touch` doesn't re-apply anything, and neither does the save of
//! a text the editor already sent as a `doc` command ([`DocWatcher::seen_text`]).

use engines_core::hash::fnv1a_bytes;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime};

pub struct DocWatcher {
    path: PathBuf,
    quiet: Duration,
    /// (mtime, size) last seen.
    seen: Option<(SystemTime, u64)>,
    /// When the file last changed, while waiting for quiet.
    changed_at: Option<Instant>,
    delivered: u64,
}

impl DocWatcher {
    /// `initial` is the text already applied (not re-delivered).
    pub fn new(path: PathBuf, quiet: Duration, initial: &str) -> Self {
        let seen = signature(&path);
        Self { path, quiet, seen, changed_at: None, delivered: fnv1a_bytes(initial.as_bytes()) }
    }

    /// `text` reached the player another way (a `doc` command): the same text saved to the file
    /// isn't delivered again (a refused one would be refused twice).
    pub fn seen_text(&mut self, text: &str) {
        self.delivered = fnv1a_bytes(text.as_bytes());
    }

    /// Poll once: `Some(text)` when a settled, genuinely new save is ready.
    pub fn poll(&mut self, now: Instant) -> Option<String> {
        let sig = signature(&self.path);
        if sig != self.seen {
            self.seen = sig;
            self.changed_at = Some(now);
            return None;
        }
        let since = self.changed_at?;
        if now.duration_since(since) < self.quiet {
            return None;
        }
        // A read that fails (a save mid-replace, a sharing or permission hiccup) stays pending and
        // is retried at the next poll.
        let text = std::fs::read_to_string(&self.path).ok()?;
        self.changed_at = None;
        let h = fnv1a_bytes(text.as_bytes());
        if h == self.delivered {
            return None;
        }
        self.delivered = h;
        Some(text)
    }
}

fn signature(path: &PathBuf) -> Option<(SystemTime, u64)> {
    let m = std::fs::metadata(path).ok()?;
    Some((m.modified().ok()?, m.len()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delivers_a_settled_new_save_once() {
        let dir = std::env::temp_dir().join(format!("engines-watch-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("doc.json5");
        std::fs::write(&p, "a").unwrap();
        let t0 = Instant::now();
        let mut w = DocWatcher::new(p.clone(), Duration::from_millis(300), "a");
        assert_eq!(w.poll(t0), None);

        std::fs::write(&p, "bb").unwrap(); // size changes ⇒ a change even within one mtime tick
        assert_eq!(w.poll(t0), None, "just changed — wait for quiet");
        assert_eq!(w.poll(t0 + Duration::from_millis(100)), None);
        assert_eq!(w.poll(t0 + Duration::from_millis(400)).as_deref(), Some("bb"));
        assert_eq!(w.poll(t0 + Duration::from_millis(900)), None, "delivered once");

        // Re-saving identical content (a new mtime at most) is not a new save.
        std::fs::write(&p, "bb").unwrap();
        let t1 = t0 + Duration::from_secs(2);
        w.poll(t1);
        assert_eq!(w.poll(t1 + Duration::from_secs(1)), None);

        // A text the editor sent as a command, then saved, isn't delivered again; a different
        // one still is.
        w.seen_text("ccc");
        std::fs::write(&p, "ccc").unwrap();
        let t2 = t0 + Duration::from_secs(4);
        w.poll(t2);
        assert_eq!(w.poll(t2 + Duration::from_secs(1)), None, "already heard as a command");
        std::fs::write(&p, "dddd").unwrap();
        let t3 = t0 + Duration::from_secs(6);
        w.poll(t3);
        assert_eq!(w.poll(t3 + Duration::from_secs(1)).as_deref(), Some("dddd"));

        // A save that can't be read yet isn't lost: it's delivered once it can be.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::write(&p, "eeeee").unwrap();
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o000)).unwrap();
            if std::fs::read(&p).is_ok() {
                // Root reads it anyway (a container running the tests as root): nothing to check.
                std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o644)).unwrap();
            } else {
                let t4 = t0 + Duration::from_secs(8);
                w.poll(t4);
                assert_eq!(w.poll(t4 + Duration::from_secs(1)), None, "unreadable for now");
                std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o644)).unwrap();
                assert_eq!(w.poll(t4 + Duration::from_secs(2)).as_deref(), Some("eeeee"), "retried, not dropped");
            }
        }
        let _ = std::fs::remove_dir_all(dir);
    }
}
