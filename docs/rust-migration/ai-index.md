# AI の検索索引・空間・旧選別（P4 / T4.3）

空間別のベクトル保存、検索空間の切替、旧選別の安全化、その境界（後続 T4.4・T4.5 への接続）。 T4.5aのpure同一場面・順位・世代別repository APIは[ai-scenes.md](ai-scenes.md)を参照（周期/選別接続はT4.5b）。設計は [p4-design.md](p4-design.md) §6・§7。実装は `crates/geoframe/src/ai/{space,index,activation,search}.rs` と `src/selection.rs`、試験は `ai/tests_index.rs`・`tests/legacy_selection.rs`・`tests/ai_index_python_rollback.rs`・`tests/ingest_cli.rs`。

T4.3の索引基盤に、[T4.4aの固定契約アダプターと画像ワーカー](ai-embedding.md)を接続した。旧ベクトルは輸入しない。実APIのLAN/LXC確認は親担当で、ローカル試験は合成DB/画像とloopbackだけ。

## 1. SpaceIdentity（同じ空間の条件）

画像と検索語のベクトルを同じ空間として扱うのは、次の**全項目が一致**するときだけ（名前や次元だけでは一致しない。版は補わない。空・不正な識別子は拒否）。

| 項目 | 意味 |
|---|---|
| `space_id` | この DB 内の空間の名前（`ai_embedding_spaces` の主キー）。再利用して別の識別を黙って上書きしない |
| `provider_space_id` | **Provider が保証する**画像・文に共通の空間 ID |
| `model_id`・`revision` | モデルと版 |
| `image_preprocess`・`text_preprocess` | 画像変換・検索語 prompt の版 |
| `dimension` | 1〜4096 |
| `normalization` | `l2`（単位ベクトルで保存）／`none`（受け取った値のまま） |

- 設定の `embedding_space`（`ai::settings::EmbeddingSpace`）は**形式を検査しただけのメタデータ**で、Provider の応答または照合済みの既知固定契約による証明ではない。`SpaceIdentity::from_declared` は、Provider が証明した `provider_space_id` を別に要る。
- 旧 `photo_ai_features.embedding_model` などの文字列から識別は作れない。**旧ベクトルは `legacy-unverified` のまま残すだけ**で、新しい空間へ輸入せず、旧モデルと同名の設定でも新しい検索語ベクトルと比べない。旧空間を検証済みと宣言する経路は、モデル・重み・前処理の版と実測照合がそろう場合に限る（今回は作らない）。
- `register_space` は、同じ `space_id` を別の識別で再登録すると `SpaceIdConflict`、同じ識別の組を別の `space_id` で登録すると `IdentityConflict`（表の UNIQUE 制約も同じ組を守る）。

## 2. 追加スキーマ

`ai::migrate`（INGEST=1 のガード・writer 移行の後だけ）が足す。`REQUIRED_SCHEMA`・Python のスキーマの再現には入れない。

| 表 | 内容 |
|---|---|
| `ai_embedding_spaces` | 上の識別。`space_id` PK、識別の組に UNIQUE、`dimension` 1〜4096、`normalization` は `l2`/`none` |
| `ai_embedding_vectors` | `(space_id, input_sha256)` PK、`sent_sha256`（実送信した再エンコード JPEG の SHA。監査用）、`dimension`、`vector`（float32 リトルエンディアン。`length = dimension*4` の CHECK）、`created_at`。索引 `idx_ai_embedding_vectors_sha` |

- `input_sha256` は品質特徴量が解析した元 derivative の SHA（`photo_quality_features.content_sha256`）。旧 `photo_embeddings`・`photo_ai_features` には**書かない**。
- 保存: 次元・NaN/Infinity・ゼロノルムを拒否。L2 は最大要素で割ってから f64 で足すので、極端な大きさでもオーバーフロー・アンダーフローしない。同じ `(space_id, input_sha256)` が有効なら最初のベクトルを残す（`Existing`）。壊れた既存の行は置き換える（`Repaired`）。writer 上の短い 1 トランザクションで、検査・正規化は writer の外（`PreparedVector::new`）で先にできる。
- 読み取り: BLOB も型（BLOB）・`dimension` の列・長さ・有限数・ゼロでないノルム・ノルム（L2 は単位ベクトル）・空間の識別で検査し（`space::decode`）、壊れた行は**読み飛ばして数える**（全応答の失敗やパニックにしない）。**公開・進捗・`status`・検索は同じ 1 つの判定（`index::scan`）から数える**ので、壊れた行（NaN/Infinity/ゼロ/単位でない L2/短い BLOB/BLOB でない型）は成功に数えず、`pending`（現在のジョブが `failed` なら `failed_excluded`）として再生成の対象になり、`status.corrupt` が実件数になる（`store_vector` は壊れた既存の行を置き換える）。検査は writer の 1 トランザクションの中でローカルに行う（外部通信・重い変換は持ち込まない）。
- 旧 DB（`INGEST=0`）はこれらの表が無いまま読める（`index::status` は `NoTables`）。`--dry-run` の「作る表・索引」は、AI の表の分として 3 件（2 表・1 索引）増える（T4.3時点6 → 9、T4.4aレビュー修正のlineage表を含めると10）。既存の表・列・行は変えない。

## 3. 索引の顔ぶれ（固定の全 index 表は作らない）

今の対象は `photos` ⋈ `photo_quality_features`（`display`・`md5`・`mtime` が現在の写真と合い、`error=''`・`content_sha256` あり）で、`display` が空でない写真。

| 変化 | 扱い |
|---|---|
| 更新（`md5`・`mtime`）・`display` の変化 | 世代が合わなくなり対象から外れる（古い SHA のベクトルは参照されない） |
| 回転 | `display` の名前が変わるので同じ（品質テーブルに無い `rotation` 列は仮定しない） |
| 削除 | 結合から消える |
| 同じ内容の複製 | `content_sha256` ごとに 1 件（代表は最小の写真 ID。旧 `ai_pipeline` の正本と同じ）。検索結果の `photo_ids` に全て持つ。代表が削除・更新されても、同じ内容の写真が残っていれば SHA は対象のまま（代表はその時点の最小の ID に変わる） |
| `hidden`・`favorite` | 対象を変えない（検索側が `include_hidden` などで絞る。旧 E6 と同じ） |
| 新しい写真 | ベクトルが保存された時点で有効な結合により対象になる（スナップショットなし） |

## 4. 切替（`settings.ai_active_space`）

1. 構築の開始時に `snapshot_targets` が今の対象と各世代を候補として持つ。候補は `content_sha256` ごとで、**その SHA の構成員すべて**（写真 ID・`display`・`md5`・`mtime`・`rotation`）を持つ（`BuildSnapshot::targets()` → `SnapshotTarget::members()`）。
2. T4.4aワーカーが `ShaState::job_key(identity)`（targetは空間/SHA、input_hashはSHAと全構成員世代のhash、processor_versionは識別全項目）で積む。URL/ポリシー/閾値はキーに入れない。`PreparedVector` をwriter外で作り、`jobs::complete` 内でvector保存とjob成功を原子的に行う。失敗履歴の継承・破損修復は [ai-embedding.md](ai-embedding.md) を参照。
3. `try_activate(conn, snapshot, identity, min_coverage)` を writer 上で呼ぶ。**1 つの書き込みトランザクションの中で**候補を再検査する。SHA は、**構築開始時の構成員のうち 1 枚でも今も同じ世代の対象**ならまだ有効（代表が消えても同じ内容の写真が残れば分母・待ちに残る）で、構成員がすべて変更・削除・回転された SHA だけを除く（`stale`）。後から同じ SHA の写真が追加されても、元の構成員が全滅していれば有効にならない（追加分は差分として後続で処理する）。件数はすべて SHA 単位。残り（`valid`）のうち、ベクトルあり=`indexed`、現在のキー（`kind`・`target_key`・`input_hash`・`processor_version`）のジョブが `failed`=`failed_excluded`、それ以外=`pending`。

| 結果 | 条件 | ポインタ |
|---|---|---|
| `Waiting` | `pending` が 1 件でもある（pending/running/deferred/未登録/obsolete） | 変えない |
| `NoVectors` | 成功したベクトルが 0 件（全件失敗を含む） | 変えない |
| `BelowThreshold` | `indexed / valid < min_coverage`（`valid` は失敗を含む） | 変えない |
| `Published` | 上のどれでもない | `settings.ai_active_space` を原子的に更新 |

- 100 件のうち 1 件だけ `failed` なら 99/100 で公開できる。手動再試行が成功してベクトルが保存されると、再切替なしで有効な結合により検索に加わる。
- 他の空間・古い処理版・別の SHA の失敗、`failed` 以外の状態では対象を除かない。構築後に追加された写真は候補に無いので公開を待たせない。
- `min_coverage` は引数（有限の 0〜1。それ以外は `InvalidThreshold`）。既定は `DEFAULT_ACTIVATION_MIN_COVERAGE = 0.95`。**今回は設定 JSON に置かない**（後続が `GEOFRAME_AI_CONFIG` に独立の任意キーを足す場合は、有限・0〜1 の検査を `activation::valid_coverage` に合わせ、不正なら AI の設定不正として AI だけを無効にする。閾値は結果の同一性・設定ハッシュに入れない）。
- `deactivate`（CLI `ai-jobs deactivate-space`）は `ai_active_space` を消すだけ。旧結果・空間・ベクトル・ジョブは**消さない**（GC は作らない）。

## 5. 読み取り（後続の E5/E6 が使う型付き境界）

- `index::status(conn)` → `IndexStatus`（`ActiveState`＝`NoTables`/`Unset`/`Active`/`Unusable`、`Progress`＝`eligible`/`indexed`/`pending`/`failed_excluded`＋理由コード別件数（`failed_reasons: Vec<(&'static str, i64)>`。保存された `last_error_code` は固定の `ErrorCode` か `other` に直す（`index::reason_code`。`ai-jobs status` の理由の集計と同じ規則）ので、未知の文字列・改行は出力に出ない。未知でも `failed` は失敗として数え、`pending` に混ぜない）、`other_space_shas`・`legacy_shas`・`other_shas`（空間間の同じ SHA は重複計数しない）、`spaces`、`corrupt`）。件数だけで、写真の値・SHA・ベクトルは持たない。E5 の `pipeline`・`embedding_model` 等はこの値から作る（[T4.4b実装](ai-search.md)）。
- `index::progress(conn, &identity)` は構築中（まだ active でない）空間の進み具合。
- `search::load_active_index(conn)` → `Loaded { status, index: Option<ActiveIndex> }`。ポインタ・識別・対象・ベクトル・状態を **1 つの読み取りトランザクション**で読むので、並行する切替の前後が混ざらない。ポインタが無い・未登録・表が無いなら `index: None`（旧結果参照。意味検索は 503 のまま）。
- 検索は `QueryVector::new(&identity, &values)`（次元・有限・非ゼロを検査）→ `ActiveIndex::search(&query, limit)`。識別が索引と**全項目**同じでなければ `IdentityMismatch`。全項目を線形に走るコサイン類似度（6,500×768 の規模で足りる想定。ANN・外部ベクトル DB は無し）。他の空間・旧結果のベクトルは読まない（混在 0）。

## 6. 旧選別の安全化（今回の見え方の意図した変更）

旧 `AI_VISIBLE` はメンバー表の行があるだけで推薦外の写真を隠していた。`crate::selection` は次を**全て**満たすときだけ抑止する（旧 AI 表・列の意味は不変。読むだけ）。フィード（`photos::list_photos`）と管理画面の `groups`・`group_pending`・`suppressed`（`selection::admin_counts`）が同じ SQL を共有する。

1. **有効なメンバー**: `photo_ai_features` の `display`・`source_md5`・`source_mtime` が今の `photos` と合い、`content_sha256` が今の品質特徴量（世代が合い `error=''`）と一致。
2. グループが `judged`・`same_scene=1` で、そのメンバーが推薦でない。
3. **同じグループに、有効で表示可能な推薦写真が 1 枚以上ある**（`hidden=0`・`display` が空でない・`lat`/`lon` あり・品質の `exact` で推薦外でない）。代表が品質側で非表示でも場面全体を消さない。

- `favorite` は従来どおり抑止を免れる。未判定・同一場面でないグループ、`hidden` 等の他の規則は変えない。管理画面の `suppressed` は従来どおり `favorite` を含めて数える（抑止される有効なメンバーの数）。`groups`・`group_pending` は有効なメンバーを 1 件以上持つグループだけ数える。
- 代表の削除・更新・回転・hidden 化・位置なし後は、残りが表示される。**検索空間の切替・新しいベクトル・新しい同一場面の候補は、この抑止集合を変えない**（旧表の旧選別だけが根拠）。
- `ai_*` の列（`ai_group` など）の出し方は従来のまま（行の重なりも同じ）。使う表は全て `REQUIRED_SCHEMA` にあり、AI の追加表が無い旧 DB（INGEST=0）でも動く。6,500 枚規模の合成 DB でフィードは約 14 ms。
- **導入前後の差**（同じ複製 DB のフィード件数・suppressed）は、世代が合わない旧メンバー（更新・回転・削除後の旧特徴量や、特徴量の行が無いメンバー）と、代表が表示できない場面で出る。差は対象の不整合で説明できるものだけ（検証は LXC の複製で記録）。

## 7. CLI

- `geoframe ai-jobs status`: 読み取りだけ。検索空間の有無・登録数、対象/保存済み/待ち/失敗で除外/壊れの件数、除外の理由コード別件数、他の空間・旧結果のベクトル数（空間 ID・モデル・SHA は出さない）。表が無い旧 DB でも書き込み 0。
- `geoframe ai-jobs deactivate-space`: `ai_active_space` を外し旧結果参照へ戻す。`GEOFRAME_INGEST=1`・検証環境のガード・`.ingest.lock`・writer を通る（サービス稼働中は終了コード 1、INGEST 無し・ガード違反は 2）。旧結果・空間・ベクトル・ジョブは消さない。Python→Rust で再切替したら reconcile が追加・変更分を拾う（新結果表を消す必要はない）。

## 8. Python への切り戻し

Python は追加表と `ai_active_space` を使わず旧表を読む。Rust の追加表・ポインタがある DB を現行 Python の `api_photos`・`api_ai_status`・`_remove_noncanonical_embeddings`・`_load_photos`・`_valid_rows` に渡しても、手動の位置・お気に入り・非表示・回転、旧の有償結果、Rust の追加表・ポインタは変わらず、世代の合う旧選別ではフィードの顔ぶれが Rust と一致する（`tests/ai_index_python_rollback.rs`。Python が無い環境では省く）。

## 9. 後続への境界と限界

- **T4.4a実装済み**: [固定契約アダプター・世代付き画像ジョブ・自動切替](ai-embedding.md)。SpaceIdentityは明示した固定契約と実model/能力を照合する。**T4.4b**: E5は `index::status`、E6は共有clientのquery＋`load_active_index`＋`QueryVector`へ接続する。
- **T4.5**: 新しい同一場面グループ・代表。**旧選別と部分的に重なる新集合は旧選別を自動で置き換えない**（旧は `selection` の有効判定のまま保持。新しい結果は別表に持ち、重なるものは管理確認）。入力集合・世代が一致する選別単位か、有効な旧選別と重ならない新しい集合だけ新判定を使える。旧の写真別判定（品質・`keep_score` 等）は世代が一致すれば空間独立で使える。
- T4.4aで固定契約の画像Embedding・差分修復・`ai_active_space`の自動切替ワーカーを実装済み。T4.4bで[E5/E6 HTTP・query LRU/レート制限](ai-search.md)を接続済み。残る未実装は設定JSONの閾値キー、T4.5の新しい同一場面の表（`ai_scene_*`）。実LAN/LXCでのRust経由受入は親担当で未確認。旧Q8_0/Geminiベクトルは輸入しない。lineage追加表とchunk/公開照会の最終方式は [ai-embedding.md](ai-embedding.md) を参照。
