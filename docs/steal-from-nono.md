# What to steal from nono

Notes from reading [nolabs-ai/nono](https://github.com/nolabs-ai/nono) at
v0.80.0 (October 2026): README, `docs/cli/**`, `crates/nono-cli/data/policy.json`,
`crates/nono-cli/src/profile/`, and the [`nono-packs` pi package](https://github.com/nolabs-ai/nono-packs/tree/main/pi).

nono is a much larger project — a Rust agent-runtime security platform with
credential brokering, per-command tool sandboxes, audit, rollback, a profile
registry, and Linux/WSL2 support. pisbx should not become that. But four ideas
are worth taking, and one of them fixes a real hole in what we built.

The test for anything here is the same as the rest of pisbx: does it let us say
something true about what the kernel will do, without adding a subsystem we
cannot explain?

## Summary

| # | Idea | Why | Effort | Priority |
|---|---|---|---|---|
| 1 | Phantom credentials + reverse proxy | pi's real API key currently sits inside the sandbox | large | P1 |
| 2 | Name the secret classes, and finish the persistence deny-list | the agent can arm pi's next session through files we forgot | small | P1 |
| 3 | Runtime `pisbx why --path --op` | static `--explain` cannot answer "can I write this file now?" | small | P2 |
| 4 | A pi extension + skill shipped as source | pi does not know it is sandboxed; every EPERM is a mystery | small | P2 |
| 5 | L7 endpoint rules on credential routes | hostname is the wrong unit for an API key | medium | P3 |

Already done, from the nono comparison: `NODE_USE_ENV_PROXY=1` on the srt child.
nono documents the exact failure (curl works, Node `fetch` returns `ENOTFOUND`)
because Node ignores `HTTP(S)_PROXY`; see `docs/cli/features/networking.mdx`
and the fix in `src/srt.rs`. Keep it pinned with a selftest.

---

## 1. Phantom credentials and a reverse proxy

### What nono does

The sandbox never holds a real key. nono injects a short-lived phantom token
(`nono_sess_…`) into the child environment, runs a reverse proxy outside the
sandbox, and swaps the phantom for the real key from the macOS Keychain,
1Password, Bitwarden, Apple Passwords, or a file/env reference at the moment the
request leaves the machine. Routes are per-provider (`upstream`,
`inject_header`, `credential_format`, `env_var`), can be scoped to a single
mediated command, and can be narrowed further with endpoint rules
(`GET /repos/*/issues/**`).

The pi pack goes further in its README: it explicitly warns that pi's
`~/.pi/agent/auth.json` is visible inside the sandbox because pi must read it,
and recommends phantom routes instead of storing keys there.

### Why pisbx needs it

Our boundary is honest but incomplete on this point. `close $HOME, open ~/.pi`
means the sandbox can read `~/.pi/agent/auth.json`, which holds the real model
key. The domain allowlist bounds where that key can go, but the provider host is
on the allowlist by definition, so the key is both usable and exfiltratable to
the one host it is valid for. A compromised extension, a prompt-injected
`bash` call, or a malicious custom tool can read it.

The README says "a secret inside a granted directory is readable; deny it
explicitly". That is true, but we cannot deny `auth.json` and still have pi
authenticate. The only way out is to remove the real secret from the sandbox.

### Design sketch

```
pisbx
 ├─ starts a reverse proxy on 127.0.0.1:<ephemeral> (unsandboxed)
 ├─ generates a phantom token per session, exports it as the provider key
 ├─ real keys stay in the Keychain, read on first use
 └─ srt child gets: OPENAI_API_KEY=pisbx_sess_…, plus a provider base URL
      pointing at http://127.0.0.1:<port>/openai
```

Proxy: a small `hyper` server (or `tiny_http` to stay dep-light) that matches
`/<service>/...`, validates the phantom, injects `Authorization` /
`x-api-key`, and forwards to the route's `upstream` over TLS. Fail closed:
unknown route, wrong method, missing token → 403, never "pass through".

Pointing pi at it: pi's built-in providers take a `baseUrl` override, and an
extension can call `pi.registerProvider("anthropic", { baseUrl: "http://127.0.0.1:<port>/anthropic" })`
while keeping built-in models and auth
(`docs/custom-provider.md`). That is the clean integration: a trusted,
user-installed extension reads `PISBX_CRED_PROXY_*` from the environment and
registers the routes. A `models.json` edit is the fallback, but pisbx must not
write that file (see §2).

Two constraints to resolve before building:

- **Localhost egress.** srt only permits connections to its own proxy port
  unless `network.allowLocalBinding` is set; that flag allows
  `network-outbound (remote ip "localhost:*")`, which is broader than one port.
  We already pass srt settings through add-only, so the user can enable it
  today, but a first-class `--credential-proxy` should document the tradeoff or
  upstream a narrower "one localhost port" option to srt.
- **pi's auth model.** The proxy needs the upstream to receive the real key. If
  pi stores an API key in `auth.json`, the extension should not read it; the
  real key must live in the Keychain and the phantom must be what pi sees.
  OAuth-refresh flows (ChatGPT auth) will not work this way without more
  thought; treat API-key providers first.

### What not to copy

Do not build a credential *manager*. Read from the Keychain via `security
find-generic-password` (or the `keyring` crate), or accept `op://` by shelling
to `op read`. Scope and endpoint rules come later; the security property is
"the real key is not in the sandbox", not "nono parity".

### Acceptance test

- `env | grep -i key` inside the sandbox shows only `pisbx_sess_*`.
- A request through the proxy returns a real completion.
- `grep -r "$REAL_KEY"` over the writable set finds nothing.
- Removing the phantom from the request yields 403, not a direct upstream call.

---

## 2. Name the secret classes, and finish the persistence deny-list

**Done** — the extra agent-directory paths are in the floor, `--init` names the
secret classes, and `--selftest` asserts `mcp.json` and `SYSTEM.md`. Doing it
exposed a bug worth recording: `paths::expand` only produced the resolved
spelling when the path already existed, so every deny under the symlinked
`~/.pi` silently did nothing until something else created the file. Resolution
now walks up to the deepest existing ancestor, which is also what makes a glob
deny work on files that are not there yet.

nono's built-in `default` profile is worth reading as a checklist, not as
config to copy (`crates/nono-cli/data/policy.json`). Its required groups deny:
`~/.ssh`, `~/.gnupg`, `~/.aws`, `~/.azure`, `~/.config/gcloud`, `~/.kube`,
`~/.docker`, `~/.git-credentials`, `~/.netrc`, `~/.npmrc`, keychains and
password managers, browser profile data, Messages/Mail/Cookies, shell history,
and shell configs. Sensitive paths get "allow discovery, deny content" on
macOS: `file-read-metadata` is allowed so programs can stat the path, while
`file-read-data` is denied.

pisbx already closes `$HOME` wholesale, which is stronger for reads. Steal the
*taxonomy* for two places where it still matters:

1. **The scaffold.** `--init` currently suggests `~/.config/git`,
   `~/.ssh/known_hosts`, and the 1Password socket. Add comments for the other
   classes so users do not hand-grant `~/.ssh` or `~/.aws` without thinking.
2. **The persistence floor.** While comparing, we found that pi auto-loads far
   more from `<agent-dir>` than pisbx deny-writes
   (`docs/configuration.md` in the pi install). The current floor denies
   `extensions/`, `install/`, `settings.json`, `keybindings.json`,
   `models-store.json`, `npm`, and `web-search-cache`. Missing:

| Path (under `~/.pi/agent`) | Loaded as | Risk if writable |
|---|---|---|
| `mcp.json` | MCP servers for every project | next-session code execution |
| `models.json` | compatible endpoints / overrides | redirect provider traffic |
| `SYSTEM.md`, `APPEND_SYSTEM.md` | system prompt replacement/addition | durable prompt injection |
| `AGENTS.md`, `AGENTS.override.md`, `CLAUDE.md` | user context files | durable prompt injection |
| `skills/` | auto-discovered skills | advertised instructions + scripts |
| `prompts/` | slash commands | user-invoked instructions |
| `themes/` | UI theme | cosmetic, low risk |
| `auth.json` | credentials | tampering vs. OAuth refresh; decide explicitly |

This is the same "arm the next session" argument the README makes for
`extensions/`, applied to every door pi leaves open. Add all of the above
except `auth.json` to the built-in `deny_write` list. For `auth.json`, choose:
deny it and require login outside the sandbox, or allow it and document that
token refresh wins, credential tampering is possible. Do not leave the choice
implicit.

The same list applies to nono, which grants `$HOME/.pi` read+write in its pi
pack — so this is not a reason to switch, it is a reason to finish our floor.

Implementation is mechanical: `src/floor.rs` (built-in denies), the `--init`
comments in `src/init.rs`, and the selftest additions for the highest-value
files (`mcp.json`, `SYSTEM.md`).

---

## 3. `pisbx why --path <path> --op read|write`

nono's `nono why --self --path … --op …` answers a runtime question in the
vocabulary of the sandbox that is actually in force. Its pi extension uses that
to turn a generic `EPERM` into actionable guidance.

`pisbx --explain` is the policy document, not the answer. Two additions:

- **Outside the sandbox**, `pisbx why` resolves the same `Policy` and reports:
  decision, matching entry, source (`built-in` / `config` / `flag`), and the
  winning deny. Reuse the path expansion and resolved-symlink logic in
  `src/paths.rs`; compare path components, not string prefixes.
- **Inside the sandbox**, the answer must come from the kernel, not from the
  policy. A `--probe` mode can attempt the access (`access(2)` for read/write,
  or open with `O_WRONLY`) and report what actually happened, which is the only
  claim that survives a deliberately wrong policy.

Exit codes should make it scriptable: `0` allowed, `1` denied, `64` usage.
Keep the static and probed results visibly distinct in the output; never imply
the static answer is enforcement.

This is a small feature that pays for itself every time the agent hits a wall.

---

## 4. Ship a pi extension and skill (as source, not auto-installed)

nono's pi pack is a good model for how the agent should experience the sandbox:
a status indicator (`nono` in the footer), a `before_agent_start` system-prompt
paragraph explaining that the boundary is OS-enforced and that `sudo`, `chmod`,
and pi approvals cannot widen it, a `tool_result` hook that recognizes denial
patterns and appends concrete next steps, and a `/nono-status` command that
prints the capability file. Its `SKILL.md` teaches the same workflow:
diagnose with `nono why --self`, then present exactly two options (one-off
restart or profile draft/promote).

We should ship the analogue in `contrib/pi-extension/` with an install note,
not install it automatically. Reasons:

- `~/.pi/agent/extensions` is deny-write by design; auto-installing would mean
  pisbx writing into the directory it tells the sandbox not to write.
- Extensions execute in-process with the session's full capabilities. Installing
  one is a trusted act, same as installing any custom tool.

What it needs from pisbx: export `PISBX_ACTIVE=1` and a readable path to the
session's `--explain` output (the generated settings file already exists under
`TMPDIR` while the session runs; expose its path rather than re-resolving).
Then the extension can show the effective write/domain counts and answer
`/pisbx-status` without guessing.

Future: use the same extension to register credential-proxy base URLs (§1).

---

## 5. L7 endpoint rules on credential routes

nono lets a credential route carry `endpoint_rules` — `{method, path}` globs —
so a key that can read issues cannot post to billing. This only makes sense
once the reverse proxy exists; a CONNECT tunnel never sees plaintext, which is
also why pisbx's hostname allowlist is the honest limit today.

When §1 lands, add per-route rules to the proxy, defaulting to deny when a
route defines any rule. Do not attempt L7 filtering on general egress: that
requires TLS interception, a CA in the sandbox, and a much larger claim than
pisbx should make. Hostname filtering stays the guarantee for non-credential
traffic.

---

## Deliberately not stealing

| Feature | Why not |
|---|---|
| Profile inheritance, groups, packs, registry | pisbx's value is one file you can audit. `extends` and group algebra is how "configure it better" becomes a second project. |
| Per-command tool sandboxes (`command_policies`) | Fundamentally different architecture (shims, exec mediation, brokering). srt cannot express it, and pi's tools are in-process anyway. |
| Audit, Merkle snapshots, rollback | Valuable but a product. Use `--no-session` and git; document it. |
| Detached PTY, attach, `nono ps`, webhooks, remote supervisor, Kubernetes | Out of scope. |
| Linux/Windows backend | Only if pisbx stops being macOS-only; then evaluate nono's library rather than reimplementing. |
| Sigstore attestation of skills/packs | No packs to attest. |
| Dangerous-command blocking | nono itself deprecates it as unenforceable for children. Filesystem and network policy are the boundary. |
| DNS-rebinding / metadata denials | srt already has `resolved-address-guard`: loopback, link-local, multicast, cloud metadata, and `deniedResolvedAddresses` are built in. Expose or document it; do not rebuild. |
| Proxy session auth | srt's proxy already carries per-session credentials. |

## Open questions

1. Can the pi extension keep provider auth intact while overriding only
   `baseUrl`, for both API-key and OAuth providers? Test with `anthropic` and
   `openai` first.
2. Does pi write `auth.json` during a normal session (OAuth refresh)? If yes,
   denying writes there has a cost we must document, not hide.
3. Does `network.allowLocalBinding` grant only localhost egress, or localhost
   bind as well? If it is too broad, request a narrower srt option before
   building the credential proxy.
4. Should `pisbx why` outside a sandbox shell out to the kernel via a
   throwaway srt invocation, or stay honestly static? Start static, add
   `--probe` for the in-sandbox case.

## Sources

- nono README and CLI docs: <https://github.com/nolabs-ai/nono>
- Networking, credential injection, tool sandbox: `docs/cli/features/*.mdx`
- Built-in deny groups: `crates/nono-cli/data/policy.json`
- Seatbelt model: `docs/cli/internals/seatbelt.mdx`
- pi pack (profile, extension, skill): <https://github.com/nolabs-ai/nono-packs/tree/main/pi>
- pi agent-dir resources: `docs/configuration.md` in the pi install
- pi provider override: `docs/custom-provider.md` in the pi install
- srt resolved-address guard:
  `@anthropic-ai/sandbox-runtime/dist/sandbox/resolved-address-guard.js`
