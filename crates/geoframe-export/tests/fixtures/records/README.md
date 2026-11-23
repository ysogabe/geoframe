# 記録の形式の実例

ジョブの記録（`<state-dir>/jobs/<job_id>.json`）と公開の記録（`<state-dir>/published/<種類>-<フォルダ名>.json`）の実例。
`tests/record_format.rs` が、これを読めること・今の書き手が同じバイト列を書くことを確かめる。Drive の操作の実装を替えても、
記録の形式が変わっていない（前のバイナリが読める）ことの証拠にする。

- 作り方: 2026-11-23 に、`geoframe-export` の本物のジョブの流れ（偽の写真サービス・偽の Drive に対する `Service`）と、
  記録の書き手（`JobStore::write`・`PublishedStore::upsert`）に書かせた。手書きではない。
- 差し替えたもの: ジョブ ID（`a1a1…` など、見て分かる値）と時刻（`2030-03-10T12:00:00+09:00` など）。
- 値は全部、架空（撮影日・Drive の ID `fid0001` …・MD5）。実データは入れない。
- `jobs/`: 成功（新規・置き換え）・衝突・失敗（取得の失敗・`drive_auth_invalid`）・止まった（`interrupted`）・照合できない
  （`blocked`）・途中の段（`uploading`・`publishing`＋MD5・`trashing` で `running` のまま残ったもの）。
- `published/<名前>/`: 公開の直後の記録（新規・置き換え）・旧フォルダの退避が残っている記録（`previous`）・MD5 を取れなかった記録
  （`files` なし）。ファイル名は、本物の置き場と同じ `<種類>-<フォルダ名>.json`。

## 生成元と、作り直すとき

- 生成元のコード: PR #87（Drive 操作の型を中立にした段）のコミット `13dc3d1`..`7fd0bdf` の範囲。実例は、その範囲の最後
  （`7fd0bdf`）のコードに書かせた。記録の書き手は `crates/geoframe-export/src/jobs.rs`（`JobStore::write`）と
  `src/published.rs`（`PublishedStore::upsert`）。この 2 つのバイト列が変わると `tests/record_format.rs` が落ちる。
- 再生成の手順（コミットしない一時の試験で行う。実例そのものを手で書き換えない）:
  1. `tests/job_flow.rs` と同じ部品（`tests/common/site.rs` の偽の写真サービス、`tests/common/drive.rs` の `Env`・偽の Drive）で
     `Service` を動かす。作りたい場面ごとに、偽の Drive の状態・規則（`Env::add_rule`）・段階の記録の口（`Hook`）を変える。
     例: 置き換えは `on_exists: replace` で、宛先に管理しているフォルダを `Env::seed_managed` で作っておく。途中の段
     （`uploading` など）は、`Hook::fail_at` でその段の記録を失敗させて `running` のまま止める。
  2. 状態ディレクトリの `jobs/<job_id>.json` と `published/<種類>-<フォルダ名>.json` を、そのまま写す。
  3. 次の値だけを置き換える（それ以外は書かれたまま）:
     - ジョブ ID（本物は乱数）→ 場面ごとに別の、見て分かる値（`a1a1…`・`a2a2…`。32 桁）。公開の記録の `job_id` も同じ値にそろえる。
     - 時刻（`started_at`・`finished_at`・`resolved_at`）→ `2030-03-10T12:00:00+09:00`・`2030-03-10T12:00:30+09:00`（`started_ns` は `started_at` の
       ナノ秒）。
     - Drive の ID は偽の Drive の `fid0001` … のままでよい（架空の値）。
  4. `cargo test -p geoframe-export --test record_format` が通ること。バイト列が変わるなら、記録の形式を変えたことになる
     ので、置き換える前に、前のバイナリが読めるか（読めない記録は `blocked`）を確かめる。
- 偽の Drive の実装（以前の rclone 版・今の HTTP 版）を替えても、実例は作り直さない。作り直すのは、記録の形式を意図して変えたときだけ。
