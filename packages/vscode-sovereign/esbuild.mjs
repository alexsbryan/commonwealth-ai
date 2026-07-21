// esbuild bundle for the extension — zero runtime deps, one file out.
import { build } from "esbuild";

const production = process.argv.includes("--production");
const watch = process.argv.includes("--watch");

const options = {
  entryPoints: ["src/extension.ts"],
  bundle: true,
  outfile: "dist/extension.js",
  platform: "node",
  format: "cjs",
  target: "node20",
  external: ["vscode"],
  sourcemap: !production,
  minify: production,
  logLevel: "info",
};

if (watch) {
  const ctx = await (await import("esbuild")).context(options);
  await ctx.watch();
} else {
  await build(options);
}
