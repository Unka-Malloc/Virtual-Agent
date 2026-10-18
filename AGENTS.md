# Agent rules for Virtual Agent

Follow the workspace rules in `../AGENTS.md` and the global requirements in
`~/AGENTS.md`. Publication audits run and complete before any signature,
unlock or credential is requested; self-review covers the whole candidate,
not only the diff.

## Layer boundary (hard rule)

This crate is the custody facade over `awp`. It owns the registry
document's lifecycle — storage, validation, merge, import/export. It
must not grow agent-execution semantics: anything a profile *does* is
awp's job, anything about reaching the agent is a4aa's job. New behavior
that does not fit custody belongs to a proposal against the layer below,
not code here.

## Verify

`scripts/verify.sh` — fmt + clippy + tests.
