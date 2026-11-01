# 運用コンソール（C） PRD

作成日: 2026-11-01。状態: 下書き（レビュー前）。設計: [design.md](design.md)。

## 背景と目的

管理画面の整え直し（[A](../admin-rework/prd.md)）と写真ライブラリ・選別（[B](../admin-library/prd.md)）は、
運用操作（同期・品質解析・AI ジョブの再試行・保留の解除）を画面から行うことを「やらないこと」にした。
今は次の手順で、**サービスを止めて CLI を動かす**しかない（[deploy/README.md](../../deploy/README.md) の
362〜367 行、[deploy/geoframe-admin.sh](../../deploy/geoframe-admin.sh) の冒頭）。

- 同期をすぐ回したい（写真を Drive に入れた直後など）: 次の周期（`GEOFRAME_DRIVE_POLL_MIN`）を待つか、
  `sudo systemctl stop geoframe-rs && sudo deploy/geoframe-admin.sh sync && sudo systemctl start geoframe-rs`。
- 品質解析を一括で回したい: 同じ手順で `quality-scan`。
- AI ジョブの失敗を再検査したい: 同じ手順で `ai-jobs retry`。
- AI の Provider・用途の停止（認証失敗・モデル不在）を直したあとの解除: 同じ手順で `ai-jobs release-pause`。
- Drive の大量削除の保留（消える行が 50 件超かつ 20% 超、または一覧が空）を意図した削除として通す:
  同じ手順で `sync --allow-mass-delete`。状況タブは件数とコマンドを示すだけ（2026-10-25 の決定）。

止める・動かす・起動し直すのあいだ miharashi の表示とサムネイルが止まり、root で直接動かす事故の余地もある。
本 PRD は、これらの操作を稼働中の `serve` の中で行う「運用」タブ（C）の範囲を定める。

大量削除の保留について、2026-10-25 のレビュー対応（[server.md §14.13](../rust-migration/server.md)）で「外すのは
`geoframe sync --allow-mass-delete` の 1 回だけ。環境変数・設定・unit では外せない」「画面からは解除できない」と決めた
（ADR は無く、server.md と deploy/README.md の記述）。**2026-11-01 にユーザーが、件数を見せて確かめる形なら画面から
解除してよいと決め直した**（この PRD の R6・R12）。「環境変数・設定・unit では外せない（定期サイクルは常に保留する）」は
変えない。

## 使う人と場面

- 作者 1 人が、家庭 LAN 内のブラウザから使う。認証は無い（[ADR 0008](../decisions/0008-photo-platform-and-miharashi-display.md)。
  外部公開時は Cloudflare Access）。
- 場面: (a) 写真を入れた直後に同期を回して、状況タブで結果を見る (b) 選別タブの前に品質解析を一括で回す
  (c) 状況タブに失敗ジョブ・Provider 停止が出たとき、原因を直してから再試行・解除する (d) 状況タブに大量削除の
  保留が出たとき、意図した削除なら件数を確かめて通す (e) 直近の操作とサイクルがどう終わったかを見る。

## ゴール / やらないこと

ゴール:

- 管理画面に 4 つ目の「運用」タブを置き、5 つの操作（同期をいま回す・品質解析を一括で回す・AI ジョブの再試行・
  AI の停止の解除・Drive の大量削除の保留の解除）を、サービスを止めずに画面から行える。
- 操作の受付・実行中・結果（段ごとの完了/省略/失敗、再試行の件数）と、定期サイクルの直近の結果を画面で見られる。
- 大量削除の解除は、保留中の件数・理由・開始時刻を見せ、画面が見た保留とサーバーの今の保留が一致するときだけ通す。
  通したあとの実行でも、確かめた件数を超える削除はしない。
- CLI（`sync`・`quality-scan`・`ai-jobs`）と `geoframe-admin.sh` は、`serve` が動いていないときの手段として残す。
  文書の「サービスを止めてから」「画面からは解除できない」の記述を、この機能に合わせて改める。

やらないこと:

- 実行中の操作の中断（停止の合図は `serve` の終了だけ）。
- 操作の履歴の永続化（プロセスの中だけ。再起動で消える。大量削除の保留の記憶と同じ扱い）。
- `organize`（ローカル写真の整理）、`ai-jobs deactivate-space`、設定の変更・再読込、サービスの再起動。
- 同期サイクルの周期（`GEOFRAME_DRIVE_POLL_MIN`）や段の並びの変更。段を 1 つずつ選んで回すこと（品質解析だけを特別扱いする）。
- `/api/admin/status`・`/api/admin/photos`・`/api/v1/photos`・`/derivatives/` の応答の形の変更。
- 認証の追加。フレームワークやビルド工程の導入（素の JavaScript を続ける）。
- 本番への反映（マージまで。配布はユーザーの立ち会い）。

## 要求

### 画面

- R1（必須）: ヘッダに「運用」タブを足す（状況 / 写真 / ライブラリ / 選別 / 設定 / 運用）。既存のタブの中身と
  `showTab` の規則は変えない。
- R2（必須）: 運用タブは、取り込みが有効（`GEOFRAME_INGEST=1`）のときだけ操作を出す。無効のときは「取り込みが無効なので
  運用操作はできない（読み取り専用の起動）」と出し、ボタンは置かない。
- R3（必須）: 操作は 5 つ。「同期をいま回す」「品質解析を一括で回す」「AI ジョブの再試行（種類ごと / すべて）」
  「AI の停止の解除（用途ごと / すべて）」「Drive の大量削除の保留を解除」。ボタンの横に、何が起きるかの 1 行説明を置く。
- R4（必須）: 同期・品質解析・保留の解除は「受け付けた → 実行中 → 終了」の 3 段階が見える。実行中は同じ種類の
  ボタンを無効にし、終了すると段ごとの結果（完了・省略（理由）・失敗・パニック・未実行）と件数を出す。
  受け付けた直後と終了時は読み直し、表示中は数秒ごとに読み直す。タブを離れると読み直しを止める。
- R5（必須）: 定期サイクルの直近の結果（開始・終了時刻、段ごとの結果）と、次のサイクルの予定時刻、実行中なら
  その旨を出す。直近の操作の履歴（最新 10 件）を新しい順に出す。
- R6（必須）: 大量削除の保留の解除は 2 段階にする。保留が無ければボタンを出さない。あれば、保留中の件数・理由・
  Drive の行数・保留の開始時刻を出し、「解除する」を押すと確認の段が開き、「N 件の削除を許可」で送る。確認の段には
  次を明記する: 「Drive の一覧を取り直し、消える行が N 件以内なら削除する。N 件を超えていれば削除せず再び保留する」
  「消えるのは Drive の写真のカタログの行と表示用画像で、その写真に付けた位置（手で直したものを含む）・★・除外も一緒に消える。
  原本（Drive 側）には触れない。元に戻すにはバックアップから」。
  サーバーが 409（保留が変わった・保留が無い）を返したら、確認を閉じて読み直す。
- R7（必須）: AI の停止の解除の横に「解除すると、認証や設定が直っていれば次の周期からクラウドへ送り直す（料金が
  かかる）。直っていなければ同じ理由で再び止まる。一時待機（`ai_waits`）は残るので、期限までは送らない」を出す。
- R8（必須）: 状況タブの大量削除の注意と AI 運用の案内の文言（「この画面からは解除できません」「サービスを止めて
  `geoframe ai-jobs …` で」）を「運用タブから」に改める。`web/tests/admin_ai_ops_render.mjs` の検証内容は変えない
  （文言が検証に含まれていれば、検証の期待値だけを新しい文言に合わせる）。
- R9（あると良い）: 状況タブの「要対応」（A の R5）に、保留の解除・失敗ジョブ・停止がある旨の行を足し、押すと
  運用タブへ移る。

### サーバー

- R10（必須）: 同期をいま回す・品質解析の一括・保留の解除は、**定期サイクルと同じ 1 本のスレッド**で順に実行する
  （並行に走らせない）。要求は 1 つだけ待てる。定期サイクルの実行中に来た要求はその直後に走り、待っている要求がある
  ときの新しい要求は 409 で断る。
- R11（必須）: 同期をいま回す＝定期サイクルと同じ段の並び（`default_stages`）を 1 回。品質解析の一括＝
  CLI `quality-scan` と同じ（未解析が無くなるまで解析し、再構築）。保留の解除＝`index_drive` の段だけを、確かめた
  件数を上限にした「削除を許す」設定で 1 回。
- R12（必須）: 保留の解除の POST は、画面が見た保留の印（`token`。件数・理由・Drive の行数・開始時刻から作る）を
  受け取り、サーバーの今の保留と違えば何もせず 409 `hold_changed`、保留が無ければ 409 `no_hold`。実行では
  Drive の一覧を取り直して計画を作り、消える行が印の件数を超えていれば削除せず再び保留する（結果に `exceeded` と出す）。
  件数以内なら削除して、以後の段は定期サイクルに任せる。
- R13（必須）: AI ジョブの再試行と停止の解除は、CLI の `ai-jobs retry`・`release-pause` と同じ関数（`ai::ops::retry`・
  `release_pause`）を `serve` の writer で呼び、応答で件数（再登録・不要・済み・検査不可・無効）または消した件数を返す。
  外部へは送らない（CLI と同じ）。背景の AI ワーカーが動いていても安全に行える（`failed` の行だけを触る）。
- R14（必須）: 進み具合を返す `GET /api/admin/ops` を足す。取り込みの有無、実行中・待ちの要求、定期サイクルの直近の
  結果と次の予定、大量削除の保留（印つき）、操作の履歴（最新 10 件）、AI の再試行ができる種類。出すのは段の名前・
  結果の種別・件数・固定の理由コード・時刻だけで、パス・SQLite の文面・モデル名・接続先は出さない（失敗の文面は
  ログにだけ出す。[server.rs](../../crates/geoframe/src/server.rs) 冒頭の方針）。
- R15（必須）: 操作の POST は 5 本（`/api/admin/ops/sync`・`/quality-scan`・`/drive-release`・`/ai-retry`・
  `/ai-release-pause`）で、既存の書き込み API と同じ経路（Host 検査、JSON の Content-Type と Origin の CSRF 検査、
  本文 64 KiB、`INGEST=0` は 503）を通す。応答は `{"ok": true, …}` か `{"ok": false, "error": <固定の符号>}`。
- R16（必須）: `/api/admin/status` の形は変えない（`sync` の鍵の一覧を固定する試験がある）。

### 文書・互換

- R17（必須）: [deploy/README.md](../../deploy/README.md)・[deploy/geoframe-admin.sh](../../deploy/geoframe-admin.sh)・
  [docs/rust-migration/server.md](../rust-migration/server.md)・[docs/architecture/photo-pipeline.md](../architecture/photo-pipeline.md)・
  `ai/ops.rs` のモジュールコメントの「サービスを止めてから」「画面からは解除できない」を、「通常は運用タブから。
  `serve` が動いていないときは CLI」に改める。`docs/README.md` の索引に本 PRD を載せる。
- R18（必須）: `GET /api/v1/photos`・`/derivatives/` の契約（miharashi）と、CLI の引数・終了コードを変えない。
  `cargo test`（描画試験を含む）と `cargo clippy --all-targets -- -D warnings` が通る。

## 受け入れ条件

- AC1（R1, R2）: 描画試験で、運用タブのボタンが見え、`GET /api/admin/ops` が `ingest: false` を返す合成応答では
  ボタンが無く読み取り専用の案内が出る。
- AC2（R3, R4, R10）: 取り込み有効の `serve`（Drive の鍵なし。`tests/ingest_cli.rs` と同じ起動）への結合試験で、
  `POST /api/admin/ops/sync` が 200 `{ok:true, request:{id, kind:"sync", state:"queued"}}` を返し、続けて同じ POST が
  409 `busy` になり、`GET /api/admin/ops` の履歴に `kind:"sync"` の記録が現れ、段 `index_drive` が `skipped`
  （鍵なし）、`index_local` が `done` で終わる。描画試験で、受付の応答のあと同じボタンが無効になり、終了の応答で
  段の結果が描かれる。
- AC3（R11）: 結合試験で `POST /api/admin/ops/quality-scan` の記録が段 `quality` 1 つで、合成 DB の未解析が 0 に
  なる（`photo_quality` の行数で確かめる）。
- AC4（R6, R12）: 単体試験で、`plan()` が上限つきの許可（`UpTo(n)`）のとき消える行が n 以下なら削除し、n を超えれば
  `mass_delete_withheld` にする。結合試験で、保留を記憶に入れた状態（`spawn_state` で `MassDeleteBoard` に記録）から
  `GET /api/admin/ops` の `hold.token` を取り、正しい `token` の POST が 200、違う `token` が 409 `hold_changed`、
  保留が無いときが 409 `no_hold`。描画試験で、確認の段に件数が出て、409 で確認が閉じて読み直す。
- AC5（R13）: 結合試験で、`ai_jobs` に `failed` の行（handler 未登録の種類）を入れた DB に対し
  `POST /api/admin/ops/ai-retry {"target":"all"}` が 200 で `unsupported` の件数を返し、行は `failed` のまま。
  `ai_provider_pauses` に行を入れた DB に対し `POST /api/admin/ops/ai-release-pause {"target":"all"}` が 200 で
  `released` の件数を返し、行が消える。`{"target":{"task":"<用途>"}}` で設定が無いときは 409 `config_unavailable`。
- AC6（R14, R16）: 結合試験で `GET /api/admin/ops` が 200・`Cache-Control: no-store`、`INGEST=0` では
  `ingest:false` で 200、HEAD・POST は 403。本文に `data_dir` のパス・SQLite の文面・鍵のパスが無い（失敗した段を
  含む合成の記録で確かめる）。`tests/admin_api.rs` を変えずに通す。
- AC7（R15）: 結合試験で、5 本の POST が Content-Type 不一致・Origin 不一致で 403、`INGEST=0` で 503、本文 64 KiB 超で
  413、JSON でない本文で 400。
- AC8（R5）: 描画試験で、定期サイクルの直近の結果と次の予定、履歴 10 件が新しい順に描かれる。
- AC9（R7, R8, R17）: `cargo test` の `static_files` と `admin_ai_ops_render` が通る。文書に「サービスを止めてから」
  「画面からは解除できない」が、CLI の節（`serve` が動いていないときの手段）以外に残っていない（grep で確かめる。
  試験にはしない）。
- AC10（R18）: 既存の試験がすべて通り、clippy が通る。`/api/v1/photos` の契約試験に変更が無い。

## 制約

- 技術: Rust（axum）＋素の JavaScript。描画試験は Node（`cargo test --test admin_ui_*` 経由。直接 `node` で回さない）。
- 設定は起動時に読む。環境変数は足さない（読み直しの間隔はコードの定数）。
- 本番機でビルド・試験はしてよいが、デプロイ・サービス操作・本番 DB への接続はしない。
- 家庭 LAN の IP・ホスト名・鍵の内容を文書・コード・PR に書かない。
- PR は目的ごとに 2 本（設計書の「実装の順番」）。

## 実装の順番（PR の分け方）

1. **PR ①**: 同期系。同期ワーカーの要求の受け口と結果の記録、`GET /api/admin/ops`、`sync`・`quality-scan`・
   `drive-release` の POST、上限つきの削除の許可、運用タブ（この 3 操作と進み具合・履歴）、状況タブの文言、
   文書の該当箇所（R1〜R6、R8、R10〜R12、R14〜R18 の同期系）。
2. **PR ②**: AI 系。`ai-retry`・`ai-release-pause` の POST（`serve` に handlers と clock を持たせる）、運用タブの
   AI の操作、R7 の案内、R9、文書の AI の箇所（R3 の AI 分、R7、R9、R13、R17 の AI 分）。

① は単独でマージ・差し戻しできる。② は ① に依存する。

## 決まっていないこと

- 品質解析の一括を「未解析が無くなるまで」にしたので、写真が多いと長く走り、そのあいだ定期サイクルと他の要求は待つ。
  上限（1 回の解析枚数）を付けるかは、実機で時間を見てから決める（設計書のリスク）。
- R9（要対応の行）は、あると良いにとどめ、PR ② の時間があれば入れる。
