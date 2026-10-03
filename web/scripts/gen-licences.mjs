// Writes public/licences.txt: Extend's own licence and the full licence text of every package the
// built website ships (package.json "dependencies"; build tools don't ship). Runs before each build.
import { existsSync, readFileSync, readdirSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const web = join(dirname(fileURLToPath(import.meta.url)), "..");
// A deploy that uploads only web/ has no ../LICENSE; the committed public/licences.txt ships as is.
if (!existsSync(join(web, "..", "LICENSE"))) {
  console.log("gen-licences: ../LICENSE is not here; keeping the committed public/licences.txt");
  process.exit(0);
}
const pkg = JSON.parse(readFileSync(join(web, "package.json"), "utf8"));
const rule = "-".repeat(80);
const parts = [
  "Silicon Extend configuration website: licences",
  "",
  "Silicon Extend is MIT licensed; its source is https://github.com/teamofsilicons/silicon-extend.",
  "Below: Extend's licence, then every third-party package this website ships, with its licence text.",
  "",
  rule,
  "Silicon Extend",
  rule,
  "",
  readFileSync(join(web, "..", "LICENSE"), "utf8").trim(),
];
for (const name of Object.keys(pkg.dependencies ?? {}).sort()) {
  const dir = join(web, "node_modules", name);
  const meta = JSON.parse(readFileSync(join(dir, "package.json"), "utf8"));
  const file = readdirSync(dir).find((f) => /^(licen[cs]e|copying)(\.(md|txt))?$/i.test(f));
  if (!file) throw new Error(`${name} has no licence file; add it to the notices by hand`);
  parts.push("", rule, `${name} ${meta.version} (${meta.license})`, rule, "", readFileSync(join(dir, file), "utf8").trim());
}
parts.push("", rule, "UIArc free components (MIT)", rule, "", readFileSync(join(web, "public", "licenses", "UIArc.txt"), "utf8").trim());
writeFileSync(join(web, "public", "licences.txt"), parts.join("\n") + "\n");
