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
  ]) {
    const src = path.join(ROOT, "node_modules", pkg, file);
    if (fs.existsSync(src)) {
      fs.copyFileSync(src, path.join(OUT_DIR, file));
    }
  }
}

export default defineConfig({
  root: ROOT,
  build: {
    outDir: OUT_DIR,
    emptyOutDir: true,
    target: "es2020",
    lib: {
      entry: path.join(ROOT, "frontend", "photo-journey-lite.js"),
      formats: ["es"],
      fileName: () => "photo-journey-lite.js",
    },
    minify: "esbuild",
    sourcemap: false,
    rollupOptions: {
      output: {
        // ハッシュを排し決定的な名前へ固定（chunk 名・アセット名）。
        chunkFileNames: "[name].js",
        assetFileNames: "assets/[name][extname]",
      },
    },
  },
  plugins: [
    {
      name: "navara-copy-runtime-assets",
      closeBundle() {
        copyNavaraRuntimeAssets();
      },
    },
  ],
});
