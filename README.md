# virtual-agent — `va`

The user-facing profile-management facade over
[`awp`](https://crates.io/crates/awp). awp is a runtime: hand it a
`RegistryDocument` and it materializes a `VirtualAgent` — it deliberately
does not care where that document lives, how profiles are saved, edited,
restored, updated or shared. **`va` owns that custody layer.**

```
~/.virtual-agent/registry.json   the canonical registry document
        │  managed by va (validate → atomic save → merge → share)
        ▼
   awp resolve / awp run         the runtime consumes it unchanged
```

Design premise: same as the sibling projects in
[docs/design-direction.md](docs/design-direction.md) — this layer adds
*user-side custody*, not new agent semantics.

## Commands

| command | what it does |
|---|---|
| `va init` | create the store (`$VIRTUAL_AGENT_HOME` or `~/.virtual-agent`) |
| `va list [agents\|profiles\|bindings]` | enumerate registry entries |
| `va show <kind> <id>` | print one entry as JSON |
| `va set <kind> <id> --file x.json` | upsert an agent/profile/binding fragment |
| `va rm <kind> <id>` | remove an entry |
| `va edit` | open the registry in `$EDITOR`; validate on save |
| `va export [<profile>] [--out f]` | whole doc, or one profile + its agent + bindings |
| `va import <file> [--force]` | merge a registry document; collisions skip or overwrite |
| `va run <name>` / `va resolve <name>` | delegate to `awp` with the managed registry |

Every mutation validates through `awp::Registry::from_document` before it
reaches disk — the store never holds a document the runtime would reject.

## Verify

`scripts/verify.sh` — fmt + clippy + tests.
