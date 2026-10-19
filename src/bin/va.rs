//! va — Virtual Agent CLI: the profile-management facade over awp.
//!
//! Owns the canonical registry document at `~/.virtual-agent/registry.json`
//! (`VIRTUAL_AGENT_HOME` overrides). Execution is delegated to the `awp`
//! binary — this CLI manages custody, not runtime behavior.

use awp::RegistryDocument;
use virtual_agent::{Error, Result, Store};

fn usage() -> ! {
    eprintln!(
        "usage:
  va init
  va list [agents|profiles|bindings]
  va show <agent|profile|binding> <id>
  va set <agent|profile|binding> <id> --file <fragment.json>
  va rm <agent|profile|binding> <id>
  va edit                         open the registry in $EDITOR
  va export [<profile>] [--out FILE]   share: whole doc or one profile unit
  va import <file> [--force]      merge a registry document
  va snapshots                    list commits (op + content hash + version)
  va restore <snapshot>           roll back to a captured version
  va revert                       undo the most recent change
  va run <profile|binding> [extra awp args...]
  va resolve <profile|binding> [extra awp args...]
  va serve                        a4aa.v1-compatible session facade over the registry"
    );
    std::process::exit(2)
}

fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

fn open_store() -> Result<Store> {
    Store::open(Store::default_root())
}

fn main() {
    if let Err(e) = run() {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("init") => {
            let store = open_store()?;
            println!("store ready: {}", store.registry_path().display());
        }
        Some("list") => {
            let kind = args.get(1).map(String::as_str).unwrap_or("profiles");
            let doc = open_store()?.load()?;
            match kind {
                "agents" => doc.agents.keys().for_each(|k| println!("{k}")),
                "bindings" => doc.bindings.keys().for_each(|k| println!("{k}")),
                "profiles" => doc.profiles.keys().for_each(|k| println!("{k}")),
                _ => usage(),
            }
        }
        Some("show") => {
            let (kind, id) = (args.get(1), args.get(2));
            let (kind, id) = match (kind, id) {
                (Some(k), Some(i)) => (k.as_str(), i.as_str()),
                _ => usage(),
            };
            let doc = open_store()?.load()?;
            let value = match kind {
                "agent" => doc.agents.get(id).map(|v| serde_json::to_value(v).unwrap()),
                "profile" => doc
                    .profiles
                    .get(id)
                    .map(|v| serde_json::to_value(v).unwrap()),
                "binding" => doc
                    .bindings
                    .get(id)
                    .map(|v| serde_json::to_value(v).unwrap()),
                _ => usage(),
            };
            let value = value.ok_or_else(|| Error(format!("{kind} not found: {id}")))?;
            println!("{}", serde_json::to_string_pretty(&value).unwrap());
        }
        Some("set") => {
            let (kind, id) = (args.get(1).cloned(), args.get(2).cloned());
            let file = flag(&args, "--file").ok_or_else(|| Error("missing --file".into()))?;
            let (kind, id) = match (kind, id) {
                (Some(k), Some(i)) => (k, i),
                _ => usage(),
            };
            let text =
                std::fs::read_to_string(&file).map_err(|e| Error(format!("read {file}: {e}")))?;
            let store = open_store()?;
            let mut doc = store.load()?;
            match kind.as_str() {
                "agent" => {
                    doc.agents.insert(
                        id.clone(),
                        serde_json::from_str(&text).map_err(|e| Error(e.to_string()))?,
                    );
                }
                "profile" => {
                    doc.profiles.insert(
                        id.clone(),
                        serde_json::from_str(&text).map_err(|e| Error(e.to_string()))?,
                    );
                }
                "binding" => {
                    doc.bindings.insert(
                        id.clone(),
                        serde_json::from_str(&text).map_err(|e| Error(e.to_string()))?,
                    );
                }
                _ => usage(),
            }
            store.save(&doc, "set")?;
            println!("{kind} {id} saved");
        }
        Some("rm") => {
            let (kind, id) = (args.get(1).cloned(), args.get(2).cloned());
            let (kind, id) = match (kind, id) {
                (Some(k), Some(i)) => (k, i),
                _ => usage(),
            };
            let store = open_store()?;
            let mut doc = store.load()?;
            let removed = match kind.as_str() {
                "agent" => doc.agents.remove(&id).is_some(),
                "profile" => doc.profiles.remove(&id).is_some(),
                "binding" => doc.bindings.remove(&id).is_some(),
                _ => usage(),
            };
            if !removed {
                return Err(Error(format!("{kind} not found: {id}")));
            }
            store.save(&doc, "rm")?;
            println!("{kind} {id} removed");
        }
        Some("edit") => {
            let store = open_store()?;
            let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vi".into());
            let status = std::process::Command::new(editor)
                .arg(store.registry_path())
                .status()
                .map_err(|e| Error(e.to_string()))?;
            if !status.success() {
                return Err(Error("editor exited non-zero".into()));
            }
            // Saving is only acknowledged if the document still validates.
            let doc = store.load()?;
            awp::Registry::from_document(doc).map_err(|e| {
                Error(format!(
                    "edited document invalid, kept on disk for repair: {e}"
                ))
            })?;
            println!("registry valid");
        }
        Some("export") => {
            let positional = args.get(1).filter(|a| !a.starts_with('-'));
            let store = open_store()?;
            let doc = match positional {
                Some(id) => store.export_profile(id)?,
                None => store.load()?,
            };
            let text = serde_json::to_string_pretty(&doc).map_err(|e| Error(e.to_string()))?;
            match flag(&args, "--out") {
                Some(path) => {
                    std::fs::write(&path, text).map_err(|e| Error(e.to_string()))?;
                    println!("exported to {path}");
                }
                None => println!("{text}"),
            }
        }
        Some("import") => {
            let file = args.get(1).ok_or_else(|| Error("missing file".into()))?;
            let text =
                std::fs::read_to_string(file).map_err(|e| Error(format!("read {file}: {e}")))?;
            let doc: RegistryDocument =
                serde_json::from_str(&text).map_err(|e| Error(format!("invalid document: {e}")))?;
            let force = args.iter().any(|a| a == "--force");
            let report = open_store()?.merge(doc, force)?;
            println!(
                "imported={} overwritten={} skipped={}",
                report.inserted.len(),
                report.overwritten.len(),
                report.skipped.len()
            );
            if !report.skipped.is_empty() {
                println!("skipped existing ids: {}", report.skipped.join(", "));
            }
        }
        Some("snapshots") => {
            let store = open_store()?;
            let commits = store.commits()?;
            for s in store.snapshots()? {
                let c = commits.iter().find(|c| c.name == s.name);
                let (op, hash) = c
                    .map(|c| (c.op.as_str(), c.hash.as_str()))
                    .unwrap_or(("?", ""));
                println!(
                    "{}  {:<8} {}  agents={} profiles={} bindings={}",
                    s.name, op, hash, s.agents, s.profiles, s.bindings
                );
            }
        }
        Some("restore") => {
            let name = args
                .get(1)
                .ok_or_else(|| Error("missing snapshot name".into()))?;
            open_store()?.restore(name)?;
            println!("restored {name}");
        }
        Some("revert") => {
            open_store()?.revert()?;
            println!("reverted to previous snapshot");
        }
        Some("serve") => {
            let store = open_store()?;
            virtual_agent::serve::run(&store)?;
        }
        Some(cmd @ ("run" | "resolve")) => {
            let name = args.get(1).ok_or_else(|| Error("missing name".into()))?;
            let store = open_store()?;
            let doc = store.load()?;
            let selector = if doc.bindings.contains_key(name) && !doc.profiles.contains_key(name) {
                "--binding"
            } else {
                "--profile"
            };
            let extra: Vec<String> = args.iter().skip(2).cloned().collect();
            let status = std::process::Command::new("awp")
                .arg(cmd)
                .arg("--config")
                .arg(store.registry_path())
                .arg(selector)
                .arg(name)
                .args(&extra)
                .status()
                .map_err(|e| Error(format!("failed to launch awp: {e}")))?;
            std::process::exit(status.code().unwrap_or(1));
        }
        _ => usage(),
    }
    Ok(())
}
