# ADR: ホスト境界を越えるエージェント稼働データはHome Assistantを唯一のバスとする

- 日付: 2026-08-29
- 状態: 提案（Codex・Claude Opus・Claude Sonnetの合議 → Fable裁定。ユーザー承認待ち）

## 文脈

「エージェント稼働状況」表示モードは2つの独立したデータソースを合成する:

1. **50-echo-left-deviceの6枚タスクカード** — 真実はMac上で常駐するブリッジプロセスのメモリ（`BridgeState`）にのみ存在する。
2. **Hermes Coding Runtimeの稼働状況** — 別ホスト（LAN内）でHTTPゲートウェイとして稼働しており、Bearerトークンで認証する。このトークンは、ジョブ投入（リポジトリへのコミットを作る操作）を含む全ツールに効く単一トークンで、**読み取り専用スコープが存在しない**。Macは既にこのトークンを保持している（開発フローで日常的に使用しているため）。

どちらのデータも、キオスク機（N100、GEOFRAME server.py）とは別ホストにある。これを取得するアーキテクチャが論点だった。当初「Hermes側のデータ取得経路は未確定」という前提で議論が始まったが、合議の過程でHermesゲートウェイへの到達性・認証方式を実地検証した結果、経路自体は実在することが確認された。

## 決定

**Home Assistantを、ホスト境界を越えるエージェント状態データの唯一のバスとする。**

1. Mac上に小さな常駐publisher（新規実装）を置き、(a) 50-echo-left-deviceの診断API（localhost限定のまま変更しない）をポーリングするEchoアダプタ、(b) Hermesゲートウェイをポーリングし要約統計を取得するHermesアダプタ、の2つのsource adapterを持たせる。
2. publisherはHome AssistantのREST API（long-livedトークン）経由で、要約値をstate、構造化ペイロードと**publisher側のタイムスタンプ**をattributesに持つエンティティへpublishする。
3. GEOFRAME側は新モジュール `app/geoframe/agents.py` を新設し、既存の`ha_get()`（`app/geoframe/ha.py`）でこれらのエンティティを読み、正規化した上で読み取り専用の `/api/agents` として公開する。フロントは[[0004-frontend-display-mode-arbitration]]で決めた`#modelayer`上にAGENTS/HERMESモードとして描画する。
4. **staleness契約を明文化する**: publish間隔のN倍（目安3倍）を超えたデータは、STALEバッジと表示の減光で可視的に劣化させる。凍結したデータが生きたデータに見えることを防ぐ。

## 理由

- **決定的な理由はHermesトークンの性質にある。** 読み取り専用スコープが存在せず、ジョブ投入まで実行できる単一トークンを、既定で`0.0.0.0`バインドかつ無認証のHTTPサーバーを運用しているキオスク機（GEOFRAME）に置くことはできない。Macが読み手（publisher）になれば、新しい資格情報の配布は一切発生しない（Macは既に開発フローでこのトークンを保有している）。
- HAのlong-livedトークンという、このシステムが既に日常的に運用している信頼モデルをそのまま再利用でき、新しい認証方式を発明する必要がない。GEOFRAME側もEchoアダプタ側も、新規の受信面（inboundなmutatingエンドポイント）を一切追加しない。
- 却下した代替案:
  - **GEOFRAMEからHermesのPostgreSQLを直接読む**: DBポートはLANから明示的に閉じられており、これを開けることはHermes側の明文化されたセキュリティ統制の後退になる。
  - **GEOFRAMEからHermesゲートウェイを直接叩く**: 上記のトークン問題がそのまま当てはまる。
  - **50-echo-left-deviceの診断HTTPサーバーをLANへ公開してGEOFRAMEがpullする**: 同じHTTPサーバーが操作系エンドポイント（アクション送信・RPC）も提供しているため、表示目的のためにLAN公開すると意図せず操作面まで露出する。50-echo-left-deviceの`SECURITY.md`が明記する「localhost限定・無認証」の前提を実質的に破壊するため却下。
  - **Mac→GEOFRAMEへの直接push（GEOFRAME側にmutatingエンドポイントを新設）**: GEOFRAMEに無認証の書き込み口が増え、[[0002-screen-power-override-via-ha]]で却下した理由と同じ後退になる。

## 制約と次段階

- タスクの詳細情報（タイトル等のプライベートな内容）はHome Assistant経由でLAN内表示に限定し、外部公開物（FAQ LP、公開Issue、ログ）には一切含めない。50-echo-left-deviceの`SECURITY.md`が示す「プライベートな内容を公開の場に出さない」という方針をここでも踏襲する。
- 該当エンティティはHome Assistantのrecorder機能から除外し、稼働状況の詳細が長期間DBに保持され続けることを避ける。
- Hermesアダプタの実装前に、Mac上の非対話的なクライアントからHermesゲートウェイのAPIへ実際に接続できるかの最小限の疎通確認（スパイク）を行う。失敗した場合のフォールバックは、Mac上のファイルドロップをpublisherが読む方式に留め、Hermes側の仕様変更やトークンの移設は行わない。
- 片方のソースが不通の場合は、生きている方だけを部分表示する。両方STALEの場合はこのモード自体を[[0004-frontend-display-mode-arbitration]]の優先度ラダーから除外する（表示しない）。
- 50-echo-left-deviceのEchoアダプタ実装は、その診断APIの仕様（`GET /api/state`）に追従する形とし、50-echo-left-device側のプロトコル定義を変更しない。
