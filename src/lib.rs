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
    /// registry validation never reaches disk.
    pub fn save(&self, doc: &RegistryDocument) -> Result<()> {
        Registry::from_document(doc.clone())
            .map_err(|e| Error::msg(format!("invalid registry document: {e}")))?;
        let text = serde_json::to_string_pretty(doc).map_err(|e| Error::msg(e.to_string()))?;
        let path = self.registry_path();
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, text).map_err(|e| Error::msg(e.to_string()))?;
        std::fs::rename(&tmp, &path).map_err(|e| Error::msg(e.to_string()))
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
