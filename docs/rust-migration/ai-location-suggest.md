# 日候補・有償回答保存・座標化（T4.6c1/c2）

AC2/4/5/8/9/10a の位置候補 runtime を本番 factory へ接続した。`location_clue` → `location_suggest`（paid回答の保存）→ 別 `location_geocode`（候補の投影）を既存 worker / writer / Guard / LockHold で処理する。候補は既存の管理API/UIに表示し、人のSAVEだけが写真の位置をmanualへ変える。Embedding #93・グループ・LXC・実Providerの受入は含まない。実写真送信や費用の追加許可を意味しない。

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

c1 の `ai_location_suggest_results` を追加する（WITHOUT ROWID、result_key 主キー）。追加索引なし。列は result_key、manifest_json、answer_json、attempts（1..5）、analyzed_at、integrity_sha256。既存 `photo_loc_runs` の11列、`photo_loc_suggestions` の14列と意味を変えない。

manifest は day / model / prompt_version / processor_version / 順序付き全 members の Generation / 順序付き sent の photo_id・source SHA・JPEG SHA を持つ。その正規化 JSON の SHA が Rust の結果キーかつ job input_hash。job target_key は Python run_id、processor_version は model / prompt版 / `location-suggest-v1` / 画像前処理版の SHA。URL / timeout / policy 全体の hash を結果同一性に混ぜない。members は16,384件、manifest は4MiBまで。

Generation は T4.6b の写真入力世代（display / 読取相対パス / md5 / mtime / rotation / taken_at / 現品質 SHA / fallback 実 SHA / 品質失敗）を共有し、clue 処理版とは分離する。GPS / hidden を画像世代に混ぜない。`Generation::observe` は lat 非NULLの写真も読める。再利用にはさらに `SourceObservation::observe` が全 sent の実 bytes と SHA を安全な読取境界で writer 外にて検証する。通常 display も実ファイルの変更・削除を未変更と推測せず、実 bytes と品質 SHA の不一致・読取不能・観測不足は Unverified で保留し、自動再送もしない。sent を人が SAVE した後もこの観測を必要とする。普通の display は品質 SHA を使い、fallback は安全な読取境界で実 SHA を writer 外で観測する。DB 内の `matches_db` は渡された fallback 観測を使って DB 世代だけを検査する。runtime は再利用と候補 CAS の前にも SourceObservation を更新し、画像準備・送信直前・応答後に実 bytes と source SHA を検査し、DB がまだ同じだからファイルも同じと推測しない。

同じ Python run_id に新世代が現れても、過去の paid answer は別 result_key の履歴として残す。同じ result_key は冪等で、違う answer への置換を拒否する。integrity SHA は manifest / 正規化 answer / attempts / analyzed_at を結び、読み取り時にキー・型・整合性を再検証する。これは壊れた付帯情報の検出であり、DB を任意に整合的に改ざんできる管理者に対する署名ではない。

`lookup` の `observed` には `saved_sent_ids` 全件の新しい `SourceObservation` を渡す（DBだけの Generation では代用できない）。`lookup` は日 / model / prompt / 処理版、現 members が保存 members の部分集合であること、残存 members と全 sent の画像世代を確認する。sent が手動 SAVE 済みなら `saved_sent_ids` を使って追加観測する。送信済み写真の更新・回転・削除は不適合、新しい構成員の追加も不適合。`analyzed_at` 降順（同時刻は result_key 順）の適合回答を選ぶ。部分集合でも元の送信順と回答番号を保持する。

互換 run は全11列を照合する。Python が互換行を書き換えた値や、壊れた manifest / source SHA を自動 trusted にしない。来歴不明の旧 done と同じ run_id、または同 model/prompt/day の部分集合が衝突すると `LegacyUnverified` で保留し、自動再送しない。元の回答・候補は変更しない。破損した Rust 履歴は `Unverified`（現実装は保守的に location_suggest の新規保存／enqueue 全体を保留）。理由に旧 error 自由文を含めない。新たな承認 CLI は作らない。

A→B→A のように互換行が別の**検証済み Rust 履歴**Bを指す場合は `ProjectionPending`。`restore_projection` が同じ tx 内で来歴と現世代を再検査して A を再投影し、再送0で再利用する。Python 由来の不明な行は復元の根拠にしない。座標化後の短い候補 tx では `SavedAnswer::applicable` で DB 世代だけでなく履歴・互換行を再照合する（lookup後の改変も拒否）。

## runtime の接続手順

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

不在かつ sendable の Day に対して、選別順の `Vec<SendImage>` から `Plan::new` を作る。`enqueue(&mut conn, &plan, &observed, now)` は writer 上の短い tx 用。`enqueue_in(&Connection, ...)` は既存 tx（例: Handler::reconcile_cleanup の Persist）用である。旧 attempts 継承を行うため、**汎用 worker の jobs::enqueue より先にこの入口を使う**。既存の claim / token / Guard / LockHold / budgets / pause / STOP を迂回する新キューは作らない。runtime の reconcile_cleanup がこの入口を汎用enqueueより先に呼び、継承とmanual retry後の非再seedを維持する。

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

ai_jobs.attempts（課金され得る失敗回数）を唯一の正とする。互換 done.attempts は保存時の ai_jobs.attempts + 1（成功要求を含む回数）、互換 error.attempts は terminal 時の ai_jobs.attempts。別の試行 counter を導入しない。同じ Python run_id の attempts は新 job INSERT 時に失敗回数として継承する。ただし、整合性を検証した Rust 履歴と互換全11列が一致する done は、保存時の成功要求1回を引いた saved.attempts - 1 を継承する。世代ごとの成功回数を失敗へ積み上げない。例えば失敗0＋成功の done.attempts=1 からは0、失敗4＋成功の done.attempts=5 からは4を継承する。未検証の旧 done は成功分を引く根拠にせず LegacyUnverified で保留し、自動再送しない。旧 error の attempts は減らさず継承し、5以上なら最初から failed / terminal（上限5へ保守的に丸める）。既存キーの enqueue / obsolete往復では reset しない。手動 jobs::retry_failed による reset 後は再 seed しない。過去の paid answer 履歴の元 attempts は変更しない。401更新後の2回目のPOSTは別の送信予算1枠で、jobの1試行とは区別する（既存 CloudClient の保証、c2が維持）。

新表は ai::pending_active / migrate_active 系だけに置き、REQUIRED_SCHEMA / Python schema には足さない。ガード後の INGEST 起動で、有効な LocationSuggest gate があるときだけ作る。未指定 / 不正 / disabled / allow_cloudなし / daily_limitなしでは作らない。INGEST=0では作成せず旧 DB を read API で読み続ける。dry-run は有効な場合だけ不足表を追加する。c1以前からは3表、c1導入済みからは2表、全て作成済みなら0（下記）。冪等移行は既存の Python 全表・全行を変えない。

受信後は有償回答の保存を優先し、保存済み回答からの座標化再開では再送しない。ただし受信直後から DB commit 前の crash / DB失敗では保存がなく再送の可能性が残る（既存 at-least-once の限界）。外部 exactly-once、新しい課金済み先行扱い、外部の冪等契約は保証しない。現世代に適用できない応答や旧 token を現在結果にしない。この c1 は既に確定した旧世代の履歴を保持するが、現世代 CAS が失敗した応答を別 tx で勝手に成功扱いする経路は作らない。

## 本番依存・新規送信・停止

`serve` は AppState を先に構築し、その **同じArc<Geocoder>** と Ingest.cache を `production_handlers(config, lan, cloud, geocoder, cache)` へ渡す。Geocoder のrate Mutexを二重に作らない。LanClientはclueとready probeで共有し、CloudClientsはプロセスで一度構築する。retryの構築は認証・DNS・HTTPを行わず、既存CLI guard / ingest lock / writer内でだけ変更する。

各周期は再利用を新規送信より先に判定する。保存回答はPending/送信画像0/CloudClientなし/認証ファイルなしでも座標化できる（location_suggestの設定・pause・周期上限は従う）。新規送信には有効なlocation_clue設定と現Decisionのavailable・画像対応readyが必要。`CloudClient::run_checked` は全体180秒を一度作り、tokenより前と各POST予約のwriter待ち後にcallbackを呼ぶ。401 refresh後の再送も同じcallback・期限・別の予算1枠を通す。callbackはDecisionを固定Discovery/healthで再確認し、その後DB集合・Readyの順位/送信順・hidden・実source SHAを検査する。Decision障害は固定`decision_wait`へ後回しにし、Cloud認証のpauseへ誤分類しない。

画像はclueの限定読取り境界を共用する。DataDir内通常ファイル、リンク/FIFO/traversal拒否、限定read中だけ画像lock、writer外のdecode/sRGB/直接原本のEXIF一度適用、derivativeへ重複回転なし、1536pxかつ1.6MP以下、fresh JPEG q88。base64込み8MiB超過等は日と世代に結び付く恒久失敗jobにし、その日を自動で再送・ログ反復させず他の日を進める。枚数やprompt版を変えて収めない。

STOPは既存HTTP GateConnectorがDNS/TCP/TLS待ち後のHTTP初writeでも検査する。Cloudの送信済み有効回答は停止中でも保存できる。既存SIGTERM60秒/unit90秒・join上限・LockHoldの所有を変更しない。

## 座標化と候補の来歴

paid回答のparse後にgeocodeを待たず、c1の回答履歴・Python全11列run・job成功を同txで保存する。次周期のreconcileが保存result_key＋現在の日集合/世代/hidden＋`nominatim-day-photo-coarser-v1`からgeocodejobを導く。CloudのURLや他用途のpolicy hashを入力キーへ混ぜない。

GeocoderのAI入口`nominatim_checked`は `Success(Vec)`（空可）/ `Transient` / `Stopping`。既存search()/GETのVec応答は維持する。AIは固定Nominatimだけを使い、GSIへは出ない。UAはConfig.tile_ua、cacheキー`geocode_osm:`・cap5000・全体1秒1要求のMutexをGETと共有する。AIのHTTP agentはredirect/proxyなしと初writeのstop gateを持つ。rate Mutex待ちは停止・8秒の待ち上限を検査し、rate sleepはStopFlagで中断できる。送信済みの有効応答は受信・cache可能。

Python `_geocode` / `_apply_run`に合わせ、日queryを引き、正常0件のときだけcomma先頭を除いて一度検索しCOARSERへ粗くする。写真別の有効回答は保存sent順でIDを解決し、現在のclue順位で読み替えない。写真地点が引けなければ日の地点とprecisionへfallbackする。未送信member/Failedは日scope、正常0件は座標なし候補として完了する。通信障害はgeocodejobだけでattempt上限5へ再試行し、manual retryだけが上限をresetする。途中で成功した問い合わせは共有cacheから再開する。Cloudへの再送理由にしない。

c2で追加する2表（既存結果/一次列のschema変更なし、追加索引なし）:

| 表 | 内容 |
| --- | --- |
| `ai_location_candidates` | photo_id主キー、result_key、現在Dayの世代/hidden snapshot JSON、投影した既存候補14列のsnapshot JSON。Rust由来と証明できる行だけを検査/復元する |
| `ai_location_suggest_holds` | 固定reason主キーとcountだけ。pending / legacy_unverified / unverified / projection_pending / decision_wait / no_images |

候補とgeocode成功は同writer tx/tokenで確定し、直前に更新したSourceObservationとSavedAnswer::applicableで全世代・現在集合・互換runを再検査する。SQL失敗は候補・付帯行・成功をrollbackし、有償回答を残す。成功後に互換候補だけが欠けた場合は、現世代と所有snapshotを検証してHTTPなしで復元する。Pythonが別内容に書き換えた候補へ偽の新来歴を付けたり、その行を消したりしない。

管理API読取りも同じ来歴検査を通す。Rustが書いたのと一致する候補が更新/回転/削除/追加/SAVE/hidden/時刻変更で古くなれば表示しない。旧候補やPythonによる別内容の行は従来どおり参照する。`photos`の列は候補作成で一切UPDATEしない。status/CLIは`ai_location_holds`と固定job理由`decision_wait`/`geocode_wait`を件数だけ追加し、無い場合キーを省略する。地名/ID/hash/run_id/日付/本文/認証を新しいログや追加statusへ出さない。

新表は有効なsuggest gate時だけmigration/dry-run対象にし、REQUIRED_SCHEMAへ足さない。INGEST=0、設定未指定/不正/無効、allow_cloud=false、日/周期上限0では新規Cloud token/DNS/POSTを行わない。旧AI表だけの読取りやPython切り戻しの既存run/suggestion列を維持する。

## c2 の検証・引継ぎ

試験は合成RGB/JPEG・合成DB・未登録のRSA fixture・loopbackだけ。実LAN/クラウド/画像/鍵/DB、SSH、LXCは使わない。fixtureは`include_str!`で埋め込み、既存の `tests/fixtures/jwt-test-pkcs8.b64` / `location-suggest-python.json` / `location-clue-python.json` 等を使用する。新たな外部配置fixtureは不要。LXCでRust単体試験を再ビルド/実行するときは更新済み`tools/gen_location_suggest_fixture.py`と既存`app/geoframe/loc_suggest.py`もソースツリーに含める（既存c1と同じPython3前提）。

新規試験は `ai::location_suggest::runtime::tests` と `geocode::tests::ai_*`。factoryからDecision/token/generate/paid保存/別Nominatim/実候補API/人のSAVE、障害と再起動でCloud POST不変、Decision未準備/停止や未許可でPOST0、token/401/応答/地名問い合わせ中の入力変更、世代失効、6枚/rank/Failed/hidden/巨大body、旧token/SQL rollback、geocode上限5/manual retry、共有cache/UA/rate、STOP待ち後/初write/受信中を確認する。c1のattempts継承・旧done保留・Python全11列互換試験も維持する。`--apply-roundtrip`では実Python ASTの`_geocode` / `_apply_run`を合成地名応答と書込みcollectorで実行し、Rustの全11列paid runとmanual SAVE後の部分集合14列候補を読み戻して一致を確認する。

最終確認（2026-10-27）: `cargo fmt --all -- --check`、`cargo clippy --workspace --all-targets -- -D warnings`、`cargo test --workspace`、`cargo build --workspace --release --target x86_64-unknown-linux-musl` が成功。**全28 suite、1107 pass / 0 fail / 2 ignored**。追加24試験（runtime22＋Geocoder停止/共有rate2）、既存c1の27試験を維持。fileはstatic-pie linked、`--version`は `geoframe 0.1.0`。Pythonのlocation-suggest / location-clue / cloud-schema AST fixture checkも成功。

通常sandboxのruntime試験初回はloopback bindの `Operation not permitted` で12失敗したため、許可された権限付き実行へ切り替えた。新fixtureの分単位時刻/Decision usage、HTTP試験のHost、通信先差替え走査が試験ファイルを識別する配置を修正した。初回workspaceの908 pass / 2 failはHostと試験配置の不備で、修正後は上記全件成功。既存flakeは観測していない。

否定対照は、①CloudのPOST予約後callbackを外す→token待ち中変更試験、②geocode最終writer CASを外す→HTTP後/commit前変更試験、③Nominatim rate待ちを外す→GET/AI並行間隔試験、の3件で期待するassertion失敗を確認し、元ソースへ復元した。

別Solの通常レビューとLXC受入は親タスクへ返す。実Provider疎通、実写真/費用の許可、#93/E5/E6、P4全体完了は主張しない。


## c1 の検証記録

最終ソースで `cargo fmt --all -- --check`、`cargo clippy --workspace --all-targets --offline -- -D warnings`、`cargo test --workspace --offline`、`cargo build --workspace --release --target x86_64-unknown-linux-musl --offline` を通過。全28 suite 合計 **1081 pass / 0 fail / 2 ignored**、新規 domain / repository は25試験。既存 ignore は大量変更の計測試験と writer の例。musl バイナリは file が static-pie linked、ldd が statically linked、`--version` も成功。

通常 sandbox での全件試験初回は loopback bind の `Operation not permitted` により lib 670 pass / 217 fail / 1 ignored で中断した。loopback を許可した権限付き再実行と、最終の日付 CAS 補強後の再実行は共に全件成功。既存の flaky failure は出ていない。再実行も合成 fixture / loopback のみで、実 LAN / 実クラウド / 実鍵 / 実写真 / 実 DB / SSH / LXC は使用していない。

AST fixture は `python3 tools/gen_location_suggest_fixture.py --check` と既存の cloud schema fixture 照合で確認。Rust の全11列互換 run を、実 Python AST の `_parse_answer` / `run_id_for` / `_reusable_run` で読み戻し、部分集合再利用も成功した。gap等値を分割する変異、旧done衝突の保留を除く変異、保存sentの観測世代照合を除く変異、旧attemptsを0へ落とす変異は、それぞれ対応する新試験が assertion で失敗することを確認し、元へ戻した。c2 の HTTP / worker / 座標化 E2E、LXC、別Solの簡易レビューは親担当の後続作業。

## 修正ラウンド2の検証

開始 HEAD `40ce3f0`（原タスク開始 `8c0b906`）。検証済み Rust done の互換 attempts に含まれる成功1回を、新世代 job の失敗回数へ継承しないよう修正した。schema・公開 API・予算経路・保存時の成功回数投影は変更していない。追加表／dry-run の不足数も従来どおり有効時1表、作成済み0。

追加2試験で、同じ送信 ID / model / prompt のまま mtime と品質行の世代を8回更新し、毎世代 claim / success、全 job の失敗回数0、互換 done.attempts=1、全8回答の履歴保持を確認。実際の record_failure_with を4回経た成功からも、新世代の失敗回数4・互換 done.attempts=5を確認した。既存試験は未検証旧 done.attempts=5＋画像世代変更でも保留／新 job なしを補強し、旧 error の上限5・manual retry だけの reset・世代往復を再確認。継承を旧処理へ戻す否定対照では domain / repository の25 pass・追加2 failとなり、修正版へ復元した。

初回の修正後 focused 試験は26 pass・1 fail。既存 A→B→A 試験が、同じ保存時刻／回答に加えて誤った attempts の差で互換投影を区別していたためで、B を後の保存時刻で確定する fixture に修正した。最終の domain / repository 全27試験は成功し、既存 flake は出ていない。

最終検証は fmt、workspace 全 targets clippy（`-D warnings`）、workspace test 全28 suite **1083 pass / 0 fail / 2 ignored**、musl release build が成功（Cargo は全て `--offline`、test は既知の loopback 制限のため権限付き）。musl は file / ldd で静的リンク、`--version` 起動成功。両 Python AST fixture の `--check` も成功。合成 fixture / loopback のみ使用し、c2・LXC・別Solによる再レビューは親へ引き継ぐ。
