# 設計案: Go単一バイナリの写真管理APIと外部AI Provider

- 状態: **提案（未実装）**
- 作成日: 2026-10-24
- 追跡: [GEOFRAME #54](https://github.com/ysogabe/geoframe-ambient-display/issues/54) / [local-model-lab #93](https://github.com/ysogabe/local-model-lab/issues/93)
- 現行方針: [ADR 0008](../decisions/0008-photo-platform-and-miharashi-display.md)

## 目的・判断基準

GEOFRAMEは「人の代わりに写真をざっくり整理・判断し、簡単に管理・活用できるAPI」である。Pythonの実行環境・venv・依存インストールを本番GEOFRAMEから取り除き、Go単一バイナリにする。推論はローカル／クラウドを問わず**外部HTTP API**に集約する。PoCや研究でPythonを使うことは妨げない。

旧pHash、画質スコア、推薦順位、モデル出力との**数値一致は目標ではない**。主要な利用体験、永続データ、表示クライアントとの公開API契約を優先する。シンプルなルールで十分ならAI呼び出しそのものを減らす。

## 境界と責務

```text
miharashi / 管理Web UI
         │ HTTP
         ▼
GEOFRAME (Go単一バイナリ)
  ├─ HTTP API / 管理画面 / 写真フィード・画像配信
  ├─ Google Drive・ローカル索引 / SQLite / derivative生成
  ├─ 簡易品質評価・近似グループ・ルールベース代表選択
  ├─ 永続ジョブ / プロバイダー選択 / 後回し・再試行
  └─ AI Providerアダプター ─────┬── local-model-lab (LAN API)
                              └── Gemini等のクラウドAPI
```

- GEOFRAMEはGPU/モデルプロセスの起動停止やPyTorchの実行に関与しない。
- `local-model-lab`はモデル実行、llm-managerによるavailable/lease、既存LANゲートウェイを担当する。
- `miharashi`の表示実装は変更しない。`GET /api/v1/photos`と`/derivatives/`の[写真フィード契約](photo-feed-contract.md)を維持する。
- モデル別HTTP APIの違いはGEOFRAME内の薄いアダプターで吸収する。全能力に共通の巨大インターフェースを強制しない。

## 既存のモデル・APIと採用候補

[local-model-labのカタログ](https://github.com/ysogabe/local-model-lab/blob/main/config/llm/profiles.json)と[画像AI評価](https://github.com/ysogabe/local-model-lab/blob/main/docs/geoframe-local-image-ai-report.md)を参照する。

| 用途 | 推奨案 | 備考 |
| --- | --- | --- |
| 画像・日本語のEmbedding | 画像対応EmbeddingGemma-2をHTTP API化 | **未提供。local-model-lab #93でPoC**。現在の`/llm-embed/v1/embeddings`はテキスト専用 |
| 同一場面候補 | GoでEmbedding近傍＋撮影時刻を使用 | 旧しきい値の厳密再現は不要 |
| 撮影地の手がかり有無 | 既存d3-lite Decision API | `/llm-decision/v1/systemone`。現行Pythonにも先行利用あり |
| 撮影地候補の具体化 | クラウドGeminiを基本、Qwen3-VLを比較 | ローカルだけで確実な場所を推定できる前提にしない |
| 写真の粗いタグ | 必要な場合だけd3-lite / Qwen3-VL | 誤認識を許容し、確定メタデータとは区別 |
| 代表写真の選択 | Goのルール優先、必要時のみGemini等 | 人の好みと旧Gemini出力の一致を受入基準にしない |
| 旅行ブログ | 既存の事実ベース生成を基本 | AI文章生成は任意 |

注意: 実写真6,515枚を使った評価でEmbeddingGemma-2 BF16は同一場面グループARI 0.944。Qwen3-VLやd3-liteによる旧Gemini選別の再現は不十分だったが、**Gemini出力は人の正解ではない**ため、移行後の体験に沿って再評価する。画像対応EmbeddingGemma-2 BF16のGPU常駐は既存4サービスとの同居時VRAM不足が懸念されるため、CPU API案とキャプション→テキストEmbedding案も比較する。

## APIとデータ

- 外部APIは既存の写真フィード・静的配信・管理画面の動作を維持し、Go実装への切り替えを内部変更として扱う。
- `catalog.db`は**再構築できない一次データ**。写真ID、手動GPS修正、お気に入り、除外、回転・人手の判断を保護する。移行前にSQLiteオンラインバックアップ、復元テストを実施する。
- AI派生データ（embedding、キャプション、スコア、グループ）は再生成してよい。**手動フィールドまで消すことは禁止**。新旧サービスによる同一DBへの同時書込みは避ける。
- Embeddingは`model_id` / `revision` / `preprocess_version` / `dimension` / `embedding_space_id`にひも付けて保存する。同じ検索インデックス内で異なるベクトル空間を混在させない。モデル切替時は別インデックスに再計算し、完成後に切替する。
- APIのGo実装ではタイムゾーン・写真の表示順・エラー処理・静的配信のパストラバーサル防止を確認する。
- HEIC・JPEGのderivative生成はライブラリ依存・配布形態・メモリを実機検証する。「Goなら必ず完全静的」とは仮定しない。

## 推論呼び出しの契約

1. `local-model-lab`の`GET /llm/catalog`と`GET /llm/services/<id>`で能力・availableを取得。利用可能状態はヒントであり、実際のHTTPエラーも必ず処理する。
2. `/llm-decision/v1/systemone`、`/llm-vl/v1/chat/completions`などの現行LAN入口を使用。`X-Client-Name: geoframe`を付与する。画像は**縮小・EXIF除去後のdata URL**で送る。
3. 画像Embedding APIのパスとJSONスキーマはlocal-model-lab #93で確定し、既存テキスト専用APIを破壊しない。
4. バックグラウンドジョブはDBに永続化し、エラー・再試行時刻・モデル/入力ハッシュ・処理版を記録。GPU lease中の停止は`defer`。再試行は冪等で、回数とバックオフに上限を設ける。
5. クラウドへの写真送信は**タスクごとの明示ポリシー**がある場合だけ許可。ローカル障害を契機に家庭写真を無断でクラウドへ送らない。
6. 写真配信・検索済みの索引・管理UIはモデル停止中も利用可能にする。検索文Embeddingが得られない場合は曖昧な結果を捏造せず利用不可を返すか既存のキャッシュを使う。
7. APIログ／トレースに写真内容・EXIF/GPS・認証情報を出さない。タイムアウト、本文サイズ、429/502/503/504、キャンセルを扱う。

設定例（**将来案。現行設定ではない**）:

```yaml
ai:
  image_embedding:
    provider: local
    service: image-embedding
    on_unavailable: defer
  photo_tags:
    provider: local
    service: decision
    on_unavailable: defer
  location:
    provider: cloud
    model: gemini
    on_unavailable: defer
  photo_judge:
    provider: cloud
    model: gemini
    on_unavailable: simple_selection
```

## 移行と検証

1. **P0**: 現行API・管理UI・SQLite・ジョブ一覧を棚卸しして代表的なE2Eシナリオを定義。local-model-lab #93で画像Embedding APIをPoC。
2. **P1**: Goによる読み取り専用カタログ、`/api/v1/photos`と静的画像配信を実装し、miharashiから表示確認。
3. **P2**: Google Drive/ローカル取り込み、EXIF・JPEG/HEIC変換、簡易品質選別を実装。
4. **P3**: 管理画面・変更API・旅行ブログ・地理情報を移し、ユーザー変更データを確認。
5. **P4**: AIアダプター／永続ジョブ／Embeddingモデル別インデックスを実装。停止、lease、タイムアウト、部分再試行、クラウド送信の禁止をテスト。
6. **P5**: SQLiteバックアップ・復元演習、旧Python停止、Goをsystemdで開始、スモークテスト、問題時のロールバックと再切替を検証。CIはGoビルド・テスト・成果物を配布し、環境のPython依存をなくす。

### 受入条件

- 写真追加→取り込み→簡単な整理→検索→表示→人手修正まで使える。
- 写真ID、お気に入り、位置手動修正などの永続データを失わない。
- miharashiの写真フィード契約に変更がない。
- GPU停止中も写真一覧・画像・管理UIを使え、AIジョブは復帰後に再開できる。
- Geminiへの画像送信は明示設定した用途に限る。
- Goバイナリがsystemd管理で動き、Python/venvがGEOFRAME本番デプロイの必須条件ではない。
- 旧画質スコアやAIモデル出力の数値一致を求めない。

## 非目標・未決事項

- 非目標: 全AIモデルの自作、local-model-labの推論コード移植、miharashiの再開発。
- 未決: 画像Embedding APIのJSON契約、CPU／GPU配備、HEIC変換バックエンド、旧DBへのマイグレーション詳細、モデル別インデックスのストレージ形式。各Phaseの実装前に決定する。
- **本設計案は採用済みADRではない**。レビュー後、必要なら新しいADRで採否を記録する。
