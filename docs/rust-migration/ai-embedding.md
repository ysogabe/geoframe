# 画像・日本語検索語Embedding（T4.4a）

T4.4aはLANアダプター、画像ジョブ、ベクトルの原子的保存、自動公開を実装する。E5/E6 HTTP・LRUはT4.4b、group/rankingはT4.5。追加Gemini送信はない。実装は `ai/{embedding,image_embedding}.rs`、共有HTTP・worker・indexと `imaging::prepare_embedding_rgb`。実LAN/実AI/実画像/実DB/SSH/鍵/cloud/本番サービスは使わず、合成DB・合成画像・loopbackで検証する。

## 固定契約と明示設定

根拠は `github.com/ysogabe/local-model-lab@c9b05b590d838b81a6b2f939af34e409f16469ad` の `docs/guides/resident-image-embedding.md`、`scripts/embedding_server.py`、`config/llm/profiles.json`。共有ghq cloneを `git show` で読み、実行・編集・checkoutはしていない。

この版はrevision/preprocess/provider_space_idをHTTPに返さない。管理者が `embedding_contract` で既知の契約版を明示し、宣言・task・実models能力・実response.modelの照合を通した場合だけ、上流の画像/文共通保証を採用する。未知Providerのモデル名/次元から空間を推測する経路はない。

```json
{
  "version": 1,
  "local_base_url": "http://192.0.2.10:8100",
  "embedding_contract": "local-model-lab-eg2-bf16-c9b05b59-v1",
  "embedding_space": {
    "space_id": "eg2-bf16-v1",
    "model_id": "embeddinggemma-2-bf16",
    "revision": "914f7f89142e33e77833254d9c9b90c3cef7303b",
    "image_preprocess": "eg2-pin-exif-rgb-srgb-768-jpeg-q82-v1",
    "text_preprocess": "eg2-raw-search-query-prefix-v1",
    "dimension": 768,
    "normalization": "l2"
  },
  "tasks": {
    "image_embedding": {"provider": "local", "model": "embeddinggemma-2-bf16", "service_id": "embed"},
    "query_embedding": {"provider": "local", "model": "embeddinggemma-2-bf16", "service_id": "embed"}
  }
}
```

URLは合成例（実接続には使えない）。既知契約の宣言には、確認済みの管理者設定を使う。新しい任意URL/Discovery URL採用の入口は足していない。未知のcontract、未知キー、重複キーはAI設定エラー（配信は継続、値はログ非出力）。contract未設定、task/provider/service/model/revision/preprocess/dimension/normalizationの不一致はアダプターを登録しない。`AiConfig::gate` は従来の形式・ポリシー検査なので、T4.4bも **必ず `EmbeddingClient::new` の成功を確認**する。

`provider_space_id=eg2-bf16-c9b05b59-914f7f89-v1` はこの固定契約が保証する空間。識別には完全なHF revision、上流checkpointのimage processor・EXIF/RGB、クライアントのsRGB/長辺768px縮小/新規JPEG q82、検索文prefixの版を含む。上流へのmodelはBF16のみ。Q8_0 aliasの要求、Q8_0/Geminiの旧ベクトル輸入・混合は行わない。

## Provider API

- `embedding::configured_identity(&AiConfig, TaskKind) -> Option<SpaceIdentity>`: 明示契約と全宣言の照合。ネットワークなし。
- `EmbeddingClient::new(config, shared_lan.clone(), task) -> Option<EmbeddingClient>`: プロセスの共有 `LanClient`（同じagent・背景/対話lane）を渡す。用途の異なるAPIは拒否する。`identity()`、`lan()` はT4.4bで利用可能。
- `probe(session, whole_deadline)`: `GET /llm/catalog` → `GET /llm/services/embed` → **固定** `GET /llm-embed/v1/models`。modelsの唯一のBF16エントリにtext/image・768次元を要求。catalog/service双方のavailable、running、lease_count=0を必須にする。任意のurl/local_url/endpointは無視。推論・予算消費なし。
- 画像POST: **固定** `POST /llm-embed/v1/embeddings`、`{model,input:[{image:"data:image/jpeg;base64,..."}],encoding_format:"float"}`。背景は1画像/job・request。外部URL、パス、圧縮bytesの直接受入を `PreparedImage` に追加していない。
- 検索語POST: `input:["task: search result | query: <query>"]`。画像にはprefixを付けない。空/300文字超は送らない（正規化・HTTP入力規則はT4.4b）。1〜8入力/完成JSON **4 MiB以下**。HTTP層でもEmbedding endpointだけ4 MiBを検査し、超過はDNS/接続/送信0。他のAI endpointは既存8 MiB。
- 応答は `{model,object:"list",data:[{index,embedding:[float,...]}],usage}`。配列順でなくindexで整列し、重複・欠落・範囲外を拒否。実model不一致は用途pause。有限数/nonzero/次元を検査し、BF16誤差をL2再正規化。画像は `PreparedVector`、検索語は `QueryVector` を必ず通す。異なる空間は検索で拒否する。
- private全DNS検査/pin、redirect 0、proxyなし、最初のHTTP write直前のStopFlagを共有。全体期限は画像60秒・検索語15秒。Discoveryはその残りと2秒の短い方。送信後の通信失敗/5xxはDiscoveryを再確認し、lease/停止なら共有wait、上流が使えるなら当該jobの反復失敗へ分類する。429はRetry-Afterとjob next_attempt、401/403はProvider pause、能力/実model不一致は用途pause。クラウドfallbackなし。

## 背景・保存・自動公開

production factoryは `INGEST=1` かつ有効なlocal image_embeddingだけを登録する。既存qualityの `content_sha256` と世代の合うphotosだけを対象にする。targetは `space_id + separator + SHA`、実ジョブキーは `ShaState::job_key(identity)` で全構成員のID/display/md5/mtime/rotationも束縛する。空間/処理版による **claim scope** があり、別空間の未完了jobはclaimしない。supersede/cleanupもその空間だけに限定する。

同じSHAのDB世代だけが変わっても失敗上限を回避できないよう、直近jobのattempts/defer_count/interruption_count/terminal/next_attempt/理由を新キーへ引き継ぐ。手動retryだけが現在jobの試行を明示的に解除する。cleanupはwriterの短いtransactionで継承・失効・登録を行い、失敗時は汎用enqueueへ進まない。succeededの有効vectorをresetせず、欠落/破損を共通検査で確認できた成功結果だけを再生成へ戻す（カウンタ・terminalは保持）。新しい世代でも有効vectorがあれば再送しない。成功キー/結果からの再起動も再送0。

画像は共有の安全なファイルread（通常ファイル・O_NOFOLLOW/サイズ制限・実fdがDataDir内）だけを画像lock内で行う。decode、EXIF一度適用、sRGB、縮小、新規JPEG化、HTTPはwriter/tx/画像lock外。送信直前はDiscovery後にもDB世代と実source SHAを再読し、その後に最終 `before_send`（停止・設定・pause・budget）を通す。応答後も実source SHAを確認し、完了時にtokenとDB世代をCASする。SAVEのlat/lon・hidden/favoriteは画像世代と混同せず、一次列を上書きしない。停止後の新規送信は0、既に送った有効な結果は既存の停止/tokenルールで確定する。

`store_prepared_in` を `jobs::complete` のsavepointで実行するため、vector保存とjob成功は原子的。旧有償表には書かない。実ファイルの再読はwriter外、最終DB CASはwriter内であり、その間の未管理のファイルだけの置換を同一transactionでlockするものではない（管理経路の世代更新/回転/削除はCASで拒否する）。

構築開始時の `BuildSnapshot` は全構成員を保持する。reconcile後およびjob確定/失敗後に既存 `try_activate_default` を呼ぶ。pendingは旧pointer保持、現キーfailedは除外、成功率既定0.95・成功1件以上を満たす場合だけ原子的に公開する。99/100と全失敗を試験。手動retryの成功・新規/変更写真は有効JOINで差分追加される。検索切替は旧選別を解除しない。snapshotはメモリのみで、再起動時は現対象と保存済み結果から再構成する。全構成員がstale/空なら次周期に構築対象を再取得する。

## T4.4bへの接続

起動時に既存の `LanClient::new(&config.ai)` を一度だけ作り、背景factoryと対話側へcloneする。`EmbeddingClient::new(&config.ai, lan.clone(), TaskKind::QueryEmbedding)` の失敗は定型503にする。対話の新規 `AiHttp` を作ると枠・agentが分裂するため、同じ `lan.http()` を使う。

```rust,ignore
// route側で設定/pause/共有wait/レート制限を確認し、キャッシュmissのときだけ呼ぶ。
let whole = Deadline::interactive();
let result = lan.http().run_interactive(&stop, move |session| {
    client.query(&session, &normalized_query, &whole, || {
        // Discovery待ちの後にも現在の停止/設定/pause/waitを確認する。
        // LAN専用。ここでCloud権限や予算を拡張しない。
        check_current_query_policy()
    })
}).await;
```

`query` は同期APIで、上のblocking境界の中だけで呼ぶ。`before_send` callbackは `Result<(), ProviderError>`。受信した `QueryVector` のidentityをactive indexと比較し、旧空間に流用しない。`ProviderError` のWait/PauseをT4.4b側の共有DB状態にも反映すること（この低レベルAPIは対話用DB/writerを所有しない）。`run_interactive` がBusyなら待たず503。LRUキー・64件・E5/E6 JSON・毎分30件・writer200msの対話予約・HTTPへの本接続はT4.4bの範囲で、本タスクはrouteを変更していない。

## スキーマ・dry-run・検証境界

T4.4aの新表/列/索引は **0**、T4.6c2まで適用済みの合成DBで追加不足数/dry-run増分は **0**。既存AI移行（空間2表+索引等）の不足一覧は従来どおりで、`REQUIRED_SCHEMA`・Python既存表は変えない。INGEST=0ではfactoryなし・画像送信なし・用途専用の新規移行なし。親所有のtasks/受入表/設計開始記録は変更しない。

合成試験は契約envelope/index対応/モデル・次元・有限数・zero/L2、4 MiB超過DNS/送信0、fresh JPEGのmetadata非送信、停止/世代/ファイル/SAVE/削除/設定/pause/lease競合、旧token拒否、HTTP中writer/画像lock/対話lane利用、原子的rollback、再起動/破損修復/差分/失敗上限・手動retry/別空間・別用途/cleanup失敗を扱う。既存全suiteで旧DB/INGEST=0、ログ秘匿、LockHold寿命、Python切戻し、Cloud/Drive互換も確認する。

実LAN/LXC疎通、実画像と日本語の意味精度、Rust/LXC経由の背景8件中の新規query15秒ACは **未確認（親担当）**。親が上流へ合成768px JPEG背景8連続+query並行を3回送り、query19〜33ms成功を確認したことは前提記録であり、本Rust実装の15秒AC達成とは数えない。通常レビューは別Sol、security-reviewはP4全体の最後。T4.4aだけでP4完了とはしない。

### 2026-10-28のローカル検証結果

- `cargo fmt --check`、`cargo clippy --workspace --all-targets -- -D warnings`: 成功。
- `RUST_TEST_THREADS=4 cargo test --workspace`: **全28 result group（26 test binary + 2 doctest group）、1135成功・失敗0・ignore2・filtered0**。内訳はlib 943成功/ignore1、main 0、integration 188成功、doctest 4成功/ignore1。T4.4a追加は契約8件+画像handler19件=27件。
- ignoreは既存の「大量ローカル変更の計測」とwriter説明の例。Python/Pillow切戻しとNode管理画面試験は `--nocapture` でも成功し、環境不足による条件付き省略なし。実LAN/LXC等は上記の未確認項目として区別する。
- 初回のloopback試験はsandboxのsocket拒否で失敗し、権限付きで再実行した。最初の全suiteは既存WALコピー試験で書き手スレッド未実行の失敗があり、単独成功を確認した後、並列数4で成功。世代付きキーに追従していなかった索引/CLI fixtureも修正し、最終全suiteで成功した。
- `cargo build --workspace --release --target x86_64-unknown-linux-musl`、`file`/`ldd`、`--version`: 成功、static-pie/statically linked、`geoframe 0.1.0`。新依存なし。
