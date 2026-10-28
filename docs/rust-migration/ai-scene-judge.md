# 任意のGemini比較（T4.5c）

同一場面（T4.5a/bの現世代`Group`）の代表を、用途の許可がある場合に限ってGeminiで比較する。P4-AC2/3/4/5/8/9/10aの範囲。要求は[p4-prd.md](p4-prd.md) R7、設計は[p4-design.md](p4-design.md) §6.3・§7、場面側の前提は[ai-scenes.md](ai-scenes.md)、クラウド通信の境界は[ai-cloud.md](ai-cloud.md)。位置候補の履歴・CAS・Python互換の手本は[ai-location-suggest.md](ai-location-suggest.md)。実装は`ai/scene_judge.rs`、`ai/scene_judge/{answer,repository,runtime}.rs`。実クラウド・実鍵・実写真・本番のDB/サービスは使わず、試験の通信は全てloopbackのスタブ。

## 送る条件

| 対象 | 扱い |
| --- | --- |
| LocalRankedで旧選別と重ならない（Disjoint）、shortlist 2〜6枚、hiddenを含まない | **送る**（比較の対象） |
| Exact（旧Pythonの有償判定が有効） | 送らない。旧判定は`paid_reuse_verified()==false`だが、有効な旧選別が既にあるため再課金しない。置換候補ではあるが、再比較は管理者の判断（下記） |
| Partial（ReviewOverlap）・Waiting・単独 | 送らない。Partialは管理確認のまま |
| hiddenの写真を含むshortlist | `include_hidden=false`の用途では送らない（送る直前・応答後・保存時にも検査） |

handlerは`photo_judge`が`provider=cloud`・`allow_cloud=true`・日の上限あり・クラウド接続（`CloudClients`）・場面計算の有効が揃うときだけ登録される（`scene_judge::enabled`）。揃わなければ**handlerも結果表も作らない**ので、ジョブは積まれず、トークン取得・DNS・HTTPは0。ローカルのグループ・代表選択（T4.5b）は別スレッドで動き続け、Providerの障害にもクラウドの許可にも依存しない。設定を変えて再起動した場合も、未送信ジョブは`run_checked`が送信ごと（token取得前・各POST予約後・401更新後）に現在の許可を検査するので、許可が外れていれば送らず、戻せば同じジョブが進む（`config_changes_after_enqueue_*`）。

## 同一性・再利用

- 回答の鍵`input_key` = SHA256(prompt版・モデル・処理版・**送信集合の（写真ID, 入力世代）をID順に並べたもの**)。グループの結果ID・順位・Overlap・URL・timeout・他用途の許可は含めない。同じ画像・世代・prompt・モデルなら、別のグループ・再起動・設定変更の後でも再送しない。モデル/prompt版/処理版/世代が違えば別の由来として再比較し、過去の履歴は残す。
- ジョブの`target_key`は送信する写真IDの集合、`input_hash`は`input_key`、`processor_version`はモデル・prompt版・送信方式版・画像前処理版から導く。世代だけが変わった古いジョブは`enqueue`で無効になる。
- 履歴表`ai_scene_judgments`（1表・不変）は`input_key`を主キーに、manifest（送信順の写真ID・世代・読み込んだ元ファイルのSHA・実際に送ったJPEGのSHA・モデル・prompt版）、正規化済みverdict、試行回数、整合hashを持つ。読み戻しは全項目を検査し、破損は`Unverified`として**送らず・再課金もしない**（表が無い場合も同じ）。
- Pythonが保存した旧判定（`photo_ai_*`）は読まず・転用しない。旧判定の完全なprompt・回転・送信集合の由来を証明できないため。Exactのグループは旧判定がそのまま有効、DisjointのグループはPython側にも有効な判定が無い。

## 画像・上限・予算

画像は`prepare_embedding_rgb`（EXIF向き適用・長辺768px・1.6MP）→RGBから`PreparedImage`を新規生成（JPEG q82）。圧縮済みバイト・EXIF・GPS・ファイル名は入口が無く構造的に送れない。送る前に、読んだ元ファイルのSHAが品質解析済みの`content_sha256`と一致することを確かめる。shortlist上限（設定の2〜6）がそのまま送信枚数の上限で、リクエスト全体はbase64込み8MiB（既存の`GenerateRequest`）。

毎POSTの前の予算は既存の`RunCtx::before_send_until`（日・周期の予約。予約は戻さない）、失敗は既存のworker分類（応答不正は`BadResponse`で5回で`failed`、停止・認証・429・予算切れは失敗に数えず待機）。送信の応答は`JsonReply`から`Verdict::parse`で検査する（全写真番号の揃い・列挙・0〜100の範囲・型。範囲外の番号は無視、重複・欠落は拒否）。同一場面なのに推薦が0枚なら`keep_score`最大の1枚を推薦にする（Pythonと同じ）。

## 適用（CAS）

応答の受信後（追加の通信なし）に、現在性をもう一度確かめて、`jobs::complete`の短いwriter transactionへ渡す。tx内（`persist`）で次を同時に照合し、一つでも外れれば何も書かずジョブを`obsolete`にする（有償応答は破棄）。

1. claim tokenと起動IDが有効（古いtokenの遅延応答は`Rejected`）。
2. ジョブの鍵が計画と一致。
3. `Prepared::load_in`（現空間・現処理版・現入力世代・参照）で、**同じshortlist（写真IDと世代）を持つLocalRankedのExactまたはDisjointのグループ**が今も在る。Partialになった・Waitingに戻った・shortlistが変わった・写真が更新/回転/削除された場合は採用しない。
4. hiddenが許可内。
5. 同じ`input_key`の回答が未保存（同内容なら成功、異なる内容は書き換えない）。

応答待ちの間にグループの結果ID（shortlist以外の構成員の追加など）だけが変わった場合は、送った画像と世代が同じなので採用する。writer内で画像・ファイル・通信は行わない（応答後のファイルSHA再確認はwriterの外）。

**採用の効果は表示と再利用だけ**。E6の`kind:"scene"`（Disjoint）のグループに、`summary`・`same_scene`・`judge_model`・`judge_source:"gemini"`、各写真に`gemini_recommended`・`gemini_keep_score`を付ける（現世代で、設定中の`photo_judge`モデルの回答のみ。既存キーの意味・並びは変えない）。管理STATUSの`ai_scenes.paid_judgments`は履歴の総数。**フィードの抑止・旧`photo_ai_*`表・`photos`の人手の列は一切変えない**。未判定・Waiting・Partialも抑止の根拠にしない。

E6の表示用の回答は、場面を読んだ読取transactionを閉じたあとに主キー1件ずつ読む（返す場面の数だけ）。履歴は追記専用で、鍵が場面の構成員・世代そのものから導かれるため、別snapshotで読んでも古い入力の回答を現在の場面に付けることはない（`applies_to`が現在のshortlistの写真と世代を再照合する）。

## 表・移行・停止

追加は1表のみ（`ai_scene_judgments`、WITHOUT ROWID、明示索引0・列追加0）。`ai::pending_active/migrate_active`の引数に`judge: bool`を足し、`scene_judge::enabled`が真のときだけ起動のINGEST guard後に作る。有効INGESTで許可が揃う場合の不足objectは+1（SAMPLE相当の許可なしでは増分0）。INGEST=0・許可なし・ローカル設定では作らず、読み取りも追加しない。全部入りの`pending/migrate`は1表を含む。

外部I/O（画像読取・縮小・通信）はwriter・DB transactionの外のAI専用スレッド。DB transactionは短い`unchecked_transaction`（読み）と`jobs::complete`だけで、通信の間は保持しない。停止（SIGTERM・LockHold）は既存のworker共通期限に従う: claim前に停止なら送信0、送信中に届いた有効な応答はtoken有効の間に確定される。

## Python互換・意図差

- 切り戻し: 旧AI表（`photo_ai_*`）には書かない。追加表のあるDBでPythonのfeed/status/`_rebuild_groups`が動き、人手の列・新しい表（`ai_scene_*`・`ai_scene_judgments`）が変化しないことを`the_schema_is_additive_and_python_rollback_*`で確認する（Python3のみ使用、Gemini/モデル呼出しなし）。PythonのAIサイクル全体（Pillow・モデル）はLXCで実行する。
- prompt本文・`JUDGE_VERSION`・`MAX_JUDGE_ATTEMPTS`・schemaはPythonのAST定数と一致（試験）。生成設定は`temperature 0.1`・`responseMimeType`・`responseJsonSchema`（T4.6a）。
- 意図差: (1) 画像の前に「Image n」ラベルのtextが入る（T4.6aの`photo_judge`の形。Pythonの`_judge_group`は画像のみ）。(2) 提示順は常に順位順（Pythonは`AI_JUDGE_SHUFFLE_RATE`既定0.2で並べ替える。MT19937の再現は行わない）。(3) 結果は新表に保存し、旧`photo_ai_groups/features`へ投影しない。(4) 送信集合の鍵に世代を含み、Pythonのgroup_id（shortlistのcontent SHA）とは一致しない。

## 管理者が判断する点

1. **Disjointの比較結果でフィードの抑止を有効にするか**（既定は無効）。有効にするには、検証済みの結果を旧`photo_ai_groups/members/features`へPython互換の形で投影し、既存の選別SQL（代表が存在する場合だけ非推薦を抑止、favorite免除）に載せる処理が要る。投影は旧表への書込みとPython側の再構築との競合を伴うため、今回は実装していない。
2. **Exact（旧Python判定が有効）の再比較**。既定は再課金しない。再比較には旧判定の由来の根拠（prompt・回転・送信集合）か、費用を許容する判断が要る。
3. **Partialの扱い**。管理確認のまま。解消方針（旧選別の置換、片方の無効化）は未決定。
4. **shuffle率・Image nラベル**。Pythonとの完全一致が要るなら別途。
5. 実クラウド送信の許可・費用・上限値（`daily_request_limit`・`GEOFRAME_AI_JUDGE_PER_CYCLE`）はこの実装で追加していない。

## 試験

`ai::scene_judge`の専用20件: domain/固定定数/応答検査/保存の読み戻し/鍵の性質/request形/migration（`tests.rs`）、loopbackスタブ+合成写真+実worker/Core（`runtime/tests.rs`）。未許可の4通り（許可なし・日の上限0・用途なし・ローカル設定）でスタブ受信0・表なし・ローカルのグループは作られる、許可時のEXIF/GPS(canary)除去・6枚上限・生成設定・一次列不変、再利用（周期・再起動・他用途/上限の設定変更で再送0、モデル変更で再比較）、日の予算1と失敗上限5（POST数と`ai_request_usage`）、Exact/Partial/Waiting/hidden/単独は送らない、応答中の回転・削除・hidden・md5・ファイル差替えとPartial化は不採用、トークン段階の失敗→許可を外す→戻す、停止（claim前0・送信中の応答は確定）、破損履歴は再課金せず、Python切り戻し、E6表示と抑止なし。
