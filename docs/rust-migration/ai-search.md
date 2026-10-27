# E5/E6: AI検索HTTPと対話runtime（T4.4b）

T4.4bは `GET /api/ai/status`・`GET /api/ai/search` を既存画面へ接続する。実装は `ai/{query,search_view}.rs`、共有LANは `ai/provider.rs`、起動時の注入は `cli.rs`・`server::AppState`。T4.4aの画像ジョブ・lineage・chunk処理・公開条件は維持する。新scene/rank表・Gemini判定はT4.5。実LAN/LXCの最終受入とP4全体security-reviewは親担当であり、ここでは合成試験の結果だけを記録する。

## APIと互換境界

- 両APIは完全一致のGET。先頭の `//` は従来どおり畳む。HEAD/POSTは既存ガードで403、その他メソッドは501。Host検査・CSRF・CORS非対応・静的ホワイトリストは従来どおり。残る `/api/ai/*` は既存の定型503。JSONは `application/json; charset=utf-8`・`Cache-Control: no-store`。
- E6はPythonの `parse_qs` の最初の非空値・`int` と同じ既存 `pyquery` を使う。`groups` は既定12、1〜30へclamp。Unicode数字・符号・数字間の `_`・重複param・不正UTF-8の扱いも共通。不正groupsは400 `bad_params`。例外文字列や入力値はエラーに含めない。
- groups変換 → AI_ENABLED判定 → 毎分制限 → query正規化/300 Unicode文字検査 → DBの順。空検索・hit・長すぎるqueryも受付窓を消費するが、不正groupsとAI無効は消費しない。毎分制限はプロセス共有の直近60秒、境界 `>=60秒` で除去、既存 `LegacyAi.search_per_min`（既定30）を尊重する。monotonic時計を注入可能にして同じlockで判定/記録する。429は定型文。
- 正規化はPythonの `" ".join(query.strip().split())`。Unicode whitespaceにFS/GS/RS/USも含める。大文字小文字は表示・送信queryに残し、cache keyだけfull casefoldする。`pycasefold.rs` はPython Unicode表から生成し、`tools/gen_ai_query_unicode.py --check` で再現する（生成環境Unicode 14.0）。NUL等の非空白制御文字はPythonと同様保持し、JSONでescapeする。
- `include_hidden` は最初の値が `1` または `true` のときだけ真。mode、group/card全項目、整数/null/bool、丸め4桁、favorite/recommended/score/keep_scoreの安定並び、構図代表・件数はPython fixtureと比較する。空結果はPythonと同じく `include_hidden` を省略する。
- **意図したPythonとの差**: 非空queryは新しいactive identityと明示した固定query契約が必要。旧DB・未構築・不適合は、候補0件でも503（Pythonの旧実装はDB行0件ならProvider確認前に200 empty）。適合するactive空間があり、検索対象がhidden/品質条件等で0件なら200 empty。旧ベクトルへ新queryを掛ける例外は設けない。
- **空queryは保存済み結果を使う**。新active/Provider/設定ファイルが無くても、世代が合う旧モデルの結果から200 recommended/emptyを返す。AI_ENABLED=0は空queryも503。新activeの有効vectorを持つ新写真は旧判定が無くても候補に入り、keep_scoreが無ければ品質点を使う。
- `GEOFRAME_AI_VISUAL_SIMILARITY` は0〜1の有限数を受け、未指定時は従来のbackend別既定値を使う。不正値はAI側の`legacy_env`エラーとして保持し、推論を無効にして写真配信は継続する。AI_ENABLED=0の指定も保持する。構図比較の内部値には安全な既定値を入れ、不正な入力値はログへ出さない。

## 一つのLAN client、期限と停止

`serve` は `LanClient::new` を一度だけ実行し、背景factoryにはclone、対話runtimeには同じagent/laneを渡す。AppStateの既定runtimeは外向きclientを作らないため、fixtureや未注入の起動も無送信。CLIのretry経路は従来どおり。

非空missは対話1枠を**待たず**取得してから `spawn_blocking` に同期queryを渡す。同時missは余分なPOSTや枠待ちを作らず即定型503（先行完了後ならhit）。背景1枠とは独立し、待つ対話を新背景より優先する既存Lanes規則を継続する。

15秒のDeadlineはparam処理前に作り、正規化、cache、Catalog待ち、Discovery、POST、再probe、最終DB読取・整形を含めて再開始しない。外側futureにも残時間のtimeoutを掛ける。同期要求は同じDeadlineとSession/StopFlagを所有し、外側がキャンセルされても枠を早く返さない。取り込み時のLockHoldをruntimeに保持する。DNS pin/接続先制約/redirect禁止/first-write停止gateは変更しない。POST末尾のDiscovery再確認時間の予約はT4.4aの規則を使う。

送信前は現在policy、active identity、DB pause/wait、LAN共有メモリのpause/wait、stop、残時間を検査し、Discovery後にも再検査する。DB transaction/読取接続/cache lockはHTTP中に保持しない。Catalogは既存のblocking読取プールを使う。LANは予算不要なのでwriter busyでも検索可能。

Provider Wait/Pauseは同じLanClientの共有メモリへ即反映し、画像/Decision/Decision依存の背景側も送信前に確認する。背景側の時刻は既存RunCtx時計を使う。writerがあるときだけ既存pause/wait表への保存を試み、順番待ちは `min(残時間,200ms)`。busyで保存できなくても同じ起動内では待機が効く。writer無しではDBに書かず、schemaも作らない。Cloud queryは現時点で未対応なので、許可設定・writerの有無にかかわらず無送信503。Cloud仮APIや予算予約の迂回・Gemini fallbackは無い。

## Cache・DB snapshot・旧判定

Cacheはquery vectorだけのLRU64件。keyは全SpaceIdentityのSHA-256（space ID/Provider空間/モデル/revision/両前処理/次元/正規化を含む）、query正規化版、正規化済みqueryのfull casefold。空query/失敗は保存しない。key/query/vectorをDebug/log/DBへ出さない。hitはprobe/POSTを行わず、Provider停止・pause・共有wait・StopFlag中でも利用できる。明示AI無効、契約/active identityの違いはhitを使わない。

送信前は小さなactive metadataだけを読み、全vectorを先読みしない。受信後はactive pointer・検証済み同一空間vector・現写真・QUALITY・旧判定・groupを同一読取transactionで得る。切替が間に入ったら503。更新/回転/削除は現世代JOINで落ち、hidden/favorite/SAVEはそのsnapshotの値を返す。古い判定はdisplay/md5/mtime/content SHA/errorが合うものだけ。新vectorの構図比較に旧vectorは混ぜず、active無しの空検索だけ同じ旧モデル内で比較する。

同SHAのvectorはArc共有し、写真ごとに768要素を複製しない。JSON整形に必要な行を確保し、返す上位groupだけ構図比較する。読取transactionは外向き通信前後で閉じ、構図の貪欲比較はDB snapshotを閉じた後で行う。比較中も期限を検査する。新たな選別書込み・人手一次列の変更は無い。

## E5

既存トップレベルkeyと型を維持する。新activeがあればphotos/eligible/pendingは `index::status_in` の検査済み実数。`failed_excluded` はpendingと分ける。旧参照時は世代適合する旧モデルのdistinct SHAをphotosへ返す。judged/recommended/groups/group_statusは現在の旧選別ビューの有効結果で数え、新空間だけで旧選別を解除しない。他モデル件数も旧参照時は有効世代、新active時は既存 `other_shas` の重複除去規則を使う。

pipelineは `index_state`（no_tables/unset/unusable/active）、`query_compatible`、indexed/pending/failed_excluded/corrupt、固定理由別件数、選別件数だけ。生の `photo_ai_status` JSONはコピーしない。未構築・不適合をcompleteとは表示しない。space ID/SHA/URL/Provider自由文のreasonは出さず、未知のfailed理由は `other`。新空間のembedding_backendは固定契約ならlocal_http、その他の保存済み空間はactive_index。

## Schema・移行・dry-run

**追加schemaなし。** T4.4aの `ai_image_embedding_lineage` を含む既存のpending/migrateとdry-run増分は不変。E5/E6読取は移行を呼ばず、INGEST=0/ai_*表無しでも読む。Wait/Pauseの必要な記録だけ既存Writer/表を使い、query/job/usage/旧有償結果・一次列は書かない。Python切り戻しの表やデータも変えない。

## 合成検証

`tools/gen_ai_search_fixture.py` は本物のPython `ai_search.api_ai_search/api_ai_status` を一時DB・固定vectorで実行し、SQL/JSON fixtureを生成する。認証・実DB・実写真・外部AIを使わない。

専用試験は `ai/query/tests.rs`。Python全card/group比較、空検索・旧DB・空結果・Unicode/int/重複param、rate偽時計、LRU64/eviction/casefold/停止中hit、同時miss/満杯、writer busy、Wait/Pause共有/保存/期限、POST後のactive切替と更新/削除/hidden/favorite/SAVE、旧構図との混合禁止、corrupt/pending/failed E5、無効/Cloud/AI0無送信、DB待ち/timeout後再probe、キャンセル中の枠/LockHold、実HTTP GET/HEAD/Host/既存ガード/STATUS/setloc/背景分離、ログ秘匿を確認する。既存T4.4aおよびHTTP/停止/予算/tokenの全回帰も実行する。

検証数・musl・性能の最終値は下記に追記する。実LANでの背景8件処理中query15秒受入は親担当であり、この合成結果で代用しない。

### 最終検証記録（2026-10-28）

- `cargo fmt --all --check`、`cargo clippy --workspace --all-targets -- -D warnings`: 成功。
- `cargo test --workspace`: **1,172 passed / 0 failed / 2 ignored / 0 measured / 0 filtered**（28 suite結果の合計）。T4.4b追加はquery専用25件＋Python丸め境界1件。ignoreは既存の大量変更の計測用lib試験とWriterの説明用doctest。新しいignore/skipなし。
- Python fixtureとUnicode生成の両 `--check`: 成功。Pythonそのものを一時DB/固定vectorへ実行し、実AI送信なし。
- 初回loopback試験はsandboxのbind拒否で失敗し、権限付き再実行。全体回帰では共有LAN待機にwall clockを使ったため既存の偽時計での再開2件が失敗した。背景側を `RunCtx::now()` に統一して解消した。カードの整数スコア型とPython丸め境界も比較で確認・修正した。最終suiteに失敗なし。
- `cargo build --release --target x86_64-unknown-linux-musl -p geoframe`: 成功。`file`: static-pie linked。`readelf -l/-d`: INTERP/NEEDED無し。`--version`: `geoframe 0.1.0`。生成物19,324,336 bytes、SHA-256 `3f76580f8c99c9c3a342541a71806db604f5a7f6ceb38410eb1186f5fde8c22a`。
- release単独の `synthetic_6500x768_http_query_performance`（1 passed、ignore 0）: 合成6,500 SHA×768要素、実loopback E6 HTTP **100ms**（Discovery/固定query POST/DB/JSON含む）、直接view **95ms**、SQLite traceでBEGIN→COMMITの読取snapshot **89,751µs**。RSSは準備後 **15,248KiB** → HTTP後 **45,616KiB** →別接続のview追加測定後 **71,356KiB**。RSSは各時点の値で、allocator/SQLite cacheを含む。別接続測定分をHTTP単独の増分やpeakとは扱わない。
- 背景POSTを同期ゲートで保持して対話query/フィード/E5/管理STATUS/setlocを2秒上限内に確認。writer busy中のLAN検索も成功し、エラー記録は200msで打ち切って共有メモリ待機を保った。短い同一DeadlineでPOST timeout後の再probeとcache失敗非保存、外側キャンセル後の枠/LockHoldを確認。時間依存の大量反復なし。

性能はこの開発ホスト上の合成値であり、画像推論時間や実LANの背景8件中query15秒を証明しない。実Provider/実写真/実DB/SSH/鍵/Cloud/本番サービスは未使用、`/var/lib/geoframe` は参照・変更していない。

### 中断した通常レビューの指摘への修正（2026-10-28）

通常レビューは利用上限で最終報告前に中断したが、新規の構図閾値環境変数が不正な場合に`Config::load`全体が失敗し、AI_ENABLED=0でもserveが終了する再現が残った。親がこの小修正を行い、構図閾値を`AiConfig`で検査して、不正なら既存の`legacy_env`へ分類するよう変更した。検証済みの閾値だけを`Config`へ渡し、内部値はbackend別既定値へ戻す。明示AI無効フラグ・配信の継続・入力値の非出力を保つ。設定JSONやDBスキーマ、Providerの契約に変更はない。

- 追加回帰は旧コードで意図した`ConfigError`により失敗することを確認した。修正後はAI設定21試験が成功。不正文字列・NaN・Infinity・範囲外、AI_ENABLED=0、未指定の既定値と有効な境界値を検査した。
- 実プロセスの既存起動結合試験に構図閾値不正＋AI_ENABLED=0のケースを追加し、healthz・フィード200、終了コード0、DB不変、固定エラーコードとログ非露出を確認した。
- `cargo fmt --all --check`、全targetsのclippy（警告をエラー扱い）、全workspace **28 group・1173成功・失敗0・既存ignore2**、musl releaseビルドが成功した。静的リンク（INTERP/NEEDED無し）とversion起動も確認した。性能の追加測定を目的とした反復はしていない。
- 中断したレビューの途中結果はquery24件・丸め1件・共有境界17件の成功報告までで、正式な完了判定には数えない。修正後コミットを対象に、新しい別Solレビューで残る確認と最終判定を行う。実LAN/LXCのHTTP並行受入は引き続き親担当。
