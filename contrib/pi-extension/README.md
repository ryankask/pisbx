# pisbx for pi

An extension and a skill that make the sandbox visible to the agent.

- a **lock on the footer's first line** (`~/Projects/pisbx (main)` followed by
  `nf-seti-lock`, in the same dim colour) so a session is never mistaken for an
  unsandboxed one without paying a footer line for it. The glyph is private-use
  and one cell wide on purpose: a colour emoji's bitmap is scaled to the cell
  height and comes out wider than the two cells it is given in a narrow-cell
  font, and kitty truncates rather than letting it overflow
  (kovidgoyal/kitty#8838), which cut the right edge off the padlock. Set
  `PISBX_LOCK` to use another glyph.
- the footer reproduces pi's two default lines: `ctx.ui.setStatus()` can only
  add a line of its own, and the default footer component needs the session
  object an extension does not get. `contrib/pi-extension/footer.ts` mirrors
  pi 1.4.0's formatting and lists the differences in its header comment
  (no `(sub)` subscription marker, no `(auto)` indicator, no routed-model arrow)
- a **system-prompt paragraph** saying the boundary is kernel-enforced, that
  approvals cannot widen it, and that `EPERM` means "report, don't retry"
- a **`/pisbx` command** printing what the policy actually grants
- a **`tool_result` hook** that turns a bare `Operation not permitted` into
  that same guidance

Both are inert unless `PISBX_ACTIVE=1`, which pisbx sets for the sandboxed
process, so installing them globally costs nothing outside pisbx.

## Install

A git source keeps `~/.pi/agent/settings.json` free of any machine-specific
path, and `pi update` picks up changes:

```bash
pi install git:github.com/ryankask/pisbx
```

A private fork needs credentials for the HTTPS clone: either `gh auth setup-git`
(so git asks gh for its keyring token), or the SSH spelling
`git:git@github.com:<you>/pisbx`. Do **not** use
`pi install ./contrib/pi-extension`: a local source is written to
`settings.json` as a path relative to that file, so it breaks when the checkout
moves and differs on every machine.

If you would rather not have a git checkout of pisbx just for this, copy the two
files instead — an extension and a skill in the agent directory are discovered
without any settings entry at all:

```bash
cp contrib/pi-extension/index.ts  ~/.pi/agent/extensions/pisbx.ts
cp contrib/pi-extension/SKILL.md  ~/.pi/agent/skills/pisbx.md
```

Publishing to npm (`pi install npm:pisbx`) would give the same stability as the
git source; it is not published yet.

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
| `PISBX_LOCK` | yours to set: the footer's lock glyph, default a space plus `nf-seti-lock` |

`PISBX_LOCK` expands `\uXXXX`, `\u{XXXXX}` and `\xXX`, so another glyph can be
tried from a shell without typing a private-use codepoint; an empty value drops
the lock. `[pisbx.env]` cannot shadow these: `PISBX_*` is rejected in the policy
file.
