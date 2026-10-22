/**
 * A footer that is pi's default, with the sandbox lock appended to the first
 * line instead of taking a line of its own.
 *
 * pi's `ctx.ui.setStatus()` renders every status on a separate line, and the
 * default `FooterComponent` needs the `AgentSession`, which an extension
 * context does not expose. So the two lines are reproduced here from what the
 * context does expose, with the same formatting rules as pi's footer.
 *
 * Mirrors pi 1.4.0 `modes/interactive/components/footer.js`. Differences are
 * cosmetic and deliberate: no subscription marker, no routed-model arrow, no
 * experimental-features badge. If pi's footer gains something worth having,
 * this is the file to update.
 */

import {
	truncateToWidth,
	visibleWidth,
	type Component,
	type ReadonlyFooterDataProvider,
	type Theme,
} from "@earendil-works/pi-tui";
import type { ExtensionContext } from "@earendil-works/pi-coding-agent";

const LOCK = " 🔒";

type Usage = {
	input: number;
	output: number;
	cacheRead: number;
	cacheWrite: number;
	cost: { total: number };
};

/** pi's `formatTokens`, verbatim. */
function formatTokens(count: number): string {
	if (count < 1000) return count.toString();
	if (count < 10000) return `${(count / 1000).toFixed(1)}k`;
	if (count < 1000000) return `${Math.round(count / 1000)}k`;
	if (count < 10000000) return `${(count / 1000000).toFixed(1)}M`;
	return `${Math.round(count / 1000000)}M`;
}

/** pi's `formatCwdForFooter`. */
function formatCwd(cwd: string, home: string | undefined): string {
	if (!home) return cwd;
	if (cwd === home) return "~";
	return cwd.startsWith(`${home}/`) ? `~${cwd.slice(home.length)}` : cwd;
}

function totals(ctx: ExtensionContext): { usage: Usage; cacheHitRate?: number } {
	const usage: Usage = {
		input: 0,
		output: 0,
		cacheRead: 0,
		cacheWrite: 0,
		cost: { total: 0 },
	};
	let cacheHitRate: number | undefined;

	for (const entry of ctx.sessionManager.getEntries()) {
		const message = entry.type === "message" ? entry.message : undefined;
		const reported =
			entry.type === "usage"
				? entry.usage
				: message?.role === "assistant"
					? message.usage
					: message?.role === "toolResult" && message.usage
						? message.usage
						: (entry.type === "branch_summary" || entry.type === "compaction") && entry.usage
							? entry.usage
							: undefined;
		if (!reported) continue;

		usage.input += reported.input;
		usage.output += reported.output;
		usage.cacheRead += reported.cacheRead;
		usage.cacheWrite += reported.cacheWrite;
		usage.cost.total += reported.cost.total;

		if (message?.role === "assistant") {
			const promptTokens = reported.input + reported.cacheRead + reported.cacheWrite;
			cacheHitRate = promptTokens > 0 ? (reported.cacheRead / promptTokens) * 100 : undefined;
		}
	}

	return { usage, cacheHitRate };
}

export interface FooterOptions {
	/** Shown at the end of the first line, e.g. " 🔒". */
	suffix: string;
}

export function sandboxFooter(
	ctx: ExtensionContext,
	theme: Theme,
	footerData: ReadonlyFooterDataProvider,
	options: FooterOptions,
): Component & { dispose?(): void } {
	// The footer renders on every frame and the totals scan the whole session,
	// so keep the last result until an entry is appended (pi caches the same
	// way, keyed by leaf and entry count).
	let cached: { leaf: string | undefined; count: number; value: ReturnType<typeof totals> } | undefined;
	const stats = () => {
		const leaf = ctx.sessionManager.getLeafId?.();
		const count = ctx.sessionManager.getEntries().length;
		if (cached && cached.leaf === leaf && cached.count === count) return cached.value;
		const value = totals(ctx);
		cached = { leaf, count, value };
		return value;
	};

	return {
		render(width: number): string[] {
			const dim = (text: string) => theme.fg("dim", text);
			const suffix = dim(options.suffix);
			const suffixWidth = visibleWidth(suffix);
			const room = Math.max(0, width - suffixWidth);

			// Line 1: cwd, branch, session name, then the lock.
			let pwd = formatCwd(ctx.sessionManager.getCwd(), process.env.HOME);
			const branch = footerData.getGitBranch();
			if (branch) pwd = `${pwd} (${branch})`;
			const sessionName = ctx.sessionManager.getSessionName();
			if (sessionName) pwd = `${pwd} • ${sessionName}`;
			if (options.suffix) pwd = `${pwd}${suffix}`;
			const pwdLine = truncateToWidth(dim(pwd), width, dim("..."));
			// `truncateToWidth` may have eaten the lock when the path is long;
			// keep it visible by truncating the path first when it does.
			const firstLine =
				visibleWidth(pwdLine) > room && options.suffix
					? `${truncateToWidth(dim(pwd.slice(0, pwd.length - options.suffix.length)), room, dim("..."))}${suffix}`
					: pwdLine;

			// Line 2: token/cost/context stats, model right-aligned.
			const { usage, cacheHitRate } = stats();
			const parts: string[] = [];
			if (usage.input) parts.push(`↑${formatTokens(usage.input)}`);
			if (usage.output) parts.push(`↓${formatTokens(usage.output)}`);
			if (usage.cacheRead) parts.push(`R${formatTokens(usage.cacheRead)}`);
			if (usage.cacheWrite) parts.push(`W${formatTokens(usage.cacheWrite)}`);
			if ((usage.cacheRead > 0 || usage.cacheWrite > 0) && cacheHitRate !== undefined) {
				parts.push(`CH${cacheHitRate.toFixed(1)}%`);
			}
			parts.push(`$${usage.cost.total.toFixed(3)}`);

			const context = ctx.getContextUsage();
			const window = context?.contextWindow ?? ctx.model?.contextWindow ?? 0;
			const percent = context?.percent;
			const percentText = percent === null ? "?" : (percent ?? 0).toFixed(1);
			parts.push(`${percentText}%/${formatTokens(window)}`);

			let left = parts.join(" ");
			if (visibleWidth(left) > width) left = truncateToWidth(left, width, "...");

			let right = ctx.model?.id || "no-model";
			if (ctx.model?.reasoning) {
				const level = ctx.thinkingLevel || "off";
				right = level === "off" ? `${right} • thinking off` : `${right} • ${level}`;
			}
			if (footerData.getAvailableProviderCount() > 1 && ctx.model) {
				const withProvider = `(${ctx.model.provider}) ${right}`;
				if (visibleWidth(left) + 2 + visibleWidth(withProvider) <= width) right = withProvider;
			}

			const padding = Math.max(0, width - visibleWidth(left) - visibleWidth(right));
			const statsLine =
				visibleWidth(left) + 2 + visibleWidth(right) <= width
					? `${dim(left)}${" ".repeat(padding)}${dim(right)}`
					: dim(left);

			const lines = [firstLine, statsLine];

			// Other extensions' statuses keep pi's behaviour: their own line.
			const statuses = Array.from(footerData.getExtensionStatuses().entries())
				.sort(([a], [b]) => a.localeCompare(b))
				.map(([, text]) => text);
			if (statuses.length > 0) lines.push(truncateToWidth(statuses.join(" "), width, dim("...")));

			return lines;
		},
	};
}
