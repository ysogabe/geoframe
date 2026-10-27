# T4.5a: 同一場面domain・順位・世代別repository

原開始SHAは `5e1862498d04a04585da67de781e798cda45d686`、この実装の開始HEADは親の開始記録 `c0d0864e675baa0630b9a8bbf10b53450082fe9a`。同じworktree / `feat/rust-migration-p4` で実装。実装は `ai/groups.rs`、`ai/groups/repository.rs`、`ai/rank.rs`。要求は [p4-prd.md](p4-prd.md) AC4/6/8/9/10a、設計は [p4-design.md](p4-design.md) §6.2/6.3/7。今回の完了範囲はdomainとDB APIであり、周期・フィード・E5/E6の本接続はT4.5b、任意Gemini比較はT4.5c、LXC/実API/独立Solレビューは親担当。

## 公開APIと呼出し順

1. `groups::pending(&Connection)` は不足表名だけを返す。`groups::migrate(&mut Connection)` はガード済みINGEST writerで明示的に呼ぶ。
2. writer外の読取接続で `groups::snapshot(&Connection) -> Result<Option<Snapshot>, Error>`。active pointer・完全SpaceIdentity・現写真/品質特徴・その空間のvector・failed job・有効旧選別を1つの読取transactionで取得する。transactionを閉じてからvectorを検査する。active無し/旧DBはNone。
3. 任意の管理者設定パスはwriter外で `rank::Model::load(path, snapshot.identity())`。配置なし/不正の `rank::Reason` をそのまま固定式fallbackの理由にできる。パス探索・既定パス設定はこのAPIでは行わない。
4. `groups::compute(&snapshot, options, Result<&Model, Reason>, stopped) -> Option<Plan>` はpure計算。停止はNone。Snapshot/Photo/Inputに現display/md5/mtime/rotation/撮影時刻/品質SHAと特徴量を保持する。写真ID/入力/モデル等はDebugへ出さない。
5. writerで成分ごとに `groups::save(conn, &plan, index, stopped)`。Writerのclosureは例えば `move |conn| Ok(groups::save(conn, &plan, index, &stopped))` として外側のWriterエラーと内側の固定domainエラーを扱う。結果はStored/Unchanged/Stale/Interrupted。Staleは再snapshot、Interruptedは未確定のまま終了する。
6. `groups::load_current(conn, processor_version)` は同じ読取transactionでpointer/rows/現入力/activeを照合し、要求した処理版の現世代結果だけを返す。版の指定は必須。重みfallbackの場合はその成分の `Group::processor_version()` が固定式版になる。

`Group` は結果ID・全入力世代・処理版・状態・重複分類・順位方式/固定fallback理由・全構成員/順位/score/shortlist・旧選別由来を持つ。`Member::shown/favorite/representative` と `Group::has_shown_representative()` はbの適用判定材料。hiddenの最上位が代表になった場合も情報を残すので、表示可能代表無しで残りを隠す根拠にはしない。favorite/hidden/手動位置/rotationを書き換えるAPIはない。

## 成分・状態・同一性

- 時刻窓は既定900秒。`Options::new` は1〜86400秒、有限cosine -1..1、shortlist 2〜6を検査する。環境変数/Configへの追加配線はしていない。既存の不正AI設定でも配信継続という原則は不変。
- 同じ完全SpaceIdentityの検査済みstored-f32 vectorだけを比較する。f32をf64へ拡張した内積とnormでPythonのcosineを再現し、差が窓と等しい場合とcosineが閾値と等しい場合を含める。同時刻はID順、辺の連結成分をunion-findで作る。連鎖は窓幅を超えた成分にもなる。
- 同SHAの全写真を保持し、vectorだけArc共有する。Pythonの正本1枚への縮約は再現しない。一部更新/削除/最小ID不在でも未変更複製は残す。同じ成分内で既につながった2点のcosineは省略してよい（連結性は同じ）。辺行列は作らない。
- 成分の最小撮影時刻−窓〜最大時刻＋窓の**両端を含む範囲**に未登録/pending/running/deferred/obsolete/結果欠損/壊れたvector/未解析品質があればWaiting。Waitingはscore=None、shortlist/representative無し、active参照を書かない。候補だけでは表示を抑止しない。
- 同空間・現全複製世代・現Embedding処理版のfailedだけをFailedEmbeddingとして除外する。品質項目が欠けたfailedも除外できる。別版/別space/古世代failedは待機のまま。再試行で有効vectorが保存された次snapshotでは復帰する。品質解析のerror行はFailedQualityとして除外する。失敗写真自体を削除しない。
- display無しはNoDisplay、時刻無しはMissingTime、不正時刻はInvalidTimeの固定理由で**対象外**。時刻不明を任意の窓へ推測して入れず、一次列を補正しない。
- 待機無しの成分はLocalRanked。Singletonも型とrepositoryで保持する（Pythonの候補は2枚以上のみ）。Gemini無しでルール代表を参照できるが、この状態だけで抑止を許可するHTTP/フィード配線は未実装。部分重複はReviewOverlapで保留する。
- 処理版は完全space fingerprint・窓・閾値の正確なf64 bits・shortlist上限・成分/順位算法版・必要な実効重みdigestから導く。group/result IDは処理版と**shortlist外も含む全構成員の入力世代**から導く。現入力世代は写真ID/display/md5/mtime/rotation/撮影時刻/現品質SHA・特徴を束縛する。URL/timeout/Cloud許可/他用途の設定は含めない。
- Pythonとcomponents/順位/shortlistを比較する一方、旧shortlist SHAだけの16桁group IDとは一致させない。完全空間/全世代由来の64桁結果ID、複製全保持、singleton保持が意図差。成分の列挙は人数降順→結果IDで安定化する（旧Pythonの平均類似度による成分間表示順はbのビューでの別責務）。

## 順位とrankfile互換

固定式は既存Rust qualityの `rank_scores(&[Metrics])` と `rank_group` が同じ内部計算を共有する。normalizeのmin/maxはグループで1度求め、同じ式を各写真へ適用する。Pythonのround(1)、同率はID昇順。学習モデルはPCAをu/cへ畳み、stored-f32 vectorに対する1内積とbase項でscoreを求め、グループ内0..100、round(2)、全同点50にする。導出係数・積和・score・hi−loのoverflow/非finiteを検出し、固定式へ戻す。学習特徴のluminanceが欠けても、固定式の必須項目があれば固定式を使える。固定式の必須特徴が欠けた画像はPendingQuality。

`geoframe-rank-linear-v1` の既存キーは維持し、任意の次の互換宣言だけを追加した。

```json
"compatibility": {
  "space_fingerprint": "SpaceIdentity::fingerprint() の完全値",
  "embedding_processor_version": "index::image_embedding_processor_version() の値",
  "input_representation": "stored-f32-v1"
}
```

これは管理者が重みの学習空間を証明する宣言である。モデル名/次元から値を推測して付けない。旧v1ファイルが持つembedding_model/dimではrevision・両前処理・Provider共通空間・正規化等を証明できないため、宣言無しはUnverifiedSpaceで固定式。明示宣言も全fingerprint/processor/model/dim/representation一致が必要で、旧Gemini/Q8_0用重みをBF16へ自動転用しない。旧形式を自動書換えする処理はない。

読取はO_NONBLOCKで開いた同一ハンドルの通常ファイルだけ、最大8MiB（stat後も上限+1まで）。JSON深さ8、各配列4096要素、PCA行256、次元1〜4096、base特徴6種whitelist/重複禁止、shape/有限数を検査する。typed serdeで未知/重複キーも拒否する。新しい数値parser/framework/crateは追加しない。source_digestは検査したbytesのSHA、結果同一性のdigestは完全spaceと実効係数から導くので、表示用name/パス/空白だけで順位処理版は変わらない。名前/パス/JSON本文をエラーやログへ出さず、全失敗は固定Reasonで呼出し側に返す。

## repository・移行・CAS・旧重複

選択した方式は**明示repository migrate/pending API**。起動factory/worker/HTTP/フィードへ配線せず、`ai::pending_active/migrate_active` にも追加していない。

| 追加表 | 内容 |
| --- | --- |
| ai_scene_groups | space_id/完全fingerprint、全入力世代、処理版、窓/閾値/shortlist上限、状態/順位方式/重みdigest、影響範囲/入力guard、旧選別由来 |
| ai_scene_members | result_id/photo_id、各世代、順位/score/shortlist/代表/表示可否/favorite。rank一意 |
| ai_scene_active | photo_id→確定済みローカル結果への参照。選別やフィードのactiveではない |

追加は3表、明示索引0、列追加0。WITHOUT ROWIDの主キーとmembersのUNIQUE自動索引を使う。`groups::pending` の増分は旧DBで3、2回目0。現CLI/INGEST起動のdry-run増分は0（未接続）。bがknown embedding設定付きINGEST1のガード済み移行へこのAPIを接続するときは+3を報告する。INGEST0では呼ばず、readonly APIも移行/書込をしない。REQUIRED_SCHEMAとPython schemaは不変。

保存は1成分/1短transaction。計算済みPlanを渡し、完全active空間・影響する全写真/品質の現値・vector bytes・現failedキー・同SHA全複製世代・有効旧選別由来・計画時の新結果参照を再照合する。別処理版の計画が先に採用されていれば後着の旧計画はStale。結果/全members/active参照は同時に確定する。SQL故障/最後の停止は全rollback、同じPlanの再保存はUnchanged。重い成分計算/順位/PCA/ファイル読取はwriterへ持ち込まない。CASの全現写真メタデータ走査と関連vector bytesの比較hashはwriterで必要な直線作業として残す。影響外の写真追加はguardに入らず、変更が続いても別の窓を進められる。影響範囲内の追加は保留/再計画し、古い顔ぶれを公開しない。

読み取りはpointerとrowsを同snapshotで読む。版/空間/世代/guardが不一致の結果は返さず、全構成員のactive参照がその結果に揃う場合だけ返す。過去の結果や別空間/別版、有償旧結果をGCしない。未完成/空集合/全failedで旧表を削除・解除しない。

旧選別は `selection::VALID_MEMBERS` の世代・表示可能推薦ルールを共有し、judged/same_scene=1で表示可能推薦のある旧群だけを有効単位とする。新全構成員と有効旧単位が同一で全旧構成員も現有効ならExact、どの有効旧単位とも重ならなければDisjoint、それ以外はPartial。複数旧群をまとめる場合もPartial。旧群を変造/削除せず、旧ID・judge model/version・旧group/member全行と有効性のdigestを保存する。`LegacyOrigin::paid_reuse_verified()` はfalse。旧列だけでは完全prompt/rotation等の由来は証明できず、この一致を有償回答再利用の許可にしない。cは新たな検証済み回答のprompt/モデル/実送信集合/画像世代を持つ必要がある。

## b/c接続点と未確認

bは有効な設定からOptionsを作り、snapshot→writer外rankfile読取/compute→各成分saveを周期に接続する。表示適用はLocalRankedの現世代、Exact/Disjoint、表示可能代表の存在等を選別ビューで確認し、Waiting/ReviewOverlapで旧抑制を置換しない。旧有効選別維持・favorite優先・代表不在時の安全表示を既存feed/search/adminへ接続して初めてAC8/9のruntime範囲を完了とする。

cはLocalRankedの全入力/shortlist/順位を基礎に、用途許可・予算・現在の画像/送信集合/世代CASを持つ任意比較を実装する。モデル名/job成功/Exact分類だけで過去の有償結果を転用しない。今回Cloud POST・Provider factory・scene handlerは追加していない。

実LAN/実AI/実写真/実DB/SSH/鍵/本番サービスと `/var/lib/geoframe` は参照/変更していない。既存rank_model.jsonの探索/読取はせず、合成JSONだけを使った。実機/LXC/P4全体security-reviewとT4.5b/cのruntime受入は未実施。

## 合成検証と測定

`ai/groups/tests.rs` は固定vector/合成SQLite/合成rank JSONのみ。`tools/gen_scene_fixture.py` は実Python `build_scene_candidates/RankModel/quality.rank_group` を一時DATA_DIRで実行し、モデル読取を固定値に置換してfixtureを作る。fixtureの再現性も試験し、Python不足/Pillow不足を黙ってskipしない。切り戻し試験はPythonのfeed/status/現行group再構築を実行し、人手一次列と新3表が不変であることを確認する（Pythonが旧groupを再構築することは正常）。

最終suite数・性能・否定対照は検証完了後に下へ追記する。

### 最終検証記録（2026-10-28）

- `cargo fmt --all --check` と `cargo clippy --workspace --all-targets -- -D warnings`: 成功、警告0。
- `RUST_TEST_THREADS=4 cargo test --workspace -- --nocapture`: **28 result group、1,208成功 / 失敗0 / 既存ignore2 / measured0 / filtered0**。内訳はlib1,016成功/ignore1、main0、integration188成功、doctest4成功/ignore1。T4.5aの専用35件はすべて成功。新ignore/条件付き省略なし。Python/Pillow/Nodeの既存回帰も実行。compile-fail doctestのコンパイルエラーは期待した成功で、製品ビルド失敗ではない。
- loopback制限は既知のため、全workspaceは初めから権限付きで実行した。実LAN/外部AIは不使用。通常試験のassert失敗は0。初回の試験コードのprivateフィールド参照/SQLite usize取得とunused-mut/type-complexity等のcompile/lint指摘を修正した。製品側では実装中の自己確認で、failedを品質不足より先に除外すること、空displayの除外、Python同等f64 cosine、処理版間のactive参照CASを確定し、それぞれ合成回帰へ固定した。
- `python3 tools/gen_scene_fixture.py --check`: 成功。連鎖/900秒同値・超過/pending影響範囲同値・超過/cosine閾値同値/品質順位・同点/学習モデルのPCA・正規化/shortlist6件を実Python出力と照合。
- 否定対照は2件。入力CASを外すと手動SAVE/favorite/hidden/回転等の試験がStored≠Staleで失敗。cosineの`>=`を`>`にするとPython閾値同値fixtureが成分不一致で失敗。どちらも意図したassertでexit101を確認し、finallyで原状復元した後、最終全workspaceを成功させた。これらの故障注入失敗を通常suiteの失敗に数えていない。
- `cargo build --workspace --release --target x86_64-unknown-linux-musl`: 成功。`file`: static-pie linked、`readelf -l/-d`: INTERP/NEEDED無し、`--version`: `geoframe 0.1.0`。19,423,416 bytes、SHA-256 `6bda467b1f141b71fc644d70ec3192f3b4527e5cc7a5941793d91ee1610781c1`。

release専用の `synthetic_6500x768_compute_releases_writer_and_measures_save` を1回実行（1成功、ignore0）。x86_64 Linux、この開発ホスト、合成6,500写真/6,500 SHA×768、**全同時刻・同一方向vector**、SQLite WAL、一成分6,500構成員、重み無し、外部通信無し、時間閾値assert無し。

| 区間 | 実測 |
| --- | --- |
| snapshot（DB読取/検査、writer外） | 81.718ms |
| compute（成分/順位、writer外） | 64.202ms |
| writer/save（一成分のCAS＋全members＋参照確定） | 87.259ms |
| RSS: 準備後→snapshot後→compute後→save/read後 | 11,876→70,108→74,748→90,420KiB |

RSSは各時点で、SQLite cache/allocatorと読取API追加実行を含み、peakやモデル推論の使用量とは扱わない。compute内の同期ゲートを保持したまま、実Writerで独立setting更新が成功することも確認した。debug専用実行ではsnapshot約397ms、compute約767ms、writer/save約292ms（同時実行の試験/allocator状態によりRSSは変動）。release値と混ぜない。

計算量は時刻ソートO(n log n)、窓内の組の走査O(P α(n))と、必要なcosine K件のO(Kd)。最悪P/KはO(n²)で、異方向で未連結の写真ばかりならCPU時間は増える。この測定は全同時刻で既に同成分のcosineを省くケースであり、最悪の全未連結cosine時間の証明にはしない。停止検査は外側と128組ごとに行う。辺行列を作らずメモリはO(n d)。すべてwriter/transaction外のため、病理的な組比較を無制限writer区間へ持ち込まない。writerは現写真メタデータ走査/関連vector bytes hash/構成員行保存の直線処理が残る。一成分の原子性を守るため6,500行の保存自体は分割しない。多数の小成分のsaveには各々メタデータ走査があるので、bは既に同一結果の成分を不要に毎周期書込み依頼しない。

AC4は現入力/完全空間/新参照CASと一次列不変、AC6は空間/破損/failed世代分離と窓保留、AC8はローカル成分・順位・状態、AC9は旧群重複分類/保持・Python切り戻し、AC10aは合成回帰/秘匿/移行無し読取/muslをdomain・DB範囲で確認した。周期/HTTP/feed/実機でのAC完了およびP4全体完了を主張しない。

### 保存済み結果の検証修正（2026-10-28）

通常レビューの必須2件を親が修正した。現参照は件数だけでなくphoto_id/result_idの集合一致を確認する。保存済みの各構成員は現写真の世代と表示可否・favoriteに照合し、全員の世代からinput_generationとresult_idを再計算する。保存された処理オプション・space・重み方式・旧群由来、有限な0〜100のscore、連続順位・安定並び・shortlist・代表と0/1フラグも検証する。不整合な結果は固定`CorruptResult`、参照集合の不一致は現結果から除外する。重い成分計算やモデル再評価をwriterへ持ち込む変更ではない。

`save`のUnchanged経路も同じ現入力・結果構造検査と、全ヘッダー/全構成員の保存値比較を通す。破損を正常な既存結果として省略しない。参照が変わった古いPlanは従来どおりStaleとし、再snapshotしたPlanでは破損した行を原子的に書き直せる。修復時は当該result_idを指す余分な参照だけを除き、別結果・別空間や旧有償表を削除しない。APIシグネチャ・schema・移行/dry-run増分は不変。

- 追加回帰2件は旧コードで意図したassert失敗を確認後、修正版で成功。集合のすり替えと、世代改変・構成員/参照の欠落・Infinity・順位の欠落・代表/フラグ不正・入力世代/処理オプション/表示可否/score範囲の破損を直接注入した。読み取りとUnchangedの両方で拒否し、再計画後の修復も確認した。
- 修正後の専用試験は性能1件を除く36件が成功。fmt・全targets clippy、全workspace **28 group・1210成功・失敗0・既存ignore2**、musl release静的リンク/versionが成功した。
- 全体試験後に、全テーブルで同じ不正result_idへ付け替えたケースを同じ回帰試験へ追加した。製品コードは変更せず、拡張した対象試験1件とclippyを再確認し成功した。追加を含め11種類の破損を扱う。LXCと独立再レビューは親の後続であり、この修正だけでT4.5a完了とはしない。

再レビューでは必須2件とも解消、必須0・推奨1だった。独立の専用36試験と追加13試験（構造破損23ケースを含む）が成功。推奨のヘッダー型変換エラーも、memberと共通の分類へ変更し、SQLite実行エラーは`Database`、保存値の型不正は`CorruptResult`とした。shortlistへ文字列を注入する12番目の破損ケースで拒否と修復を確認し、専用36試験・fmt・全targets clippyが成功した。この小修正後に全workspaceは反復していない。

検証LXCでは同じ修正を含むmusl release試験バイナリで専用36件すべて成功（省略・ignoreなし）。Python fixture再生成と実切り戻しも含む。合成6,500件×768次元・全同時刻／同方向を1回測定し、snapshot 330.608ms、compute 232.508ms、writer/save 590.046ms、RSSは準備後10,704→snapshot後71,612→compute後74,568→save/read後75,476KiBだった。writer外計算中にも独立Writer操作が成功した。これは専用試験バイナリの単回値で、実画像・Provider・runtimeの性能保証や全未連結cosineの最悪時間の測定ではない。周期処理への接続はT4.5bで行う。

## T4.5b: 周期・選別ビュー・E5/E6のruntime接続（2026-10-28）

原開始SHA **64d3860** を維持し、親の開始記録HEAD `25ac12c05db3b4040d70fcaebea62e9ab5d00ce7` から同じcheckout/`feat/rust-migration-p4`で実装した。上のT4.5aの「未接続」は当時の境界であり、今回の接続点は `groups/runtime.rs`、`worker.rs`、`ai.rs`、`ingest.rs`、`search_view.rs`、`admin.rs`。新しいクラウド送信・photo_judge handler・有償判定の置換は追加しない。P4-AC3/4/6/8/9/10aの今回のローカルruntime・合成HTTP範囲を扱い、T4.5c、独立Solレビュー、親の最終LXC/実API受入、P4全体の完了とは区別する。

### 起動・設定・仕事量

`INGEST=1 && AI_ENABLED=1 && ai.invalid().is_none()` で `Runtime::production(Arc<Config>)` とローカル専用スレッドを起動する。AI設定ファイルが未指定でも、DBに既にある完全なactive identityと適合vectorから処理できる。Providerの登録/Discovery、LAN lane、photo_judgeの許可/予算には依存しない。画像・Decision・日候補の既存workerと単一Writer/StopFlag/LockHoldを共有し、Provider応答中も独立して進む。AI無効時はworker startupの回収・run記録もしない。既存INGESTの基本移行と、AI背景処理の無効化を区別する。

| 既存設定 | 今回の適用 |
| --- | --- |
| GEOFRAME_AI_POLL_S | 既存60秒、最小30秒の周期を共有 |
| GEOFRAME_AI_TIME_WINDOW_MIN | 既定15分。1〜86400秒の有限値、整数撮影秒との比較に同値な秒のfloorを使う |
| GEOFRAME_AI_SIMILARITY | Pythonのbackend別既定（local 0.83、gemini 0.82）、有限-1〜1 |
| GEOFRAME_AI_SHORTLIST_MAX | Pythonと同じ整数・2〜6 clamp、既定6 |
| GEOFRAME_RANK_MODEL | 明示パス、未指定/空ならDATA_DIR/models/rank_model.json。writer/tx外で毎回検証 |

不正な数値は既存`legacy_env`としてAIだけを無効にし、明示AI_ENABLED=0と写真配信を保つ。重みの欠落/不正/空間不適合は既存の固定Reasonと固定式fallback。モデル名/次元から旧重み互換を推測しない。完全identity、写真/品質/failed、設定、実効係数digestの変化は次周期で再計画する。fallback理由だけが変わった場合も保存ヘッダーの照合で更新する。

周期はsnapshot→モデル読取→compute→保存値照合をwriter外で行う。`pending_saves(conn, plan, limit)` は全ヘッダー・全members・正確な参照集合を比較し、正常同一結果をwriterへ再依頼しない。破損をID一致だけで省略しない。1周期最大16成分、原則8192構成員（単一成分は原子性のため分割しない）。未処理分は次周期へ持ち越す。共通の協調的期限10秒を計算・writer待ち・保存で共有し、停止/期限時は未確定をrollbackする。単一SQL/DB読取を強制中断する実時間保証ではない。

`save_batch(conn, plan, indexes, stopped)` は最大16成分の短いtransactionで現在入力を1回共有読取し、各成分の完全guard/構造/expected_active CASを保つ。Staleの成分以外を進められる。SQLエラー・停止はbatch全体rollback。旧`save`/`load_current`と破損分類・fresh Plan修復は維持する。時刻範囲、photo ID、旧群の所属、参照集合の読取内索引を作り、成分ごとの全写真map構築やactive参照の全走査を除いた。新しいDB索引は追加していない。成分計算自体の窓内cosineは最悪O(n²d)であり、writerへ持ち込まない。

### 読取API・選別・後続cへの境界

- `Prepared::read(conn, config)` はactive metadataとrankfileを最終read transactionの外で準備し、`Prepared::load_in(conn)` は呼出側の同一snapshot内でactive本体/完全空間/現世代/現processor/全members/参照を検査する。途中の空間切替では結果を使わない。モデルが成分単位でMissingFeature/Overflow等へfallbackする場合は、別の現入力snapshotからwriter/tx外でその理由を再検証し、最終snapshotでも同じguardである場合だけ採用する。新weightが利用可能になった古い固定式結果を正常扱いしない。
- E6の既存card/group JSON形・型を維持し、有効な成分にだけ`local_rank/local_score/local_representative/scene_state/scene_overlap`を追加する。LocalRanked Exact/Disjointをscene groupとして使い、空queryは保存順位、意味検索は同じactive vectorのcosineを使う。favoriteと世代適合する旧判定の優先を保つ。PartialはReviewOverlapとして管理情報を残し、旧groupの単位を保持する。Waitingは検索の代表/順位にしない。
- E5 `pipeline.scenes` と管理STATUS `ai_scenes` に`state/local_ranked/waiting/review_overlap/rules/linear`の固定コード・件数を追加。管理写真の任意`scene`に状態/重複/順位/score/代表/shortlistを付ける。旧DBはschemaを作らず、管理追加キーも3表が存在する時だけ。破損したsceneはunavailable/未採用にして配信全体を止めない。
- E5 `selection_groups/selection_suppressed` はfeedと共通の旧選別から数える。LocalRanked/Waiting/ReviewOverlapはjudgedへ加算しない。`selection::admin_counts.suppressed`は表示可能な非favoriteの実際の抑止数へ揃えた。合成管理fixtureでは、位置無しの旧非推薦を除くため1→0。選別表・抑止集合・フィード件数は周期の前後で不変であり、この集計の修正を新しい抑止解除と混同しない。
- `ai_scene_active`はローカル結果の参照であり、feed非表示の許可ではない。旧有償表は今回一切書換えない。検索空間の切替、新しい未判定成分、pending窓による新規feed抑止は0。旧代表の更新/削除/hidden/位置無し/品質非表示時の安全な表示とfavorite免除は共通選別SQLを維持する。

後続T4.5cは`Group`の現入力世代/処理版/全members/shortlist/順位/Overlap/LegacyOriginを使える。置換候補は現入力集合・世代の一致するExact、またはDisjointのみで、Partialは管理確認のまま。`paid_reuse_verified()==false`は維持する。今回のscene参照やローカル代表を有償判定済みの証拠にしない。新Gemini回答の送信集合・prompt・モデル・世代・token CASと抑止採用はcが別途実装する。

### 移行・dry-run・Python

T4.5aの3表を`ai::pending_active/migrate_active`へ接続した。引数は従来のconn/clue/suggestに`scenes: bool`を追加し、INGEST guard/lock後の起動が有効性を渡す。全部入り`ai::pending/migrate`も3表を含む。列追加0・追加明示索引0・新規schema定義0（既存3表の起動配線）。REQUIRED_SCHEMA・Python schema・旧行は不変。

用途専用表なしのPython相当DBでは、有効INGESTの不足objectは10→13、全用途込みの明示移行は14→17。既にaの3表があれば増分0。INGEST0/AI0/不正AI設定では今回の3表を作らず、readonly読取も追加しない。INGEST0のdry-runは従来の基本10件を仮計画するだけ、有効INGESTのdry-runは13件を報告する。Python切り戻しの既存合成試験では新表を無視してfeed/status/旧group再構築を実行し、一次列と新表を維持する。

### 親のLXC用fixture

`tools/gen_scene_runtime_fixture.py --output <存在しない専用ディレクトリ> [--photos 6500] [--many-components] [--weight]` は標準ライブラリだけで合成DBを作る。既存パスの更新は拒否し、アプリのimport・本番DB・実weight・鍵・外部通信は無い。旧Python schema＋合成768次元active indexだけを作り、scene3表は未作成。共通空間はsynthetic専用で、実BF16/Geminiとの互換宣言はしない。任意weightはこの合成空間の完全fingerprintを持つ。

このfixtureは有効INGESTで不足10 object（既にindexの3 objectがあるため）→migration→ローカル周期→E5/E6空検索・管理・再起動のLXC確認に使える。`GEOFRAME_EMBED_BACKEND=local`、専用DATA_DIR、AI_ENABLED=1、AI_CONFIG未指定で外部送信0。実画像ファイルは作らないため画像配信の受入には使わない。意味検索/遅延Provider/画像E2Eは合成RGBとloopback stub付きのunit executable `production_worker_connects_images_local_cycles_e5_e6_and_local_advances_during_slow_provider`、性能は`synthetic_6500x768_compute_releases_writer_and_measures_save`を使う。全同時刻・同方向は最悪cosineケースではない。多数の独立窓は`local_many_small_components_are_bounded_and_progress_without_rescanning_on_writer_per_group`で16/16/8/0を確認する。

### T4.5b最終検証記録

- `cargo fmt --all --check`、`cargo clippy --workspace --all-targets -- -D warnings`、`git diff --check`: 成功。
- 最終 `RUST_TEST_THREADS=4 cargo test --workspace`: **28 result group、1225 passed / 0 failed / 2 ignored / 0 measured / 0 filtered**。内訳lib1033成功/既存ignore1、main0、integration188成功、doctest4成功/既存ignore1。T4.5bの追加15試験を含み、条件付き省略0。Python/Pillow/Nodeの既存回帰・Python切り戻しも実行した。
- 本番factoryと実Worker spawnから、合成RGB2画像→loopback Embedding→active公開→ローカル周期→E5/E6保存済み推薦・意味検索を確認。空queryの追加POST0。追加画像POSTを同期ゲートで保持し、別スレッドのローカル順位更新、feed/E5の進行と停止/joinを確認した。
- 否定対照2件: pending_savesの全member照合を外すと破損修復試験が失敗、expected_active CASを外すと新processor確定後の旧batch PlanがStoredとなって試験が失敗した。両方とも意図したassert/exit101を確認し、finallyで復元。復元後の全suiteは上記の成功数。故障注入を通常suiteの製品失敗へ加算しない。
- 初回loopback bindはsandboxのPermissionDeniedで失敗し、socket権限付きで成功。移行3表/管理の実抑止数と追加キー/未指定Providerでのローカル起動に関する旧fixture期待値を更新した。最終通常suiteに失敗は無い。実写真/実DB/実weight/鍵/本番/LAN/SSH/Cloud/lease操作は未使用。
- `cargo build --workspace --release --target x86_64-unknown-linux-musl`: 成功。`file`: static-pie linked、`readelf -l/-d`: INTERP/NEEDED無し。`--version`: geoframe 0.1.0。19,933,224 bytes、SHA-256 `c27038073bb371bb49be4a795056bdd36aa4e9844fe2b0b5f049cf935a4676a7`。
- musl release unit executableの既存6500性能fixtureを実周期保存へ拡張し、対象を1回測定（1成功、ignore0）。この開発ホスト、6500写真/6500 SHA×768、全同時刻・同方向、重み無し、外部通信無し。実周期はsnapshot **138.881ms**、モデル読取込みcompute **77.592ms**、writer待ち **28.77µs**、writer内CAS/batch-save **198.391ms**、周期全体 **425.234ms**、選択/保存1成分・6500構成員。期限/時間の性能assertは置いていない。同時に通常回帰試験が稼働していた値で、専有ホストの性能保証ではない。
- 同fixtureの事前分離snapshot/computeは148.236ms/73.149ms。RSSは準備後→分離snapshot後→分離compute後→周期保存/読取後が **11964→72440→75020→78444KiB**。事前snapshot/Plan、allocator/SQLite cacheを含む各時点の値で、周期単独の増分・peakではない。computeを同期ゲートで保持した間の別writer更新も成功した。多数小成分の試験は40成分を16/16/8/0保存、期限切れwriter依頼の後着保存0を確認した。

独立Solレビュー、実LAN/LXC、実weightでの品質評価、任意Gemini判定と有償抑止への適用、P4全体security-reviewは未実施として親へ引き継ぐ。成分計算の最悪O(n²d)、大きい単一成分の原子的保存、協調停止、履歴を自動GCしない点は既知の境界。今回の合成runtime試験でP4全体完了・P5移行可能とはしない。
