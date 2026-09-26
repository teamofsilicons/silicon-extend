// Copies the informative documentation into the website at build time:
//   understanding/cli.yaml     → the CLI reference (commands, device commands, errors, exit codes)
//   understanding/TECHNICAL.md → "How Extend works", rendered to HTML
// Output: src/generated/docs.json. If the understanding/ folder isn't there (a build that only
// has web/), the last generated file is kept.
import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { parse } from "yaml";
import { Marked } from "marked";

const here = dirname(fileURLToPath(import.meta.url));
const root = join(here, "..", "..", "understanding");
const out = join(here, "..", "src", "generated", "docs.json");

if (!existsSync(join(root, "cli.yaml")) || !existsSync(join(root, "TECHNICAL.md"))) {
  if (existsSync(out)) {
    console.log(`gen-docs: ${root} not found; keeping ${out}`);
    process.exit(0);
  }
  console.error(`gen-docs: ${root}/cli.yaml and TECHNICAL.md are needed to build the docs pages`);
  process.exit(1);
}

const cli = parse(readFileSync(join(root, "cli.yaml"), "utf8"));

// Section headers in cli.yaml are comments ("# ───── Devices ─────"); recover them so the
// reference keeps the file's grouping.
const lines = readFileSync(join(root, "cli.yaml"), "utf8").split("\n");
const groups = [];
let group = null;
let inCommands = false;
let inDeviceCommands = false;
for (const line of lines) {
  if (/^commands:/.test(line)) { inCommands = true; inDeviceCommands = false; continue; }
  if (/^device_commands:/.test(line)) { inDeviceCommands = true; inCommands = false; continue; }
  if (/^[a-z_]+:/.test(line)) { inCommands = false; inDeviceCommands = false; }
  const header = /^\s*#\s*─+\s*(.+?)\s*─+\s*$/.exec(line);
  if (header && inCommands) { group = { title: header[1], usages: [] }; groups.push(group); continue; }
  const deviceHeader = /^\s{2}#\s+(.+)$/.exec(line);
  if (deviceHeader && inDeviceCommands) { group = { title: deviceHeader[1], device: true, usages: [] }; groups.push(group); continue; }
  const usage = /^\s*- usage: (.+)$/.exec(line);
  if (usage && group) group.usages.push(usage[1].replace(/^'|'$/g, ""));
}

const normalize = (value) => (typeof value === "string" ? value : value == null ? null : JSON.parse(JSON.stringify(value)));
const commands = (list, device) =>
  (list ?? []).map((c) => ({
    usage: String(c.usage),
    who: c.who ?? null,
    needs: c.needs ?? null,
    capability: c.capability ?? null,
    summary: c.summary ?? null,
    takes: normalize(c.takes),
    gives: normalize(c.gives),
    errors: c.errors ?? null,
    api: c.api ?? null,
    notes: c.notes ?? null,
    device,
  }));

const allCommands = [...commands(cli.commands, false), ...commands(cli.device_commands, true)];
const grouped = groups.map((g) => ({
  title: g.title,
  device: !!g.device,
  commands: g.usages.map((u) => allCommands.find((c) => c.usage === u || c.usage === u.replace(/''/g, "'"))).filter(Boolean),
})).filter((g) => g.commands.length);

// TECHNICAL.md: drop the contract banner meant for agents, and the open questions (drafting notes).
let technical = readFileSync(join(root, "TECHNICAL.md"), "utf8");
technical = technical
  .replace(/^> Contract file\..*\n(> .*\n)*/m, "")
  .replace(/\n## Open questions[\s\S]*$/, "\n")
  .replace(/IAM names this an organization; Extend keeps IAM's wire names\./g, "Extend keeps IAM's wire names.")
  .replace(/\[Open questions\]\(#open-questions\)/g, "the open questions kept with the contract")
  .replace(/\*\*See Open questions 1–3:\*\*/g, "**Open questions:**");

const slug = (text) => text.toLowerCase().replace(/<[^>]+>/g, "").replace(/[^a-z0-9]+/g, "-").replace(/^-|-$/g, "");
const toc = [];
const marked = new Marked({
  gfm: true,
  renderer: {
    heading({ tokens, depth }) {
      const text = this.parser.parseInline(tokens);
      const id = slug(text);
      if (depth === 2 || depth === 3) toc.push({ id, text: text.replace(/<[^>]+>/g, ""), depth });
      return `<h${depth} id="${id}">${text}</h${depth}>\n`;
    },
    table(token) {
      return `<div class="table-scroll">${marked.Renderer.prototype.table.call(this, token)}</div>`;
    },
  },
});
const technicalHtml = marked.parse(technical);

mkdirSync(dirname(out), { recursive: true });
writeFileSync(
  out,
  JSON.stringify(
    {
      generated_from: ["understanding/cli.yaml", "understanding/TECHNICAL.md"],
      cli: {
        name: cli.name,
        summary: cli.summary,
        install: cli.install,
        links: cli.links,
        grammar: cli.grammar,
        global_flags: cli.global_flags,
        environment: cli.environment,
        state_files: cli.state_files,
        exit_codes: cli.exit_codes,
        errors: Object.entries(cli.errors ?? {}).map(([code, [exit, meaning]]) => ({ code, exit, meaning })),
        groups: grouped,
        not_exposed: cli.not_exposed,
        examples: cli.examples,
      },
      technical: { html: technicalHtml, toc },
    },
    null,
    1,
  ),
);
console.log(`gen-docs: wrote ${out} (${allCommands.length} commands, ${toc.length} sections)`);
