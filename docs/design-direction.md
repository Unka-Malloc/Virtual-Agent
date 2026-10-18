# Design direction: the custody facade

This workspace family abstracts over agent hosts in **two opposite
directions** (see the sibling documents in A4AA, AwP and
AgentOps-Runtime for the full comparison). Virtual Agent sits on the
**native-max side**, one level above the runtime:

| layer | repo | owns |
|---|---|---|
| language | A4AA | what agents natively speak (adapter lanes, mechanism surface) |
| composition | AwP | which profiled agent you get (registry → `VirtualAgent`) |
| **custody** | **Virtual Agent** | **where profiles live: save / edit / update / share** |

AwP is intentionally custody-free: a `RegistryDocument` is just data to
it — a config file, a section of a larger config, a synced bundle; the
crate does not care where it comes from. That boundary is what makes
this layer possible: Virtual Agent manages the document's lifecycle
(validation, atomic persistence, merge, export) and delegates every
execution to `awp` unchanged. It adds no agent semantics of its own —
it cannot, because the runtime it wraps does not define any either.

Corollaries:

- A profile that fails `awp::Registry::from_document` never lands in the
  store — custody without validation is just file copying.
- `va` never talks to an agent process directly. Execution stays in the
  runtime; custody stays here.
- Sharing is a document operation: `va export <profile>` extracts the
  profile plus the agent spec it needs and the bindings that name it —
  a self-contained fragment another store can merge.
