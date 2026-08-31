/* `npm run build:navara` が生成する app/static/navara/ の整合性を検証する。
 *
 * このディレクトリはコミット済みの生成物であり、次の不変条件を保つこと:
 *  - エントリ（photo-journey-lite.js）はハッシュ無しの固定名で、5つの公開APIを再輸出する
 *  - メインバンドル（photo-journey-lite2.js）はエンジンWASMを base64 で内包する
 *  - @navaramap/three/dist/assets/（大気・雲・ノイズ・水の重いテクスチャ）は
 *    参照されていない（デッドウェイトを同梱しない）
 *  - エンジン系WASM3種は \0asm マジックを持つ実体として同階層に置かれる
 *  - spark worker チャンク（spark.module-*.js）が1つだけ存在する
 */
const assert = require('assert');
const fs = require('fs');
const path = require('path');

const NAVARA = 'static/navara';
const entries = fs.readdirSync(NAVARA);

function read(name) {
  return fs.readFileSync(path.join(NAVARA, name));
}

// ---- エンジンWASM3種（実体） ----
for (const wasm of ['navara_wasm_bg.wasm', 'navara_wasm_api_bg.wasm', 'navara_wasm_worker_bg.wasm']) {
  const buf = read(wasm);
  assert.ok(buf.length > 0, `${wasm} が空`);
  assert.strictEqual(buf.readUInt32BE(0), 0x0061736d, `${wasm} が \0asm マジックで始まっていない`);
}

// ---- エントリ（facade）の公開APIと固定名 ----
const facade = read('photo-journey-lite.js').toString('utf8');
for (const sym of ['LITE_OPTIONS', 'addPhotoJourneyLayers', 'createPhotoJourneyView',
                   'flyPhotoJourneyCamera', 'setPhotoJourneyCamera']) {
  assert.ok(facade.includes(sym), `エントリが ${sym} を再輸出していない`);
}
assert.ok(facade.includes('./photo-journey-lite2.js'), 'エントリがメインバンドルを相対importしていない');

// ---- メインバンドル（WASM内包・デッドウェイト排除） ----
const bundle = read('photo-journey-lite2.js');
assert.ok(bundle.length > 1024 * 1024, 'メインバンドルが1MB未満（ビルドが崩れている疑い）');
assert.ok(bundle.includes(Buffer.from('data:application/wasm;base64,')), 'エンジンWASMがbase64で内包されていない');
assert.ok(!bundle.includes(Buffer.from('assets/')), 'メインバンドルが assets/（重いテクスチャ）を参照している');

// ---- spark worker チャンク（1つだけ） ----
const sparkFiles = entries.filter(f => /^spark\.module-.*\.js$/.test(f));
assert.strictEqual(sparkFiles.length, 1, `spark worker チャンクは1つのはずが ${sparkFiles.length} 個: ${sparkFiles.join(', ')}`);
assert.ok(read(sparkFiles[0]).length > 0, 'spark worker チャンクが空');

console.log('navara build integrity: OK');
