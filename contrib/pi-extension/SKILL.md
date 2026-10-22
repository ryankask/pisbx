---
name: working-in-the-sandbox
description: Work inside a pisbx sandbox — diagnose Operation not permitted, blocked hosts, and read-only paths without fighting the boundary. Use whenever a command fails with EPERM or a network call is refused, or when a task needs a path outside the workspace.
---

# Working inside pisbx

You are in a macOS Seatbelt sandbox that the kernel enforces. pi's approvals,
`sudo` and changing your command do not widen it. The policy lives outside the
sandbox, in a file you cannot read or write.

## When something fails

**`Operation not permitted` / `EPERM`.** The path is outside the policy. Say
which path failed and what you wanted to do with it; the human can grant it.
Do not retry, do not try another spelling of the same path (`/tmp` vs
`/private/tmp`, the symlink vs its target), and do not route around it with a
different tool.

**A blocked host.** The proxy answers with a refusal, not a timeout. Report the
host. Ask for it to be added to `network.allowedDomains`, or for a run with
`--domains …`.

**Reads outside the workspace.** `$HOME` is closed with carve-outs; only the
paths the policy names are readable. Toolchains on `PATH`, pi's own
installation and the workspace are open.

**`.git/config`, `git remote add`, `git submodule add`.** Blocked unless
`allowGitConfig` is set. Report it rather than editing the file by hand.

**Commit signing.** `SSH_AUTH_SOCK` is set by the policy when signing is
configured; if `git commit -S` fails, that grant is missing, not broken.

## What you can do

- Everything inside the workspace: read, write, run builds and tests.
- Write to pi's state directory (`~/.pi/agent`), except the parts that code the
  next session would load — `extensions/`, `install/`, `mcp.json`, `skills/`,
  `SYSTEM.md`, `APPEND_SYSTEM.md`, `AGENTS.md`, `prompts/`, `settings.json`.
  Those are denied on purpose: writing them would arm the next session.
- Reach the hosts on the allowlist, including over SSH via git (srt injects the
  proxy command; a bare `ssh` is not proxied).

## Ask for

Present exactly two options, one-off or permanent:

1. one run: `pisbx --allow-write <path>` (or `--domains <host>`)
2. permanent: add the path to `allowWrite` / the host to `allowedDomains` in
   `~/.config/pisbx/config.toml`

Then carry on. `pisbx why --path <path> --op read|write` prints which entry
decided any path, and where it came from.
