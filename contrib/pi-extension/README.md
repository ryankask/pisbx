# pisbx for pi

An extension and a skill that make the sandbox visible to the agent.

- a **lock in the footer** (`🔒 sandbox`) so a session is never mistaken for an
  unsandboxed one
- a **system-prompt paragraph** saying the boundary is kernel-enforced, that
  approvals cannot widen it, and that `EPERM` means "report, don't retry"
- a **`/pisbx` command** printing what the policy actually grants
- a **`tool_result` hook** that turns a bare `Operation not permitted` into
  that same guidance

Both are inert unless `PISBX_ACTIVE=1`, which pisbx sets for the sandboxed
process, so installing them globally costs nothing outside pisbx.

## Install

```bash
pi install ./contrib/pi-extension          # from a pisbx checkout
# or copy into the agent directory:
cp -R contrib/pi-extension ~/.pi/agent/extensions/pisbx
```

pisbx deliberately does not install this for you. `~/.pi/agent/extensions` is
deny-write inside the sandbox — installing there would mean pisbx writing into
the directory it tells the sandbox not to write — and an extension runs
in-process with the session's full capabilities, so installing one is a trusted
act.

## What it reads

| Variable | Meaning |
|---|---|
| `PISBX_ACTIVE` | `1` when the session is sandboxed |
| `PISBX_SETTINGS` | the generated srt settings document for this run |
| `PISBX_POLICY` | path to the policy file (not readable inside the sandbox) |
| `PISBX_VERSION` | pisbx version |

`[pisbx.env]` cannot shadow these: `PISBX_*` is rejected in the policy file.
