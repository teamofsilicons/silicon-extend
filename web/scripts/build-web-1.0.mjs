// Builds the released 1.0.0 website (git tag v1.0.0, only its web/ folder) into
// test-results/web-1.0/web/dist, so e2e-compat can run the 1.0 bundle against the 1.1 mock service:
// a 1.1 service must keep the 1.0 website working (API v1 stays additive). The dependencies are
// the same as today's, so the current node_modules is linked in instead of installed again.
import { execFileSync } from "node:child_process";
import { existsSync, mkdirSync, rmSync, symlinkSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const web = join(dirname(fileURLToPath(import.meta.url)), "..");
const tag = process.env.WEB_1_0_TAG || "v1.0.0";
const root = join(web, "test-results", "web-1.0");
const old = join(root, "web");

rmSync(root, { recursive: true, force: true });
mkdirSync(root, { recursive: true });
const archive = execFileSync("git", ["archive", "--format=tar", tag, "web"], { cwd: join(web, ".."), maxBuffer: 1 << 28 });
execFileSync("tar", ["-x", "-C", root], { input: archive });
if (!existsSync(join(old, "node_modules"))) symlinkSync(join(web, "node_modules"), join(old, "node_modules"), "dir");
// 1.0's own build runs gen-docs against ../understanding, which isn't in the archive: its committed
// docs.json is used as it is, and only vite builds.
// Same origin, so the bundle talks only to the local mock through vite preview's proxy: a
// production build would otherwise call the production service.
execFileSync(join(web, "node_modules", ".bin", "vite"), ["build", "--logLevel", "warn"], {
  cwd: old,
  stdio: "inherit",
  env: { ...process.env, VITE_EXTEND_API_URL: "same-origin", VITE_IAM_LOGIN_URL: "" },
});
console.log(`build-web-1.0: built the ${tag} website into ${join(old, "dist")}`);
