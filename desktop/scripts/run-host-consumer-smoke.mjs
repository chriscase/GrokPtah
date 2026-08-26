/**
 * Pack the staged `@grokptah/client` package and drive it from two external
 * consumers: a trusted host (Node conditions) and a browser/public consumer
 * (explicit `browser` and `worker` conditions).
 *
 * The trusted consumer must reach GrokPtah's powers only through the
 * `@grokptah/client/host` seam; the browser consumer must keep its unchanged
 * root and `./ui-core` surface while failing to resolve that seam at all.
 */
import { copyFile, mkdtemp, mkdir, readdir, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { spawn } from "node:child_process";

const packageSource = new URL("../dist/public/", import.meta.url);
const fixtures = new URL("./fixtures/", import.meta.url);
const workspace = await mkdtemp(join(tmpdir(), "grokptah-host-consumer-"));
const packDirectory = join(workspace, "pack");

function run(command, args, options = {}) {
  return new Promise((resolve, reject) => {
    const child = spawn(command, args, { ...options, stdio: ["ignore", "pipe", "pipe"] });
    let output = "";
    child.stdout.on("data", (chunk) => { output += chunk; });
    child.stderr.on("data", (chunk) => { output += chunk; });
    child.on("error", reject);
    child.on("close", (code) => {
      if (code === 0) resolve(output);
      else reject(new Error(`${command} ${args.join(" ")} exited ${code}: ${output}`));
    });
  });
}

try {
  await mkdir(packDirectory, { recursive: true });
  await writeFile(
    join(workspace, "package.json"),
    JSON.stringify(
      { name: "grokptah-host-consumer-fixture", private: true, type: "module" },
      null,
      2,
    ),
  );
  const npm = process.platform === "win32" ? "npm.cmd" : "npm";
  await run(npm, ["pack", "--ignore-scripts", "--pack-destination", packDirectory], {
    cwd: fileURLToPath(packageSource),
  });
  const packed = (await readdir(packDirectory)).find((name) => name.endsWith(".tgz"));
  if (!packed) throw new Error("npm pack did not produce a package archive");
  await run(npm, [
    "install",
    "--offline",
    "--ignore-scripts",
    "--no-package-lock",
    "--prefix",
    workspace,
    join(packDirectory, packed),
  ], { cwd: workspace });

  for (const fixture of ["host-consumer.mjs", "browser-consumer.mjs"]) {
    await copyFile(new URL(fixture, fixtures), join(workspace, fixture));
  }

  process.stdout.write(
    await run(process.execPath, [join(workspace, "host-consumer.mjs")], { cwd: workspace }),
  );
  for (const condition of ["browser", "worker"]) {
    process.stdout.write(
      await run(
        process.execPath,
        [`--conditions=${condition}`, join(workspace, "browser-consumer.mjs"), condition],
        { cwd: workspace },
      ),
    );
  }
} finally {
  await rm(workspace, { recursive: true, force: true });
}
