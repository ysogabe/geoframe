/* ============================================================
   vite.config.mjs — 3D PHOTO JOURNEY Lite の Navara バンドル生成
   ============================================================
   `npm run build:navara` が app/static/navara/ を決定的に生成する。

   方針:
     - @navaramap/three の dist/index.js（ESM）をエントリとし、three /
       postprocessing / engine / engine-api / engine-worker を単一バンドルへ集約する。
     - 動的な `new URL("assets/…", import.meta.url)` 参照（three-stdlib の draco や
       fontWorker、テクスチャ）は Vite/Rollup が書き換えないため、pre-built の
       `node_modules/@navaramap/three/dist/assets/` をそのまま app/static/navara/assets/
       へコピーする（バンドルと同一階層に置き、相対解決させる）。
     - engine の WASM グロス（navara_wasm_bg.wasm 等）も同階層へコピーする。
       Vite はビルド時に base64 へインラインすることがあるが、実 .wasm も併置することで
       どちらの解決経路でも成立する。
     - ハッシュ付きファイル名を避け、決定的な出力名に固定する。
   ============================================================ */
import { defineConfig } from "vite";
import { fileURLToPath } from "node:url";
import path from "node:path";
import fs from "node:fs";

const ROOT = path.dirname(fileURLToPath(import.meta.url));
const OUT_DIR = path.join(ROOT, "app", "static", "navara");

/* engine 系 WASM の実体（wasm-bindgen が `new URL("x_bg.wasm", import.meta.url)` で
   参照する）をバンドルと同一階層へコピーする。

   ※Vite はビルド時にこの WASM を base64 へインラインする（バンドル内の
   `data:application/wasm;base64,AGFzb…`）。よってこの実 .wasm は実行時の二重化に
   過ぎないが、「JS/WASM/worker 資産をコミットする」要件を満たす実体であり、
   `\0asm` マジックの検査対象にもなる（安全網を兼ねる）。

   ※`@navaramap/three/dist/assets/`（大気・雲・ノイズ・水のテクスチャ等）は意図的に
   コピーしない。これらは addDefaultPhotorealScene()（本 Lite では呼ばない）専用の
   重い資産で、バンドルは参照していない（バンドル内に `assets/` 参照は0件）。 */
function copyNavaraRuntimeAssets() {
  for (const [pkg, file] of [
    ["@navaramap/engine", "navara_wasm_bg.wasm"],
    ["@navaramap/engine-api", "navara_wasm_api_bg.wasm"],
    ["@navaramap/engine-worker", "navara_wasm_worker_bg.wasm"],
    ["@navaramap/engine-font-worker", "navara_wasm_font_worker_bg.wasm"],
  ]) {
    const packageRoot = path.join(ROOT, "node_modules", pkg);
    const direct = path.join(packageRoot, file);
    const src = fs.existsSync(direct)
      ? direct
      : pkg === "@navaramap/engine-font-worker"
        ? findFontWorkerWasm(packageRoot)
        : direct;
    if (fs.existsSync(src) && fs.statSync(src).isFile()) {
      fs.copyFileSync(src, path.join(OUT_DIR, file));
    }
  }
}

function findFontWorkerWasm(packageRoot) {
  const assets = path.join(packageRoot, "dist", "assets");
  if (!fs.existsSync(assets)) return assets;
  const candidate = fs.readdirSync(assets).find((name) =>
    /^navara_wasm_font_worker_bg-.*\.wasm$/.test(name),
  );
  return candidate ? path.join(assets, candidate) : assets;
}

/* @navaramap/three@0.1.1 publishes the tile/font workers as base64 data URLs.
   Those workers resolve their WASM with self.location.href, which is invalid
   when self.location is a data: URL. Keep the generated bundle self-contained,
   but serve these two workers as real sibling files. */
function externalizeNavaraWorkers() {
  const workerDataUrl =
    /new URL\("data:text\/javascript;base64,([^"]+)", import\.meta\.url\)(?:\.toString\(\)|\.href)/g;
  return {
    name: "navara-external-workers",
    generateBundle(_options, bundle) {
      const emitted = new Set();
      for (const output of Object.values(bundle)) {
        if (output.type !== "chunk") continue;
        output.code = output.code.replace(workerDataUrl, (full, encoded) => {
          const source = Buffer.from(encoded, "base64").toString("utf8");
          const isTileWorker = source.includes("navara_wasm_worker_bg-");
          const isFontWorker = source.includes("navara_wasm_font_worker_bg-");
          if (!isTileWorker && !isFontWorker) return full;

          const fileName = isTileWorker
            ? "navara-worker.js"
            : "navara-font-worker.js";
          const normalized = source
            .replace(/navara_wasm_worker_bg-[A-Za-z0-9_-]+\.wasm/g, "navara_wasm_worker_bg.wasm")
            .replace(/navara_wasm_font_worker_bg-[A-Za-z0-9_-]+\.wasm/g, "navara_wasm_font_worker_bg.wasm")
            .replace(/\n\/\/# sourceMappingURL=.*$/gm, "");
          if (!emitted.has(fileName)) {
            this.emitFile({ type: "asset", fileName, source: normalized });
            emitted.add(fileName);
          }
          return `new URL("./${fileName}", import.meta.url).toString()`;
        });
      }
    },
  };
}

export default defineConfig({
  root: ROOT,
  resolve: {
    // All Navara/default-plugin packages share the pinned Three.js instance.
    // Keep this explicit for Vite's split worker/chunk graph.
    dedupe: ["three"],
  },
  build: {
    outDir: OUT_DIR,
    emptyOutDir: true,
    target: "es2020",
    // @navaramap/three imports its tile worker with `?url&worker`.  Library
    // mode otherwise enables Rollup's inlineDynamicImports and Vite can turn
    // that worker into a data: URL.  The worker itself resolves relative
    // WASM/module URLs, so it must remain a real file served from this output.
    assetsInlineLimit: 0,
    lib: {
      entry: path.join(ROOT, "frontend", "photo-journey-lite.js"),
      formats: ["es"],
      fileName: () => "photo-journey-lite.js",
    },
    minify: "esbuild",
    sourcemap: false,
    rollupOptions: {
      output: {
        inlineDynamicImports: false,
        // ハッシュを排し決定的な名前へ固定（chunk 名・アセット名）。
        chunkFileNames: "[name].js",
        assetFileNames: "assets/[name][extname]",
      },
    },
  },
  plugins: [
    externalizeNavaraWorkers(),
    {
      name: "navara-copy-runtime-assets",
      closeBundle() {
        copyNavaraRuntimeAssets();
      },
    },
  ],
});
