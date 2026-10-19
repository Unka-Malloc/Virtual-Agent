# Agent rules for Virtual Agent

## Layer boundary (hard rule)

This crate is the user-facing facade over `awp` and `a4aa`. It owns:

- the registry document's lifecycle — storage, validation, merge,
  import/export, snapshot/commit/revert custody;
- the session facade (`va serve`) — an `a4aa.v1`-compatible boundary that
  resolves bindings to specs, pins custody records onto sessions and
  relays the underlying protocol unchanged.

It must not grow agent semantics of its own: anything a profile *does*
is awp's job, anything about reaching the agent is a4aa's job. New
behavior that does not fit custody or session relay belongs to a
proposal against the layer below, not code here.

## Verify

`scripts/verify.sh` — fmt + clippy + tests.
