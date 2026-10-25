/**
 * pisbx — make the sandbox visible to pi.
 *
 * A no-op unless `PISBX_ACTIVE=1`, which pisbx sets for the sandboxed process.
 * It does three things, all of them cheap:
 *
 *   1. a lock on the footer's first line, so the session is never mistaken
 *      for an unsandboxed one, without adding a line;
 *   2. a paragraph in the system prompt saying the boundary is enforced by the
 *      kernel, not by approvals, and what to do about `EPERM`;
 *   3. `/pisbx`, which prints what the policy actually grants.
 *
 * Install with `pi install <path to this directory>` (or copy it into
 * `~/.pi/agent/extensions/`). pisbx never installs it for you: the directory it
 * would write into is deny-write by design, and an extension runs in-process
 * with the session's full capabilities.
 */

import { readFileSync } from "node:fs";
import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";
import { sandboxFooter } from "./footer.ts";

const ACTIVE = process.env.PISBX_ACTIVE === "1";
const SETTINGS_PATH = process.env.PISBX_SETTINGS;

/** `nf-seti-lock`, one cell wide. See `footer.ts` for why not the 🔒 emoji. */
const DEFAULT_LOCK = " \uE672";

/**
 * The marker on the footer's first line. `PISBX_LOCK` swaps it, and `\uXXXX`,
 * `\u{XXXXX}` and `\xXX` escapes are expanded, so a glyph can be tried from a
 * shell without typing a private-use codepoint. An empty value drops it.
 */
function lock(): string {
	const raw = process.env.PISBX_LOCK;
	if (raw === undefined) return DEFAULT_LOCK;
	return raw.replace(
		/\\u\{([0-9a-f]+)\}|\\u([0-9a-f]{4})|\\x([0-9a-f]{2})/gi,
		(_match, braced: string | undefined, four: string | undefined, two: string | undefined) =>
			String.fromCodePoint(Number.parseInt(braced ?? four ?? two!, 16)),
	);
}

type Settings = {
	filesystem?: {
		allowRead?: string[];
		allowWrite?: string[];
		denyRead?: string[];
		denyWrite?: string[];
		allowGitConfig?: boolean;
	};
	network?: {
		allowedDomains?: string[];
		allowUnixSockets?: string[];
		allowMachLookup?: string[];
	};
};

function settings(): Settings | undefined {
	if (!SETTINGS_PATH) return undefined;
	try {
		return JSON.parse(readFileSync(SETTINGS_PATH, "utf8")) as Settings;
	} catch {
		return undefined;
	}
}

function summary(): string {
	const s = settings();
	if (!s) return "pisbx: policy unavailable (PISBX_SETTINGS unreadable)";

	const fs = s.filesystem ?? {};
	const net = s.network ?? {};
	const count = (list?: unknown[]) => list?.length ?? 0;

	return [
		`writes     ${count(fs.allowWrite)} granted, ${count(fs.denyWrite)} denied`,
		`reads      ${count(fs.allowRead)} carve-outs under a closed $HOME`,
		`network    ${count(net.allowedDomains)} hosts: ${(net.allowedDomains ?? []).join(", ") || "none"}`,
		`sockets    ${count(net.allowUnixSockets)}`,
		`clipboard  ${count(net.allowMachLookup) > 0 ? "on" : "off"}`,
		`git config ${fs.allowGitConfig ? "writable" : "read-only"}`,
		"",
		"Grant more with `pisbx --allow-write <path>` for one run, or add it to",
		"~/.config/pisbx/config.toml. `pisbx why --path <p> --op read|write` explains",
		"any path from outside the sandbox.",
	].join("\n");
}

const PROMPT = `

## Sandbox (pisbx)

This session runs inside pisbx: pi is wrapped in a macOS Seatbelt sandbox that
the kernel enforces. It is not a set of approval prompts and cannot be widened
from inside.

- Writes are limited to the workspace, pi's state directory, temp space and
  whatever the policy grants. \`$HOME\` is closed for reads except carve-outs.
- Network is limited to an allowlist enforced by a proxy; any other host fails
  with a proxy error rather than a connection timeout.
- \`sudo\`, \`chmod\`, retries and asking for approval do not change any of this.

When a command fails with \`Operation not permitted\` / \`EPERM\`: stop and say
which path failed and what you were trying to do. Do not retry it, do not try
another spelling of the same path, and do not work around it with a different
tool. The human can grant the path for one run with \`pisbx --allow-write <path>\`,
or permanently in \`~/.config/pisbx/config.toml\`.

Likewise a blocked host is policy, not an outage: report the host, and let the
human add it to \`network.allowedDomains\` or pass \`--domains\` for the run.
`;

export default function (pi: ExtensionAPI) {
	if (!ACTIVE) return;

	// The lock rides on the footer's first line rather than taking its own:
	// setStatus() always renders a separate line. No fallback on purpose — if
	// this throws, pi's footer is the thing that needs fixing, and a silent
	// switch to a status line would hide that.
	pi.on("session_start", async (_event, ctx) => {
		if (!ctx.hasUI) return;
		ctx.ui.setFooter((_tui, theme, footerData) =>
			sandboxFooter(ctx, theme, footerData, { suffix: lock() }),
		);
	});

	pi.on("before_agent_start", async (event) => {
		return { systemPrompt: event.systemPrompt + PROMPT };
	});

	// Turn a bare EPERM into something the model can act on. pisbx's own
	// `--selftest` covers the policy; this covers the conversation.
	pi.on("tool_result", async (event) => {
		if (!event.isError) return undefined;

		let text = "";
		for (const block of event.content) {
			if (block.type === "text") text += `${block.text}\n`;
		}

		const denied = /Operation not permitted|EPERM\b/.test(text);
		if (!denied) return undefined;
		if (text.includes("pisbx: this path is outside the sandbox")) return undefined;

		return {
			content: [
				...event.content,
				{
					type: "text" as const,
					text: [
						"pisbx: this path is outside the sandbox.",
						"Do not retry or work around it — report the path to the human.",
						"They can grant it with `pisbx --allow-write <path>` for one run,",
						"or in ~/.config/pisbx/config.toml.",
					].join(" "),
				},
			],
		};
	});

	pi.registerCommand("pisbx", {
		description: "Show what the sandbox currently grants",
		handler: async (_args, ctx) => {
			ctx.ui.notify(summary(), "info");
		},
	});
}
