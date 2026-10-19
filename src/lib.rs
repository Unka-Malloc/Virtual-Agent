//! virtual-agent — the profile-custody layer over `awp`.
//!
//! awp is a runtime: hand it a `RegistryDocument` and it materializes a
//! `VirtualAgent`. It deliberately does not care where that document
//! lives. This crate owns the part awp declines: the canonical store at
//! `~/.virtual-agent/registry.json`, validated mutation, import/export
//! for sharing, and delegation to the `awp` binary for execution.

use awp::{Registry, RegistryDocument};
use std::path::{Path, PathBuf};

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug)]
pub struct Error(pub String);

impl Error {
    fn msg(m: impl Into<String>) -> Self {
        Self(m.into())
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

/// The profile store: one canonical registry document under a root dir.
pub struct Store {
    root: PathBuf,
}

impl Store {
    /// `$VIRTUAL_AGENT_HOME`, else `~/.virtual-agent`.
    pub fn default_root() -> PathBuf {
        if let Some(h) = std::env::var_os("VIRTUAL_AGENT_HOME") {
            return PathBuf::from(h);
        }
        let home = std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        home.join(".virtual-agent")
    }

    /// Open the store at `root`, creating it (with an empty registry
    /// document) when absent.
    pub fn open(root: impl Into<PathBuf>) -> Result<Self> {
        let store = Self { root: root.into() };
        std::fs::create_dir_all(&store.root).map_err(|e| Error::msg(e.to_string()))?;
        if !store.registry_path().exists() {
            store.save(&RegistryDocument::default())?;
        }
        Ok(store)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The canonical registry document awp consumes.
    pub fn registry_path(&self) -> PathBuf {
        self.root.join("registry.json")
    }

    pub fn load(&self) -> Result<RegistryDocument> {
        let text =
            std::fs::read_to_string(self.registry_path()).map_err(|e| Error::msg(e.to_string()))?;
        serde_json::from_str(&text)
            .map_err(|e| Error::msg(format!("registry document corrupt: {e}")))
    }

    /// Validate then atomically persist. A document that fails awp's
    /// registry validation never reaches disk. The previous registry, when
    /// one exists and differs, is kept as a snapshot first — every write is
    /// recoverable.
    pub fn save(&self, doc: &RegistryDocument) -> Result<()> {
        Registry::from_document(doc.clone())
            .map_err(|e| Error::msg(format!("invalid registry document: {e}")))?;
        let text = serde_json::to_string_pretty(doc).map_err(|e| Error::msg(e.to_string()))?;
        let path = self.registry_path();
        if path.exists() && std::fs::read(&path).ok().as_deref() != Some(text.as_bytes()) {
            let _ = self.snapshot()?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, text).map_err(|e| Error::msg(e.to_string()))?;
        std::fs::rename(&tmp, &path).map_err(|e| Error::msg(e.to_string()))
    }

    fn snapshots_dir(&self) -> PathBuf {
        self.root.join("snapshots")
    }

    /// Keep at most `RETAIN` newest snapshots.
    const RETAIN: usize = 50;

    /// Copy the current registry into `snapshots/`. No-op when no registry
    /// exists yet; skips when the newest snapshot already holds the same
    /// bytes.
    pub fn snapshot(&self) -> Result<Option<String>> {
        let src = self.registry_path();
        let Ok(current) = std::fs::read(&src) else {
            return Ok(None);
        };
        let dir = self.snapshots_dir();
        std::fs::create_dir_all(&dir).map_err(|e| Error::msg(e.to_string()))?;
        if let Some(latest) = self.snapshots()?.first() {
            if std::fs::read(dir.join(&latest.name)).ok() == Some(current.clone()) {
                return Ok(None);
            }
        }
        let name = format!(
            "{}.json",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or_default()
        );
        std::fs::write(dir.join(&name), current).map_err(|e| Error::msg(e.to_string()))?;
        let all = self.snapshots()?;
        for old in all.iter().skip(Self::RETAIN) {
            let _ = std::fs::remove_file(dir.join(&old.name));
        }
        Ok(Some(name))
    }

    /// All snapshots, newest first.
    pub fn snapshots(&self) -> Result<Vec<Snapshot>> {
        let dir = self.snapshots_dir();
        let mut out = Vec::new();
        let Ok(entries) = std::fs::read_dir(&dir) else {
            return Ok(out);
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if !name.ends_with(".json") {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(entry.path()) else {
                continue;
            };
            let Ok(doc) = serde_json::from_str::<RegistryDocument>(&text) else {
                continue;
            };
            out.push(Snapshot {
                name,
                agents: doc.agents.len(),
                profiles: doc.profiles.len(),
                bindings: doc.bindings.len(),
            });
        }
        out.sort_by(|a, b| b.name.cmp(&a.name));
        Ok(out)
    }

    /// Restore a snapshot as the live registry. The snapshot is validated
    /// through awp like any save, and restoring itself snapshots the
    /// current state — a restore is itself reversible.
    pub fn restore(&self, name: &str) -> Result<RegistryDocument> {
        if name.contains('/') || name.contains('\\') || name.contains("..") {
            return Err(Error::msg("invalid snapshot name"));
        }
        let path = self.snapshots_dir().join(name);
        let text = std::fs::read_to_string(&path).map_err(|e| Error::msg(e.to_string()))?;
        let doc: RegistryDocument = serde_json::from_str(&text)
            .map_err(|e| Error::msg(format!("snapshot corrupt: {e}")))?;
        self.save(&doc)?;
        Ok(doc)
    }

    /// Merge `other` into the stored document. Existing ids are kept
    /// unless `force` is set; colliding ids collect into `conflicts`.
    /// Returns the id lists that were overwritten or skipped.
    pub fn merge(&self, other: RegistryDocument, force: bool) -> Result<MergeReport> {
        let mut doc = self.load()?;
        let mut report = MergeReport::default();
        for (id, entry) in other.agents {
            merge_one(&mut doc.agents, id, entry, force, &mut report);
        }
        for (id, entry) in other.profiles {
            merge_one(&mut doc.profiles, id, entry, force, &mut report);
        }
        for (id, entry) in other.bindings {
            merge_one(&mut doc.bindings, id, entry, force, &mut report);
        }
        self.save(&doc)?;
        Ok(report)
    }

    /// Extract a shareable subset: one profile plus the agent it targets
    /// and every binding that names it — the unit users hand around.
    pub fn export_profile(&self, profile_id: &str) -> Result<RegistryDocument> {
        let doc = self.load()?;
        let profile = doc
            .profiles
            .get(profile_id)
            .ok_or_else(|| Error::msg(format!("profile not found: {profile_id}")))?
            .clone();
        let mut out = RegistryDocument::default();
        if let Some(agent_id) = profile.agent.as_ref() {
            if let Some(agent) = doc.agents.get(agent_id) {
                out.agents.insert(agent_id.clone(), agent.clone());
            }
        }
        out.profiles.insert(profile_id.to_string(), profile);
        for (name, binding) in &doc.bindings {
            if binding.profile() == profile_id {
                out.bindings.insert(name.clone(), binding.clone());
            }
        }
        Ok(out)
    }
}

fn merge_one<T>(
    map: &mut std::collections::BTreeMap<String, T>,
    id: String,
    value: T,
    force: bool,
    report: &mut MergeReport,
) {
    if map.contains_key(&id) {
        if force {
            map.insert(id.clone(), value);
            report.overwritten.push(id);
        } else {
            report.skipped.push(id);
        }
    } else {
        map.insert(id.clone(), value);
        report.inserted.push(id);
    }
}

/// One stored snapshot: filename and entry counts for listing.
pub struct Snapshot {
    pub name: String,
    pub agents: usize,
    pub profiles: usize,
    pub bindings: usize,
}

/// Outcome of a merge: which ids were inserted, overwritten (force), or
/// skipped because they already exist.
#[derive(Default)]
pub struct MergeReport {
    pub inserted: Vec<String>,
    pub overwritten: Vec<String>,
    pub skipped: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store(tag: &str) -> (Store, PathBuf) {
        let dir = std::env::temp_dir().join(format!("va-test-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = Store::open(&dir).unwrap();
        (store, dir)
    }

    #[test]
    fn open_creates_empty_registry() {
        let (store, dir) = store("open");
        let doc = store.load().unwrap();
        assert!(doc.profiles.is_empty() && doc.agents.is_empty() && doc.bindings.is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A minimal valid document: one agent spec plus a profile naming it
    /// (`agent` is mandatory on a new profile in awp validation).
    fn doc_with_profile() -> RegistryDocument {
        serde_json::from_value(serde_json::json!({
            "agents": {
                "example-agent": {
                    "id": "example-agent",
                    "lane": "cli",
                    "command": {
                        "program": "echo",
                        "args": [],
                        "env": { "sanitized": { "inherit": [], "set": {} } }
                    }
                }
            },
            "profiles": { "p": { "agent": "example-agent", "name": "P" } }
        }))
        .unwrap()
    }

    #[test]
    fn merge_inserts_and_skips_on_conflict() {
        let (store, dir) = store("merge-skip");
        let a = doc_with_profile();
        let report = store.merge(a.clone(), false).unwrap();
        assert_eq!(report.inserted, vec!["example-agent", "p"]);

        let report = store.merge(a, false).unwrap();
        assert_eq!(report.skipped, vec!["example-agent", "p"]);
        assert_eq!(store.load().unwrap().profiles.len(), 1);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn merge_force_overwrites() {
        let (store, dir) = store("merge-force");
        let a = doc_with_profile();
        store.merge(a.clone(), false).unwrap();
        let report = store.merge(a, true).unwrap();
        assert_eq!(report.overwritten, vec!["example-agent", "p"]);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn invalid_document_never_reaches_disk() {
        let (store, dir) = store("invalid");
        let mut bad = RegistryDocument::default();
        // A profile patch without `agent` fails awp validation.
        bad.profiles
            .insert("p".into(), awp::ProfilePatch::default());
        assert!(store.merge(bad, false).is_err());
        assert!(store.load().unwrap().profiles.is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn corrupt_registry_reports_error() {
        let (store, dir) = store("corrupt");
        std::fs::write(store.registry_path(), "{not json").unwrap();
        assert!(store.load().is_err());
        let _ = std::fs::remove_dir_all(dir);
    }
}

#[cfg(test)]
mod snapshot_tests {
    use super::*;

    fn store(tag: &str) -> (Store, PathBuf) {
        let dir = std::env::temp_dir().join(format!("va-snap-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        (Store::open(&dir).unwrap(), dir)
    }

    fn doc_with(id: &str) -> RegistryDocument {
        serde_json::from_value(serde_json::json!({
            "agents": { "a": { "id": "a", "lane": "cli",
                "command": { "program": "echo", "args": [],
                    "env": { "sanitized": { "inherit": [], "set": {} } } } } },
            "profiles": { id: { "agent": "a" } }
        }))
        .unwrap()
    }

    #[test]
    fn save_snapshots_previous_version() {
        let (s, dir) = store("auto");
        s.save(&doc_with("p1")).unwrap();
        s.save(&doc_with("p2")).unwrap();
        // Two snapshots: the initial empty registry, then the p1 doc.
        let snaps = s.snapshots().unwrap();
        assert_eq!(snaps.len(), 2);
        assert_eq!(snaps[0].profiles, 1);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn restore_brings_back_previous_doc() {
        let (s, dir) = store("restore");
        s.save(&doc_with("p1")).unwrap();
        s.save(&doc_with("p2")).unwrap();
        let name = s.snapshots().unwrap()[0].name.clone();
        s.restore(&name).unwrap();
        assert!(s.load().unwrap().profiles.contains_key("p1"));
        // empty + p1 + the p2 state the restore snapshotted — reversible.
        assert_eq!(s.snapshots().unwrap().len(), 3);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn duplicate_write_leaves_no_new_snapshot() {
        let (s, dir) = store("dedupe");
        s.save(&doc_with("p1")).unwrap();
        s.save(&doc_with("p1")).unwrap();
        // Only the initial empty doc was captured; the identical rewrite
        // added nothing.
        assert_eq!(s.snapshots().unwrap().len(), 1);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn restore_rejects_bad_names() {
        let (s, dir) = store("names");
        for n in ["../x", "a/b", ".."] {
            assert!(s.restore(n).is_err());
        }
        let _ = std::fs::remove_dir_all(dir);
    }
}
