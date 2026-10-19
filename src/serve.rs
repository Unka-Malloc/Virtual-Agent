//! `va serve` — the Virtual-Agent session facade.
//!
//! A JSON-RPC 2.0 NDJSON service over stdio, wire-compatible with
//! `a4aa.v1`: hosts keep one `va serve` child per process and speak the
//! same session/open|prompt|cancel|close protocol they would to
//! `a4aa serve` — except session/open names a registry *selector*
//! (`profile` or `binding`, plus an optional `patch` merged into that
//! profile) instead of carrying a raw AgentSpec. Virtual-Agent resolves
//! the selector through awp against the custody store, pins the resolved
//! spec's content hash into the response (`custody`), and relays
//! everything else verbatim — events, permission requests and
//! cancellation flow through untouched, so uncertain-effect and
//! fail-closed semantics survive the extra hop intact.
//!
//! For agents without a streaming lane the facade is also where a
//! run-to-completion execution can be expressed as a streaming session;
//! synthesis arrives with the executor lane — the relay does not
//! fabricate traffic it cannot substantiate.

use crate::{fnv1a_hex, Error, Result, Store};
use serde_json::{Map, Value};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};

fn a4aa_binary() -> String {
    std::env::var("A4AA_BIN").unwrap_or_else(|_| "a4aa".into())
}

/// The custody record pinned onto a session/open result: which selector
/// produced the spec and what content the session actually runs.
#[derive(Clone, Debug, serde::Serialize)]
pub struct Custody {
    pub selector: String,
    #[serde(rename = "specHash")]
    pub spec_hash: String,
    #[serde(rename = "registryHash")]
    pub registry_hash: String,
}

/// Resolve the session/open selector against the custody store and
/// produce the a4aa-shaped params plus the custody pin. `patch` fields
/// merge into the resolved profile's document entry before validation —
/// per-request profile tuning without touching the stored document.
pub fn open_params(store: &Store, params: &Map<String, Value>) -> Result<(Value, Custody)> {
    let binding = params.get("binding").and_then(Value::as_str);
    let profile = params.get("profile").and_then(Value::as_str);
    let (selector, label) = match (binding, profile) {
        (Some(b), None) => ("binding", b),
        (None, Some(p)) => ("profile", p),
        _ => {
            return Err(Error::msg(
                "session/open needs exactly one of binding or profile",
            ))
        }
    };
    let mut doc = store.load()?;
    let registry_text = std::fs::read_to_string(store.registry_path()).unwrap_or_default();
    let profile_id = if selector == "binding" {
        doc.bindings
            .get(label)
            .ok_or_else(|| Error::msg(format!("binding not found: {label}")))?
            .profile()
            .to_string()
    } else {
        label.to_string()
    };
    let entry = doc
        .profiles
        .get_mut(&profile_id)
        .ok_or_else(|| Error::msg(format!("profile not found: {profile_id}")))?;
    if let Some(patch) = params.get("patch").and_then(Value::as_object) {
        let mut base = serde_json::to_value(&*entry).map_err(|e| Error::msg(e.to_string()))?;
        if let Some(base_map) = base.as_object_mut() {
            for (k, v) in patch {
                base_map.insert(k.clone(), v.clone());
            }
        }
        *entry = serde_json::from_value(base).map_err(|e| Error::msg(e.to_string()))?;
    }
    let registry = awp::Registry::from_document(doc)
        .map_err(|e| Error::msg(format!("invalid registry: {e}")))?;
    let agent = if selector == "binding" {
        registry.resolve_binding(label)
    } else {
        registry.resolve(label)
    }
    .map_err(|e| Error::msg(format!("resolve {selector} {label}: {e}")))?;
    let spec = serde_json::to_value(agent.spec()).map_err(|e| Error::msg(e.to_string()))?;
    let custody = Custody {
        selector: format!("{selector}:{label}"),
        spec_hash: fnv1a_hex(spec.to_string().as_bytes()),
        registry_hash: fnv1a_hex(registry_text.as_bytes()),
    };
    let mut out = Map::new();
    out.insert("spec".into(), spec);
    for key in ["cwd", "env", "options", "resume"] {
        if let Some(v) = params.get(key) {
            out.insert(key.into(), v.clone());
        }
    }
    Ok((Value::Object(out), custody))
}

/// The pending custody for one in-flight session/open, keyed by request id.
type Pending = Arc<Mutex<HashMap<String, Custody>>>;

fn id_key(id: &Value) -> String {
    match id {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Spawn `a4aa serve` and run the relay until host stdin closes — EOF
/// drops the child's stdin, which closes its sessions exactly as a
/// direct sidecar teardown would.
pub fn run(store: &Store) -> Result<()> {
    let mut child: Child = Command::new(a4aa_binary())
        .arg("serve")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|e| Error::msg(format!("spawn a4aa serve: {e}")))?;
    let mut child_in = child.stdin.take().expect("piped stdin");
    let child_out = child.stdout.take().expect("piped stdout");

    let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
    let upstream = {
        let pending = Arc::clone(&pending);
        std::thread::spawn(move || {
            let stdout = std::io::stdout();
            // Lock per write, not for the whole relay: the input thread
            // must be able to answer a failed session/open while a live
            // session's events are flowing through here.
            for line in BufReader::new(child_out).lines() {
                let Ok(line) = line else { break };
                let mut v: Value = match serde_json::from_str(&line) {
                    Ok(v) => v,
                    Err(_) => {
                        let _ = writeln!(stdout.lock(), "{line}");
                        continue;
                    }
                };
                if let Some(id) = v.get("id") {
                    let key = id_key(id);
                    let custody = pending.lock().unwrap().remove(&key);
                    if let (Some(custody), Some(result)) =
                        (custody, v.get_mut("result").and_then(Value::as_object_mut))
                    {
                        result.insert(
                            "custody".into(),
                            serde_json::to_value(&custody).unwrap_or(Value::Null),
                        );
                    }
                }
                let mut out = stdout.lock();
                let _ = writeln!(out, "{v}");
                let _ = out.flush();
            }
        })
    };

    let stdin = std::io::stdin();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        let mut v: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let is_open = v.get("method").and_then(Value::as_str) == Some("session/open");
        if is_open {
            let id = v.get("id").cloned().unwrap_or(Value::Null);
            let params = v.get("params").cloned().unwrap_or(Value::Null);
            let empty = Map::new();
            let params_obj = params.as_object().unwrap_or(&empty);
            match open_params(store, params_obj) {
                Ok((new_params, custody)) => {
                    pending.lock().unwrap().insert(id_key(&id), custody);
                    v.as_object_mut()
                        .unwrap()
                        .insert("params".into(), new_params);
                }
                Err(e) => {
                    let reply = serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "error": {"code": -32602, "message": e.to_string(),
                                  "data": {"stage": "resolve"}},
                    });
                    let stdout = std::io::stdout();
                    let mut out = stdout.lock();
                    let _ = writeln!(out, "{reply}");
                    let _ = out.flush();
                    continue;
                }
            }
        }
        let _ = writeln!(child_in, "{v}");
        if child_in.flush().is_err() {
            break;
        }
    }
    drop(child_in);
    let _ = upstream.join();
    let _ = child.wait();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store(tag: &str) -> (Store, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("va-serve-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        (Store::open(&dir).unwrap(), dir)
    }

    fn seed(store: &Store) {
        let doc: awp::RegistryDocument = serde_json::from_value(serde_json::json!({
            "agents": { "a": { "id": "a", "lane": "cli",
                "command": { "program": "echo", "args": ["hi"],
                    "env": { "sanitized": { "inherit": [], "set": {} } } } } },
            "profiles": { "p": { "agent": "a", "model": "m0" } },
            "bindings": { "worker": { "profile": "p", "prompt": "do it" } }
        }))
        .unwrap();
        store.save(&doc, "set").unwrap();
    }

    #[test]
    fn open_resolves_binding_and_pins_custody() {
        let (s, dir) = store("binding");
        seed(&s);
        let params = serde_json::json!({
            "binding": "worker", "cwd": "/tmp/w", "options": {"model": "m1"}
        });
        let (params, custody) = open_params(&s, params.as_object().unwrap()).unwrap();
        assert_eq!(params["cwd"], "/tmp/w");
        assert_eq!(params["options"]["model"], "m1");
        assert!(params["spec"]["command"]["program"].as_str().is_some());
        assert!(params.get("binding").is_none()); // selector never leaks downstream
        assert_eq!(custody.selector, "binding:worker");
        assert_eq!(custody.spec_hash.len(), 16);
        assert_eq!(custody.registry_hash.len(), 16);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn patch_merges_without_touching_the_stored_document() {
        let (s, dir) = store("patch");
        seed(&s);
        let params = serde_json::json!({
            "profile": "p",
            "patch": {"model": "override", "effort": "high"}
        });
        let (params, _) = open_params(&s, params.as_object().unwrap()).unwrap();
        assert!(params["spec"].is_object());
        // The stored document keeps its original model — patch is per-request.
        let doc = s.load().unwrap();
        assert_eq!(doc.profiles["p"].model.as_deref(), Some("m0"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn missing_selector_and_unknown_binding_fail() {
        let (s, dir) = store("bad");
        seed(&s);
        let empty = serde_json::json!({});
        assert!(open_params(&s, empty.as_object().unwrap()).is_err());
        let both = serde_json::json!({"profile": "p", "binding": "worker"});
        assert!(open_params(&s, both.as_object().unwrap()).is_err());
        let unknown = serde_json::json!({"binding": "nope"});
        assert!(open_params(&s, unknown.as_object().unwrap()).is_err());
        let _ = std::fs::remove_dir_all(dir);
    }
}
