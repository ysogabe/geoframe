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
