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
does the enforcement: it generates the Seatbelt profile and runs a localhost
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
| Always denied writes | `~/.pi/agent/{bin,extensions,git,install,mcp.json,skills,SYSTEM.md,APPEND_SYSTEM.md,AGENTS.md,AGENTS.override.md,CLAUDE.md,prompts,settings.json,keybindings.json,models.json,models-store.json,npm,themes,trust.json,web-search-cache}`, `~/.npm/_logs`, `~/.claude/debug`, the pisbx binary, the pisbx config |
| Always denied writes in the workspace | `.pi`, `.agents`, `AGENTS.md`, `AGENTS.MD`, `AGENTS.override.md`, `CLAUDE.md`, `CLAUDE.MD` |
| Reads | **home is closed**; open: the workspace, `~/.pi`, pi's runtime, and the directories on `$PATH` that live under `$HOME` |
| Network | none |
| Clipboard | off |
| Apple Events | off; allowing them is an unsandboxed code-execution path |
| Project resources | refused (`--no-approve`), so a cloned repo cannot load extensions before you look |

The most important denies are `extensions/`, `install/`, `mcp.json` and
`settings.json`: any of them writable means a sandboxed agent can arm the next
session with code it chose. `bin/` holds pi's launcher and the `fd`/`rg` binaries
it puts on PATH, and `git/` holds git-installed packages; either one writable is
the same next-session code execution. A write to `SYSTEM.md`,
`APPEND_SYSTEM.md`, `AGENTS.md` or `skills/` is durable prompt injection. The
floor lets a session run; it does not let it persist.

The same reasoning applies to project configuration in the workspace. `.pi`
holds settings, MCP servers, system prompts, extensions, skills, prompts and
themes; `.agents/skills` holds project skills; and the context files
(`AGENTS.md`, `AGENTS.override.md`, `CLAUDE.md`) load without project trust.
All are readable, so pi still uses them as project-level overrides, but they
are deny-write: edit them on the host, not through the sandboxed agent. This
also means `pi install --local`, `pi config --local` and `pi mcp add --local`
cannot run inside the sandbox.

Because `bin/` is denied, pi cannot auto-download `fd`/`rg` inside the
sandbox. Install them on the host, or make sure they are already on `PATH`.

`~/.pi/agent/auth.json` is the deliberate exception: pi rewrites it when OAuth
tokens refresh, so it stays writable, and readable, inside the sandbox. A
prompt-injected agent can therefore read the model credential. The domain
allowlist bounds where it can be sent, and closing it properly needs credential
masking.

Two of srt's switches are exposed because everyday work needs them and both are
off by default: `allow-git-config` (write `.git/config`, so `git remote add` and
submodules work; `.git/hooks` stay blocked either way) and `allow-unix-sockets`
(talk to a specific socket, such as an SSH agent used for commit signing).

## First run

`--init` reads the machine (`$DOTFILES`, caches, the directories on `$PATH`,
the model provider in `~/.pi`) and writes one commented config file. It is the
only time pisbx inspects your environment. From then on the file is the policy,
and a run without it exits `78`:

```bash
$ pisbx
pisbx: /Users/you/.config/pisbx/config.toml does not exist. Run `pisbx --init` to write a starter policy.
```

The scaffold is a starting point. Paths it cannot infer are mentioned in
comments, and the clipboard block ships commented out because reading the
pasteboard lets the agent see whatever you copied.

## Policy file

One file, `~/.config/pisbx/config.toml`, written in **srt's own schema** plus
two pisbx-owned tables (`[pisbx]` and `[projects]`), so there is no translation
layer for the srt keys and every option srt grows works here without a pisbx
release. It is read from `$HOME`, never from the workspace: a cloned
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

- Paths are interpreted: tilde expansion, resolution to the symlink target,
  and target-relative resolution for the five path keys. Writing `~/x` therefore
  means the same as inside the sandbox, where Seatbelt sees the resolved path.
  Resolution is computed for paths that do not exist yet too, by resolving the
  deepest ancestor that does. A deny that only worked once the file had been
  created would be no deny at all.
- The floor is merged in, add-only: pi's state directory, its runtime, the
  workspace, temp space, and the always-denied set (`extensions/`, `install/`,
  the policy file itself). No file entry can open those.
- `--read-only` drops `allowWrite` and `allowGitConfig` for the run.
- Precedence: explicit domain sources accumulate. `allowedDomains` from the
  file, the matching `[projects]` section, and `--domains` are unioned;
  `PISBX_DOMAINS` applies only when none of them names a domain.
  `deniedDomains` and `--deny-domains` also accumulate, and a denied domain
  wins.
- Unknown keys inside `filesystem` are an error; anywhere else they are
  forwarded to srt and named in a warning, since srt would otherwise drop them
  silently. Inside `[projects]`, unknown keys are an error instead.

### pisbx's own settings

A `[pisbx]` table holds the few decisions that are pisbx's rather than srt's.
It is stripped before the settings document is built, so nothing here can be
mistaken for an srt option, and unknown keys are an error.

```toml
[pisbx]
trust = true                 # load project .pi extensions without asking
unset = ["GITHUB_TOKEN"]     # strip what the sandbox should not inherit

[pisbx.env]                  # environment for the sandboxed process
SSH_AUTH_SOCK = "~/Library/Group Containers/<TEAM>.<app>/t/agent.sock"
```

`[pisbx.env]` is what makes commit signing work without prefixing every command:
srt cannot set environment for you, and the sandbox's own `SSH_AUTH_SOCK` is a
launchd listener with no identities. `unset` is the other direction: pi
inherits your shell, so a token exported there would otherwise be readable
inside the sandbox.

`--explain` lists names and never values, and a name that appears in both
`env` and `unset` is a config error:

```
environment for the sandboxed process
  SSH_AUTH_SOCK  set (config)
  GITHUB_TOKEN   unset (config)
```

### Credentials that stay outside the sandbox

pi needs a model key, and anything pi can read a prompt-injected agent can read.
A credential route keeps the real secret in this process and gives the sandbox a
phantom instead:

```toml
[network]
allowLocalBinding = true          # required: the sandbox must reach 127.0.0.1

[pisbx]
credential-proxy-port = 8712      # fixed, so a static models.json can name it

[pisbx.credentials.openai]
upstream = "https://api.openai.com"
secret = "op://Private/OpenAI/api-key"   # or env:NAME, or file:PATH
env = "OPENAI_API_KEY"                   # pi finds the phantom here
```

At launch pisbx resolves the secret with `op read`, starts a proxy on
`127.0.0.1:8712`, exports `PISBX_CREDPROXY_OPENAI=http://127.0.0.1:8712/openai`
and puts the phantom in `OPENAI_API_KEY`. Point the provider's `baseUrl` at that
route in `models.json`; every request carries the phantom, the proxy swaps it for
the real key on the way out, and a request without the phantom gets `403`
instead of being forwarded.

This version supports API-key providers only. OAuth providers (pi's
`openai-codex`, for one) keep refresh tokens in `auth.json` and are not proxied.
Requests need a `Content-Length` body; chunked uploads are refused. The proxy
needs `curl` on `PATH` (present on every macOS install), and `allowLocalBinding`
must be on; pisbx enforces that.

## Per-project policy

Deltas live in the same host-owned file, keyed by the project directory. They
are not in the repo, so a cloned project cannot widen its own sandbox:

```toml
[projects."~/Projects/foo".filesystem]
allowWrite = ["~/.cargo", "target"]
denyWrite = [".env"]

[projects."~/Projects/foo".network]
allowedDomains = ["registry.npmjs.org", "static.crates.io"]
deniedDomains = ["*.internal.example"]
```

The key must be absolute, `~`, or start with `~/`, and it is resolved the way Seatbelt
resolves a path: symlinks and case included. It must name the directory pisbx
runs in exactly; there is no ancestor inheritance and no globbing, and two keys
that resolve to the same directory are a config error. `--explain` prints the
section that matched, or `none (N configured)`.

A project section can set the filesystem lists, `allowGitConfig`, and the
network lists (`allowedDomains`, `deniedDomains`, `allowUnixSockets`,
`allowMachLookup`). It cannot set `[pisbx]` settings or change scalar srt
switches, and unknown keys are an error rather than being forwarded.

Lists add to the global policy, and denies always win, so a project can narrow a
global grant by denying a path or domain. The built-in denies on project agent
configuration (`.pi`, `.agents`, `AGENTS.md`, and the rest of
`ALWAYS_DENY_WRITE_PROJECT`) still win, so a project section cannot reopen
writing to its own instructions. Explicit domain sources — the global file, the
matching project section, and `--domains` — accumulate; `PISBX_DOMAINS` is used
only when none of them names a domain. `--deny-domains` subtracts for a run.
`--read-only` still drops every write.

Because the section is host-authored, it can grant more than the global policy.
The repo cannot provide or edit it, and the sandboxed agent cannot widen it.
Renaming or moving a project means updating its key.

One-off deltas are still flags:

```bash
pisbx --allow-write ~/.cargo --domains registry.npmjs.org
```

Put that line in a shell function or an alias. Do not put a widening flag in the
repo's own `justfile`: the sandboxed agent can edit the justfile, and you would
be the one running it.

## Commands

| Command | Purpose |
|---|---|
| `pisbx [options] [--] [pi args…]` | run pi under the sandbox |
| `pisbx --init [--force]` | write a starter policy; never clobbers without `--force` |
| `pisbx --explain` | the effective policy, with `built-in` / `config` / `project` / `flag` on every entry |
| `pisbx --print-config` | the srt settings document (JSON) |
| `pisbx --selftest` | run real commands through srt and assert what actually happens |
| `pisbx --read-only` | keep every write except pi's state directory |
| `pisbx --domains <list>` | add explicit allowlist domains for this run |
| `pisbx --deny-domains <list>` | block those domains for this run (wins over allowed) |
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
| 78 | not initialised; run `pisbx --init` |
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
- An agent can rewrite session transcripts. Use pi's `--no-session` when audit
  matters.
- The network allowlist keys on the CONNECT host, not TLS contents.
- macOS only.

## Sources for Seatbelt rules

SBPL is sparsely documented, and the useful rules are the ones someone already
debugged against a real toolchain. These are worth reading before adding a rule
or a path:

- [srt's macOS profile generator](https://github.com/anthropics/sandbox-runtime/blob/main/src/sandbox/macos-sandbox-utils.ts):
  the code that builds the profile pisbx runs. It covers read/deny ordering,
  symlink resolution, `.git/hooks` denies, and the startup Mach/sysctl set.
- [agent-safehouse's profiles](https://github.com/eugene1g/agent-safehouse/tree/main/profiles):
  a large, maintained collection. `00-base.sb` and `10-system-runtime.sb` cover
  process, Mach and sysctl plumbing; `30-toolchains/` has mise, uv, Node/pnpm,
  Rust and Python paths; `60-agents/pi.sb` covers pi's own state.
- [OpenAI Codex's Seatbelt backend](https://github.com/openai/codex/blob/main/codex-rs/sandboxing/src/seatbelt.rs)
  and its [`seatbelt_base_policy.sbpl`](https://github.com/openai/codex/blob/main/codex-rs/sandboxing/src/seatbelt_base_policy.sbpl):
  a deny-default read/write split with a narrow sysctl/Mach allowlist.
- [Gemini CLI's built-in profiles](https://github.com/google-gemini/gemini-cli/blob/main/packages/cli/src/utils/sandboxBuiltinProfiles.ts):
  permissive and restrictive variants, with Docker and credential denies and
  the macOS service allowlist.
- [agent-seatbelt](https://github.com/CJHwong/agent-seatbelt): a small two-file
  profile focused on secret read-denies (SSH, AWS, browser profiles, shell
  history) and write-denies on shell init files and `.git/hooks`.
- [PostHog's `dev-sandbox.sb`](https://github.com/PostHog/posthog/blob/master/bin/dev-sandbox.sb):
  a dev-stack profile that denies home reads and re-opens the repo plus
  toolchain caches.
- [Chromium's macOS sandbox policies](https://source.chromium.org/chromium/chromium/src/+/main:sandbox/policy/mac/common.sb):
  a primary source for many base rules, though larger than a coding-agent
  policy needs.
- [Harn's sandbox config census](https://harnlang.com/sandbox-config-census.html):
  measured toolchain config and cache paths per command. It lists paths, not
  SBPL rules, but it is useful when a new tool hits `EPERM`.
- [Apple's Sandbox Guide](https://reverse.put.as/wp-content/uploads/2011/09/Apple-Sandbox-Guide-v1.0.pdf)
  and `man sandbox-exec`: the syntax and operation names the others rely on.

Seatbelt is deny-by-default, so a profile is mostly a list of things that were
allowed because a real command failed without them. Copy the structure and the
reasoning; a whole profile rarely fits another toolchain.

## Prior art

[zerobox](https://github.com/afshinm/zerobox) is the closest general-purpose
equivalent (Seatbelt plus proxy, plus secret injection and snapshots).
[agent-safehouse](https://github.com/eugene1g/agent-safehouse) is a larger,
maintained Seatbelt policy system, but it does no egress filtering.
[yolobox](https://github.com/finbarr/yolobox) and
[VibeBox](https://github.com/robcholz/vibebox) take the container/VM route.
None of them know pi.

## License

MIT; see [LICENSE](LICENSE).
