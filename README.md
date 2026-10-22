# pisbx

Run the [pi](https://github.com/earendil-works/pi) coding agent inside a macOS
Seatbelt sandbox: one directory writable, home closed, no network unless you
name the hosts.

```bash
pisbx --init                   # once: write a starter policy from this machine
pisbx                          # sandbox pi against the current directory
pisbx --selftest               # prove the boundary holds on this machine
```

pisbx is a policy layer. [srt](https://github.com/anthropics/sandbox-runtime)
does the enforcement — it generates the Seatbelt profile and runs a localhost
proxy that decides which domains may be reached. pisbx decides what to ask for,
and knows the handful of things about pi that a generic wrapper cannot.

## Why this exists

The pieces that make pi work under Seatbelt are not obvious, and each fails in a
confusing way when missing:

| Fact | Symptom when unhandled |
|---|---|
| `~/.pi` is usually a symlink into a checkout, and Seatbelt matches resolved paths | `EPERM: mkdir ~/.pi/agent/settings.json.lock`, then "No API key found for the selected model" |
| The TUI calls `tcsetattr`/`setRawMode` | `Error: setRawMode EPERM` after the screen has already drawn |
| The pasteboard is a Mach service | pi reports "Clipboard unavailable" |
| `/tmp` is a symlink to `/private/tmp` | a grant on `/tmp` grants nothing |
| Seatbelt cannot filter by hostname | you must allow all egress or none, unless a proxy does the filtering |
| srt merges its own write conveniences into every profile | a policy that claims "home is closed" is quietly wrong |

## Defaults

Nothing is allowed that pi does not need to start.

| Surface | Default |
|---|---|
| Writes | the workspace, `~/.pi/agent`, `/private/tmp`, `/private/var/folders` |
| Always denied writes | `~/.pi/agent/{extensions,install,settings.json,keybindings.json,models-store.json,npm,web-search-cache}`, `~/.npm/_logs`, `~/.claude/debug`, the pisbx binary, the pisbx config |
| Reads | **home is closed**; open: the workspace, `~/.pi`, pi's runtime, and the directories on `$PATH` that live under `$HOME` |
| Network | none |
| Clipboard | off |
| Apple Events | off — allowing them is an unsandboxed code-execution path |
| Project resources | refused (`--no-approve`), so a cloned repo cannot load extensions before you look |

Denying writes to `extensions/` and `install/` is the highest-value line in the
file: either one writable means a sandboxed agent can arm the next session with
code it chose. The floor exists to let a session run, not to let it persist.

Two of srt's switches are exposed because everyday work needs them and both are
off by default: `allow-git-config` (write `.git/config`, so `git remote add` and
submodules work — `.git/hooks` stay blocked either way) and `allow-unix-sockets`
(talk to a specific socket, such as an SSH agent used for commit signing).

## First run

`--init` reads the machine — `$DOTFILES`, caches, the directories on `$PATH`,
the model provider in `~/.pi` — and writes one commented config file. It is the
only time pisbx inspects your environment. From then on the file is the policy,
and a run without it exits `78`:

```bash
$ pisbx
pisbx: /Users/you/.config/pisbx/config.toml does not exist. Run `pisbx --init` to write a starter policy.
```

The scaffold is a starting point, not an oracle: paths it cannot infer are
mentioned in comments, and the clipboard block ships commented out because
reading the pasteboard lets the agent see whatever you copied.

## Policy file

One file, `~/.config/pisbx/config.toml`, written in **srt's own schema** — so
there is no translation layer, and every option srt grows works here without a
pisbx release. It is read from `$HOME`, never from the workspace: a cloned
repository must not be able to widen its own sandbox, and the file is
`deny-write` inside the sandbox so the agent cannot widen it either.

```toml
[filesystem]
allowRead = ["~/Projects/dotfiles", "~/.cache", "~/.ssh/known_hosts"]
allowWrite = ["~/scratch"]
denyWrite = ["~/scratch/.git"]
allowGitConfig = true            # .git/config, not .git/hooks

[network]
allowedDomains = ["api.openai.com", "github.com", "*.github.com"]
allowUnixSockets = ["~/Library/Group Containers/<TEAM>.<app>/t/agent.sock"]
allowMachLookup = ["com.apple.pasteboard.1", "com.apple.lsd.mapdb", "com.apple.lsd.modifydb"]
allowLocalBinding = true          # dev servers on localhost
deniedResolvedAddresses = ["10.0.0.0/8", "192.168.0.0/16"]

[network.deniedDomainReasons]
"github.com:22" = "SSH to GitHub goes through git; a bare ssh is not proxied."
```

What pisbx does on top of whatever the file says:

- **paths get interpreted**: tilde expansion, resolution to the symlink target,
  and target-relative resolution for the five path keys. Writing `~/x` therefore
  means the same as inside the sandbox, where Seatbelt sees the resolved path.
- **the floor is merged in, add-only**: pi's state directory, its runtime, the
  workspace, temp space, and the always-denied set (`extensions/`, `install/`,
  the policy file itself). No file entry can open those.
- **`--read-only`** drops `allowWrite` and `allowGitConfig` for the run.
- **precedence**: `allowedDomains` is replaced, never extended — flags beat the
  file, and the file beats `PISBX_DOMAINS` in the environment, so an inherited
  variable cannot quietly override policy you wrote.
- **unknown keys** inside `filesystem` are an error; anywhere else they are
  forwarded to srt and named in a warning, since srt would otherwise drop them
  silently.

## Per-project policy

There is no workspace config file, by design. Deltas are flags, in whatever
wrapper you already run:

```bash
pisbx --allow-write ~/.cargo --domains api.openai.com,registry.npmjs.org
```

Put that line in the repo's `justfile`, a shell function, or an alias.

## Commands

| Command | Purpose |
|---|---|
| `pisbx [options] [--] [pi args…]` | run pi under the sandbox |
| `pisbx --init [--force]` | write a starter policy; never clobbers without `--force` |
| `pisbx --explain` | the effective policy, with `built-in` / `config` / `flag` on every entry |
| `pisbx --print-config` | the srt settings document (JSON) |
| `pisbx --selftest` | run real commands through srt and assert what actually happens |
| `pisbx --read-only` | keep every write except pi's state directory |
| `pisbx --clipboard` | allow the pasteboard for this run |
| `pisbx --allow-git-config` | let this run write `.git/config` (hooks stay denied) |
| `pisbx --allow-unix-sockets <paths>` | let this run connect to those sockets |

Options are consumed only up to the first non-option argument or `--`;
everything after belongs to pi. Inspection modes work without a config file, so
you can read the floor before initialising.

`--selftest` must be run outside another sandbox: nested srt cannot bind its
unix-socket mux.

## Exit codes

| Code | Meaning |
|---|---|
| 0 | success (also: `--init` found an existing file and declined to clobber) |
| 1 | selftest failure |
| 64 | usage error |
| 65 | invalid config file |
| 69 | `srt` or `pi` missing |
| 78 | not initialised — run `pisbx --init` |
| *other* | srt's status, when it runs pi |

## Design

```
cli      → our flags; the rest of argv is pi's
floor    → what pi needs before any policy question: workspace, state, runtime
init     → one-time scaffold from the machine's own configuration
paths    → tilde, target-relative, and resolved spellings of every granted path
config   → TOML policy file, parsed with line-accurate errors
policy   → the resolved policy, rendered as srt JSON or as an explanation
srt      → locate the binary, run it, capture output
selftest → assertions about what the policy actually enforces
main     → resolve inputs into a Policy, then dispatch
```

| Crate | Role |
|---|---|
| `clap` | flag definitions and `--help`; the argv pre-split above it keeps pi's arguments out of clap's hands |
| `serde` + `serde_json` | the settings document and the pi config files `--init` reads |
| `toml` | the policy file |
| `anyhow` | error context; exit codes attach at the boundary |
| `tempfile` | the generated settings file, 0600, removed unless `--keep-temp` |

Every granted path carries its source, and `--explain` is generated from the
same struct that produces the settings. The acceptance test for the tool is that
the explanation matches what the kernel enforces.

## Development

```bash
just check                       # fmt-check + clippy -D warnings + tests
just build                       # release binary, build paths remapped
just install                     # cargo install --path .
just selftest                    # against the real srt, outside a sandbox
```

The unit tests cover argument splitting, path expansion, policy rendering
(round-tripped through `serde_json`), config parsing and the scaffold. The
end-to-end assertions live in `pisbx --selftest`, which needs a real kernel, so
they belong in CI rather than in `cargo test`.

## Limits

- Seatbelt is a same-kernel boundary: it does not defend against a kernel or
  Seatbelt bug, and says nothing about what runs outside pisbx.
- Reads are closed under `$HOME` but opened by every path in the policy. A
  secret inside a granted directory is readable; deny it explicitly.
- Session transcripts are writable, so they are a record, not evidence: an agent
  can rewrite its own history. Use pi's `--no-session` when audit matters.
- The network allowlist keys on the CONNECT host, not TLS contents.
- macOS only.

## Prior art

[zerobox](https://github.com/afshinm/zerobox) is the closest general-purpose
equivalent (Seatbelt plus proxy, plus secret injection and snapshots);
[agent-safehouse](https://github.com/eugene1g/agent-safehouse) is the most
developed Seatbelt policy system but deliberately does no egress filtering;
[yolobox](https://github.com/finbarr/yolobox) and
[VibeBox](https://github.com/robcholz/vibebox) take the container/VM route.
None of them know pi.

## License

MIT — see [LICENSE](LICENSE).
