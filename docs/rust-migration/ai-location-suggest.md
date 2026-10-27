# 日候補の domain・有償回答保存（T4.6c1）

AC2/4/8/9/10a のうち日集合・固定要求・応答型・世代別保存／再利用の domain / DB 部分を実装した。`production_handlers` への登録、CloudClient の実行、Decision の送信前 ready 確認、Nominatim 座標化、候補の HTTP/UI 統合は T4.6c2。Embedding #93・グループ・LXC・実 AI の受入は含まない。写真一次列と `photo_loc_suggestions` には書かない。

## 固定契約

`location_clue::load_targets` → `location_suggest::group_days` の順に呼ぶ。後者は現在の hidden を DB から独立に読み、`include_hidden=false` では hidden の Pending も集合から除く。時刻 / ID の安定順、隣接 gap が **設定の infer_windows_s の最大値を超える**場合、または元の taken_at の先頭10文字の日付が変わる場合に分ける。等しい gap では分けない。Config の `infer_windows_s()` をそのまま渡し、12時間の固定値で広い設定を無視しない。

各 Day は全 cloud 対象の `members()` と、Ready の通過写真だけを選んだ `sent_ids()` を持つ。Pending が一つでも残れば `sendable=false`。Failed は判定済み・送信しない。score >= 0.2 または landmark を通し、landmark 優先 / score 降順 / ID 昇順で最大6枚。全 Failed / 閾値未満なら `NoImages` で要求を作らない。hidden / 設定 / STOP / Provider ready / 送信許可 / 予算の送信直前 gate をこの domain で代替しない。

`run_id_for(model,sent_ids)` は Python の `loc-v2|model|comma(sorted(ids))` の UTF-8 SHA256 先頭16hexに `loc-` を付ける。これはハッシュの順序で、送信順は別に保持する。`Plan::request()` の固定 prompt / schema / 日付と `写真1` 等の番号は Python に一致する。photo_judge は従来の `Image n` のまま。`responseJsonSchema` / temperature 0.1 / base64 込み8MiBの既存制限を維持する。

`SendImage` は photo_id / 実読取元 source SHA / `PreparedImage`。`Plan::new` は選別順と同じ ID 列、現在の写真世代と一致する source SHA を要求する。`PreparedImage` の入口は RGB→fresh JPEG のままで、圧縮済み入力の constructor は追加しない。`PreparedImage::sha256` は実送信 JPEG の来歴を返す。writer に画像やファイルを読ませない。

## 型付き応答と Python との意図差

`Answer::parse(reply.json_text(), sent_count)` は CloudClient が transport envelope を解いた後の JSON object を受ける。必須 string の place_name、既知 Precision / Confidence、evidence、写真番号付き PhotoAnswer を返す。Debug / エラーには本文を出さない。未知 precision/confidence は unknown、写真別未知 precision は日単位の precision、写真別未知 confidence は unknown。写真番号は送信順に対する1始まりで、適用時には必ず **SavedAnswer の sent_ids** で解決する。現行の選別順で読み替えない。

Python の不正型で例外になる穴は互換の対象にしない。

- 応答 JSON は1MiBまで。地名 / country / city / query は各512 UTF-8 bytes、country_code は16 bytes。制御文字を拒否する。これらは地名検索の値で、URL / 宛先 / パスには使わない。
- evidence は配列なら string へ正規化（JSON の有限 scalar / container は Python str 互換）、配列以外なら空。64要素、各2048 bytes、container の深さ8 / 各64要素。改行・tab以外の制御文字は拒否する。
- photos は64要素まで、object / 有効整数 index / string place_name を持つ項目だけを採用。範囲外 index、place_name のない項目は除外する。bool / 小数 / i64 桁溢れ index、重複番号、不正な photos 型、採用項目の query の不正型は固定 `BadResponse`。NaN / Inf / 非有限数の JSON は拒否する。
- country / city / country_code / geocode_query が欠けた場合は空文字を補う。存在するのに string でない場合は拒否する。未知 payload は正規化結果から捨て、自由な URL 等として保持しない。未知 enum のみを明示的に正規化する。

これらの上限・型検査・空文字補完・未知 field 除去は Python より厳格な差である。schema が許す通常の合成応答は受け付ける。Python の定数と純粋関数は `tools/gen_location_suggest_fixture.py` が AST から抽出し、アプリ import / 通信 / 実画像なしで fixture を作る（`--check` で実ソースとの一致を検証）。アプリ実行時の Python ソース読取依存はない。

## 来歴・履歴・互換行

`ai_location_suggest_results` を1表だけ追加する（WITHOUT ROWID、result_key 主キー）。追加索引なし。列は result_key、manifest_json、answer_json、attempts（1..5）、analyzed_at、integrity_sha256。既存 `photo_loc_runs` の11列、`photo_loc_suggestions` の14列と意味を変えない。

manifest は day / model / prompt_version / processor_version / 順序付き全 members の Generation / 順序付き sent の photo_id・source SHA・JPEG SHA を持つ。その正規化 JSON の SHA が Rust の結果キーかつ job input_hash。job target_key は Python run_id、processor_version は model / prompt版 / `location-suggest-v1` / 画像前処理版の SHA。URL / timeout / policy 全体の hash を結果同一性に混ぜない。members は16,384件、manifest は4MiBまで。

Generation は T4.6b の写真入力世代（display / 読取相対パス / md5 / mtime / rotation / taken_at / 現品質 SHA / fallback 実 SHA / 品質失敗）を共有し、clue 処理版とは分離する。GPS / hidden を画像世代に混ぜない。`Generation::observe` は lat 非NULLの写真も読める。再利用にはさらに `SourceObservation::observe` が全 sent の実 bytes と SHA を安全な読取境界で writer 外にて検証する。通常 display も実ファイルの変更・削除を未変更と推測せず、実 bytes と品質 SHA の不一致・読取不能・観測不足は Unverified で保留し、自動再送もしない。sent を人が SAVE した後もこの観測を必要とする。普通の display は品質 SHA を使い、fallback は安全な読取境界で実 SHA を writer 外で観測する。DB 内の `matches_db` は渡された fallback 観測を使って DB 世代だけを検査する。c2 は再利用と候補 CAS の前にも SourceObservation を更新し、画像準備・送信直前・応答後に実 bytes と source SHA を検査し、DB がまだ同じだからファイルも同じと推測しない。

同じ Python run_id に新世代が現れても、過去の paid answer は別 result_key の履歴として残す。同じ result_key は冪等で、違う answer への置換を拒否する。integrity SHA は manifest / 正規化 answer / attempts / analyzed_at を結び、読み取り時にキー・型・整合性を再検証する。これは壊れた付帯情報の検出であり、DB を任意に整合的に改ざんできる管理者に対する署名ではない。

`lookup` の `observed` には `saved_sent_ids` 全件の新しい `SourceObservation` を渡す（DBだけの Generation では代用できない）。`lookup` は日 / model / prompt / 処理版、現 members が保存 members の部分集合であること、残存 members と全 sent の画像世代を確認する。sent が手動 SAVE 済みなら `saved_sent_ids` を使って追加観測する。送信済み写真の更新・回転・削除は不適合、新しい構成員の追加も不適合。`analyzed_at` 降順（同時刻は result_key 順）の適合回答を選ぶ。部分集合でも元の送信順と回答番号を保持する。

互換 run は全11列を照合する。Python が互換行を書き換えた値や、壊れた manifest / source SHA を自動 trusted にしない。来歴不明の旧 done と同じ run_id、または同 model/prompt/day の部分集合が衝突すると `LegacyUnverified` で保留し、自動再送しない。元の回答・候補は変更しない。破損した Rust 履歴は `Unverified`（現実装は保守的に location_suggest の新規保存／enqueue 全体を保留）。理由に旧 error 自由文を含めない。新たな承認 CLI は作らない。

A→B→A のように互換行が別の**検証済み Rust 履歴**Bを指す場合は `ProjectionPending`。`restore_projection` が同じ tx 内で来歴と現世代を再検査して A を再投影し、再送0で再利用する。Python 由来の不明な行は復元の根拠にしない。座標化後の短い候補 tx では `SavedAnswer::applicable` で DB 世代だけでなく履歴・互換行を再照合する（lookup後の改変も拒否）。

## c2 の接続手順

以下の API は CloudClient / 認証を必要としない。画像準備・観測と HTTP は writer 外で行う。

```rust,ignore
let targets = location_clue::load_targets(&read, &data, clue_model)?;
let days = location_suggest::group_days(
    &read, &data, targets, include_hidden, &config.infer_windows_s(),
)?;
// 各 Day について、まず再利用を調べる。Pending/送信画像0でも保存済み回答を読める。
let sent = location_suggest::saved_sent_ids(&read, &day, cloud_model)?;
let observed = sent.into_iter()
    .filter_map(|id| SourceObservation::observe(&read, &data, id).transpose())
    .collect::<rusqlite::Result<Vec<_>>>()?;
match location_suggest::lookup(&read, &day, cloud_model, &observed)? {
    Lookup::Reusable(saved) => { /* CloudClient なしで別 geocode job へ */ }
    Lookup::ProjectionPending => { /* writer で restore_projection、その回答を geocode へ */ }
    Lookup::LegacyUnverified | Lookup::Unverified => { /* 保留。自動再送しない */ }
    Lookup::Absent => { /* day.sendable() なら準備・enqueue。まだ送信しない */ }
}
```

不在かつ sendable の Day に対して、選別順の `Vec<SendImage>` から `Plan::new` を作る。`enqueue(&mut conn, &plan, &observed, now)` は writer 上の短い tx 用。`enqueue_in(&Connection, ...)` は既存 tx（例: Handler::reconcile_cleanup の Persist）用である。旧 attempts 継承を行うため、**汎用 worker の jobs::enqueue より先にこの入口を使う**。既存の claim / token / Guard / LockHold / budgets / pause / STOP を迂回する新キューは作らない。c1 では handler も CloudClients も構築・登録しない。

c2 の Handler::run は `plan.request()` を CloudClient へ渡して応答を parse し、実 SHA / 現 policy を再検査した後に、次の短い保存 callback を返せる。

```rust,ignore
let answer = Answer::parse(reply.json_text(), plan.sent_ids().len())?;
Ok(Box::new(move |tx| {
    location_suggest::persist_answer(tx, &claimed, &plan, &answer, now)
})) // worker が jobs::complete で token / savepoint / succeeded と共に確定
```

repository を単独で使うときは `complete(&mut conn, &claimed, &plan, &answer, now)` が同じ jobs::complete を包む。`persist_answer` は外側 tx と現 token を要求し、集合の追加/削除/手動SAVE/画像世代変更なら Changed。savepoint が callback の全書込を捨て、obsolete にする。旧tokenは callback に入らず Rejected。SQL失敗は履歴・互換行・jobを全て rollback。実 HTTP / 画像計算は一切含めない。

失敗は jobs::record_failure_with の terminal callback または worker の terminal 修復用 Persist から `project_failure(tx,&plan,now)` を呼ぶ。実 job の failed / attempts / 固定 ErrorCode をキーで読み、同じ tx で互換 error に投影する。途中 retry/defer では投影しない。未知 error は `other`、paid done は上書きしない。現集合が変われば Changed で部分保存しない。

## 試行・移行・残る境界

ai_jobs.attempts（課金され得る失敗回数）を唯一の正とする。互換 done.attempts は保存時の ai_jobs.attempts + 1（成功要求を含む回数）、互換 error.attempts は terminal 時の ai_jobs.attempts。別の試行 counter を導入しない。同じ Python run_id の旧 error 等の attempts は新 job INSERT 時に継承し、5以上なら最初から failed / terminal（上限5へ保守的に丸める）。既存キーの enqueue / obsolete往復では reset しない。手動 jobs::retry_failed による reset 後は再 seed しない。過去の paid answer 履歴の元 attempts は変更しない。401更新後の2回目のPOSTは別の送信予算1枠で、jobの1試行とは区別する（既存 CloudClient の保証、c2が維持）。

新表は ai::pending_active / migrate_active 系だけに置き、REQUIRED_SCHEMA / Python schema には足さない。ガード後の INGEST 起動で、有効な LocationSuggest gate があるときだけ作る。未指定 / 不正 / disabled / allow_cloudなし / daily_limitなしでは作らない。INGEST=0では作成せず旧 DB を read API で読み続ける。dry-run は有効な場合だけ不足1表を追加し、既存表があれば0。冪等移行は既存の Python 全表・全行を変えない。

受信後は有償回答の保存を優先し、保存済み回答からの座標化再開では再送しない。ただし受信直後から DB commit 前の crash / DB失敗では保存がなく再送の可能性が残る（既存 at-least-once の限界）。外部 exactly-once、新しい課金済み先行扱い、外部の冪等契約は保証しない。現世代に適用できない応答や旧 token を現在結果にしない。この c1 は既に確定した旧世代の履歴を保持するが、現世代 CAS が失敗した応答を別 tx で勝手に成功扱いする経路は作らない。

c2 では Decision ready / CloudClient / 401待ち後の現写真と許可 gate、別 location_geocode job、Geocoder共有と0件/障害の区別、候補 CAS（photos.lat/lon不変）、handler登録、HTTP/UI/Python切り戻しの通し試験を実装する。これらとLXCは今回の合格主張に含めない。

## c1 の検証記録

最終ソースで `cargo fmt --all -- --check`、`cargo clippy --workspace --all-targets --offline -- -D warnings`、`cargo test --workspace --offline`、`cargo build --workspace --release --target x86_64-unknown-linux-musl --offline` を通過。全28 suite 合計 **1081 pass / 0 fail / 2 ignored**、新規 domain / repository は25試験。既存 ignore は大量変更の計測試験と writer の例。musl バイナリは file が static-pie linked、ldd が statically linked、`--version` も成功。

通常 sandbox での全件試験初回は loopback bind の `Operation not permitted` により lib 670 pass / 217 fail / 1 ignored で中断した。loopback を許可した権限付き再実行と、最終の日付 CAS 補強後の再実行は共に全件成功。既存の flaky failure は出ていない。再実行も合成 fixture / loopback のみで、実 LAN / 実クラウド / 実鍵 / 実写真 / 実 DB / SSH / LXC は使用していない。

AST fixture は `python3 tools/gen_location_suggest_fixture.py --check` と既存の cloud schema fixture 照合で確認。Rust の全11列互換 run を、実 Python AST の `_parse_answer` / `run_id_for` / `_reusable_run` で読み戻し、部分集合再利用も成功した。gap等値を分割する変異、旧done衝突の保留を除く変異、保存sentの観測世代照合を除く変異、旧attemptsを0へ落とす変異は、それぞれ対応する新試験が assertion で失敗することを確認し、元へ戻した。c2 の HTTP / worker / 座標化 E2E、LXC、別Solの簡易レビューは親担当の後続作業。
