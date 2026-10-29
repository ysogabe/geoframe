# T4.4a レビュー修正（2026-10-28）

原タスク開始: `6fe7fd7e6a3ef4e191373445004508f0305279a7`。開始記録 `2521f86` は保持。
今回の修正開始: `b297f6b24b225e162a59f8b003f5d5c668354181`。
branchは `feat/rust-migration-p4`、同じworktreeで自分で実装し、再委任・別checkout作成・PR・mergeは行わない。
`tasks.md` / `p4-foundation-acceptance.md` は今回の編集・stage対象外。

## 採用した指摘と方式

| 指摘 | 修正と直接の回帰 |
| --- | --- |
| 必須1 P1 系統のterminal/履歴消失 | `ai_image_embedding_lineage` が同space/SHA/processorの現jobを指す。対象消失中も参照先を残し、新世代へattempt/defer/interruption/terminal/next/errorを継承。旧succeededや秒精度の時刻・生成idで現系統を選び直さない。A成功→複製B成功→vector欠損→422→不適格→復帰を61秒刻みで実行し、Bと新Cのterminal保持を確認。同秒A→B→A→全不適格→復帰/新Cを、通常上限・恒久・中断上限・非試行deferの4系統で検証。 |
| 明示retryとの整合 | enqueueは正確なキーのterminalを解除しない。引継ぎは参照先と正確な行の2行のみで、全履歴MAX/ORではない。手動Requeueは現在jobと同target/processorの旧世代のカウンタ・terminal・next/errorを同一transactionでリセットし、pointerを現在jobへ合わせる。このため旧terminalが復活しない。lineage作成前のretry、既存Aへの往復、新Cへの移行も試験。 |
| 必須2 P1 POST timeoutが入口waitへ変わる | 画像60秒/対話15秒の同じ全体期限内で、POSTの末尾2秒（残り6秒未満は残りの1/3）を再probeへ予約。受信POSTをCondvarで保留し、再probeまで応答しない同期fixtureを900ms全体期限で検証。availableならRepeatedと上限直前からのfailed遷移、lease/サービス停止/到達不能ならWait、クライアント停止ならStopping。HTTP gateのstartedを使い、DNS/接続前の失敗を非試行waitへ分類。DNS/接続中停止はPOST 0。偽時計で15秒期限が再開始/延長されないことを検証。 |
| 必須3 P2 processor越境 | cleanupと新しい内部 `supersede_processor_in` の両方をtarget/processorへ限定。image handlerのgeneric reconcileは空とし、後段の汎用supersedeへ戻る経路を除去。他用途の既存 `supersede/enqueue_batch` は同じ意味。別processor・別space・別space同processorそれぞれのpending/running/deferred/failedとtokenを保護し、計画後の世代変更および同kind claimの境界を検証。 |
| 推奨 分割とN+1 | read側で対象SHAをHashSetへ収集し、writerは1 SHA/closure・最大200 closure/txへ分割。各chunkで索引付きSHA再読と継承・pointer・失効を原子的に確定。途中chunkのSQL故障で先行chunkだけ残り、generic reconcileが呼ばれないことを検証。実workerの第1 COMMITへ停止を同期し、199件が確定/未claimのまま停止し、再起動相当の再計画で450件へ修復。failedはbulk SELECT+HashMap。単一SHAの実行/再利用で全件scanを除去。公開済み、または未完成vectorのqueued jobが残る場合はread側1 SQLからmaintenance writerを省く。公開時のwriter再検証は維持。 |
| 参考 文書 | `ai-index.md` 末尾を実装済み画像ワーカー/自動切替と、T4.4b以降のE5/E6/LRU/HTTP等の境界へ更新。`ai-embedding.md` を最終方式・移行影響へ追従。 |

lineage未導入の旧queueの最初の取り込みだけは、未完了の世代を優先して初期化する。全行obsolete等で旧コードが順序を既に失った場合はterminalを保守的に保持する。移行前に失われた履歴や同秒順序を正確に復元することはできない。必要なら現在キーの明示retryで旧履歴ごと解除できる。導入後は対象消失中もpointerが残り、時刻・idで履歴を推定しない。

## API・schema・dry-run

- 外部公開Rust APIのシグネチャ、E5/E6 HTTP、設定形式、固定 `embedding_contract` は不変。追加した期限予約・送信追跡・SHA限定scan・processor限定supersede・lineage enqueueは内部API。
- `jobs::TABLES/pending/migrate` に **1表** `ai_image_embedding_lineage(target_key, processor_version, job_id)` を追加。主キーは前2列、job_idはai_jobsへのFK（ジョブを明示削除した場合はCASCADE）。追加列・明示索引・新crateは0。SQLiteの主キー自動索引は1つ増える。
- 既存T4.4a DBの不足一覧/dry-runは **+1**。Python相当DBから用途専用表なしのAI objectは9→10、全表は18→19。既存job行を変えない冪等移行と不足一覧を直接試験。INGEST=0の旧DBは書かず読める。
- `REQUIRED_SCHEMA`、Python schema、一次列、旧有償結果表は不変。Q8_0/Gemini vector輸入0、追加Gemini送信0。試験用に既存rusqliteのtrace featureだけ追加し、SQL本文/値を出力せず件数を数える。本番依存の追加なし。

## 否定対照

各修正を一時的に外して、対応する重要試験が欠陥を検出することを確認後、finallyで復元した。

| 一時復元/無効化 | 実際の失敗 |
| --- | --- |
| b297f6bのimage cleanup | 旧成功からBのterminalが消え、failed件数0（期待1）。 |
| 同じ旧cleanup | 同秒の通常上限履歴が `(0,0,0,false,now,"")` に戻る（期待attempts=5/defer=6/terminal=true/timeout）。 |
| 同じ旧cleanup | 別processorのpending/running/deferred/failedがobsolete/token NULLになる。Optionの値比較で検出（fixtureのNULL型エラーで済ませない）。 |
| POST末尾の再probe予約だけを外す | availableでも `Wait(Entrance,Unreachable)` に戻り、`Transient(Timeout)` のassertが失敗。 |
| retry時のlineage履歴解除だけを外す | 旧terminalが1件残る（期待0）。 |

いずれも意図したassertでexit 101。修復後は同じ回帰が成功。独立レビューの `/tmp/geoframe-t44a-review-nvqqo9o4` は内容を読んで回帰へ適応し、コピー元を編集/無断コピーしていない。

## 6500件の測定

合成DB・実画像ファイルなし・loopback fixture・x86_64 Linux、cargo **debug/unoptimized + debuginfo**。初回cleanup、最大200 closure/tx（登録1+6500 SHA）、時間の閾値assertなし。standalone画像suiteで次を記録した。

- cleanup: **33 writer区間、合計608.065ms、最大19.372ms**。時間測定にSQL traceは入れない。レビューの旧コードは単一writer区間606msであり、総時間の短縮は主張しない。最大占有の分割が目的。
- 未完成vectorが残るmaintenance: **writer closure 0**。公開後もCASEの遅延評価で全件queue照会を省く。
- 全failedの公開判定: **writer 97.297ms、SQL計8、failed SELECT 1**（trace_v2で実測）。旧コードのfailed照会は1 scanにつき6500。レビューの旧maintenance時間91msとほぼ同規模で、残る世代hash/ローカル再検証のCPUを時間短縮と誤認しない。
- 古い `keys.contains` の全件線形照合による最大6500²比較を除去。計画はHashSetとソート、writerはSHA/自然キー/主キー索引で該当行を処理する。全件failed分類は1 SELECTとHashMap照合。毎jobのwriter全件scanを省き、未公開で完了した段階の原子的公開検証は残す。

## 検証結果

- `cargo fmt --all --check`、`cargo clippy --workspace --all-targets -- -D warnings`: 成功。
- `RUST_TEST_THREADS=4 cargo test --workspace -- --nocapture`: **28 result group（26 test binary + 2 doctest group）、1146成功・失敗0・ignore2・filtered0**。内訳はlib 954成功/ignore1、main 0、integration 188成功、doctest 4成功/ignore1。
- 画像suite27件、Embedding契約/通信11件。初回実装の19+8から計11件追加（パラメータ化したterminal4種、retry導入前後、reprobe5種等はこの件数の内側）。
- ignoreは従来の大量ローカル変更計測とwriter説明例の2件。Python/Pillow切戻し・Node管理画面試験は最終nocaptureログでも成功し、環境不足による条件付き省略なし。compile-fail doctestのコンパイルエラー出力は期待どおりの成功として区別する。
- 初回sandbox画像suiteは19件すべてloopback bindのEPERMで失敗し、権限付き再実行。実装判定前の環境失敗として区別する。最初の権限付き画像suiteの2失敗は、新FKが明示job削除を拒否した点（CASCADEへ修正）と、有効vector再利用中の公開をmaintenance guardが抑えた点（修正）だった。
- workspace初回の4失敗は移行の追加object期待値、次の実行の3失敗はCLIの追加object/表数期待値。新しい移行仕様にfixtureを更新。否定対照の失敗とは別に扱う。
- `cargo build --workspace --release --target x86_64-unknown-linux-musl` 成功、`file`: static-pie linked、`ldd`: statically linked、`--version`: geoframe 0.1.0。

実LAN/実AI/実画像/実DB/SSH/本番/鍵/cloudと `/var/lib/geoframe` は未使用。実LAN/LXC・背景8件中query15秒の実受入は親担当、T4.4bのE5/E6/LRU/HTTPとT4.5 groupは範囲外。今回だけでP4完了とはしない。通常の別Sol再レビューとP4全体最後のsecurity-reviewは未実施。
