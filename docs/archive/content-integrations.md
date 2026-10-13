# GeoFrame 拡張コンテンツ検討 — Google連携と写真以外の表示

作成日: 2026-07-28 / 前提: RPi400またはN100ミニPC、写真はローカルフォルダに「雑に置く」運用（Phase 1実装済み）

---

## 1. Google連携で面白いデータ

### ◎ Google Calendar — 実装容易・効果大（本命）

- 「今日の予定」チケットをHUDに表示、次の予定までのカウントダウン
- **場所付き予定を地図にプロット**し、フォトジャーニーの合間に「次の目的地」として巡回に混ぜる — GeoFrameの世界観と自然に融合する
- 実装: Google Calendar API（OAuth、無料枠で十分）。このCowork環境にはCalendar MCPが接続済みなので、表示内容の試作・壁打ちは今すぐできる

### ◎ Googleマップ タイムライン（ロケーション履歴）— 演出効果最大、ただし半手動

- 行動履歴の**移動軌跡を地図にアニメーション描画**（1日/1旅行分の軌跡が流れ、写真がその上に咲く）。「旅の再生装置」になり、演出インパクトは全コンテンツ中最大
- 忖度なしの注意: タイムラインは2024〜25年にデバイス内保存へ移行し、**公式APIは存在しない**。スマホの設定からエクスポートしたJSONを photos/ と同じく「フォルダに雑に置く」運用になる（自動同期は不可）。写真と同じ設計思想で吸収できるのが救い
- 代替: スマホのGPSロガーアプリ（OwnTracks等）を使えば全自動化も可能（Google外の解になる）

### ○ Google Photos — Picker APIで写真追加（Phase 2）

- 前回整理の通り、全ライブラリ自動同期は不可。Pickerで選択→サーバーがダウンロード→photos/に保存、で既存パイプラインにそのまま乗る

### ○ Gmail — 旅程抽出「NEXT JOURNEY」パネル

- フライト・ホテル・レストラン予約メールから次の旅程を抽出し、「次の旅」の目的地を地図に予告表示。行き先の用途地域・ハザードを先に眺める、という遊びができる
- 実装はLLM併用（構造化抽出）が現実的。優先度は中〜低だが独自性は高い

### △ 見送り推奨

- **Google Fit**: REST APIは2025年に終了済み。後継はHealth Connect（Android端末内）かFitbit Web API。歩数表示をやるならFitbit経由
- YouTube等の視聴データ: 位置と結びつかず世界観に合わない

## 2. 写真以外の表示コンテンツ（キー不要度×効果で整理）

| 優先 | コンテンツ | データ源 | 認証 | 相性 |
|---|---|---|---|---|
| ★1 | **自分統計**: 都道府県制覇マップ、総移動距離、月別撮影ヒートマップ | 手元写真のEXIFだけで生成可能 | 不要 | 追加データゼロで「自分のデータ」の面白さ。制覇率カウンタはHUD向き |
| ★2 | **天気パネル**: 現在地/写真地点の予報・気温 | 気象庁 予報JSON | 不要 | 常時表示の定番。地味だが生活実用性を担保 |
| ★3 | **地震・気象警報**: 直近の地震を地図に波紋リング演出、警報時はHAZARD色に画面が遷移 | 気象庁 地震・警報フィード | 不要 | サイバーHUDと相性最強。防災データ（Phase 2の浸水想定等）への導線にもなる |
| ★4 | 日の出/日の入り・月齢・ISS通過 | 天文計算（ライブラリ）/ Open Notify | 不要 | アンビエント表示の彩り。夜間モード切替のトリガにも使える |
| ○ | 上空の航空機をリアルタイム表示 | OpenSky Network（無料・レート制限あり） | 軽 | 「管制室感」が出る。話のネタとして強い |
| ○ | 電車運行情報・遅延 | 公共交通オープンデータ(ODPT)（無料登録） | 要登録 | 朝の実用枠。通勤があるなら価値大 |
| ○ | ニュースティッカー | NHK等のRSS | 不要 | 既存ティッカー枠にそのまま流せる |
| △ | 深夜アンビエント: 自宅周辺の地価・人口メッシュをゆっくり流す | 不動産情報ライブラリ | Phase 2 | APIキー取得後の楽しみ |

## 3. 実装済み: Phase 1.5「自分統計」＋「気象庁（天気・地震）」

当初「次の一手」として提案していた**「自分統計」＋「気象庁（天気・地震）」は実装済み**。上表（§1）の★1自分統計、★2天気パネル、★3地震・気象警報がいずれも実現しており、「写真巡回の合間に挟まる別画面」というローテーション構造（写真N枚 → 統計 → 写真N枚 → 天気/地震 → …）で画面の単調さを解消する狙い通りに動いている。Google Calendarはまだ未実装（OAuth設定の手間はあるが確実に動く見込み）。タイムライン軌跡は演出の目玉として旅行後に試すのが良い、という位置づけも変更なし。

## 4. 写真同期の比較検討（Google / Apple / Amazon）— 2026-07-28調査

| 経路 | 自動化 | EXIF/GPS | 評価 |
|---|---|---|---|
| **Google Drive＋rclone定期sync（2026-07-28追加）** | 半自動（Driveへは手動投入、以降は全自動） | 完全保持・無加工 | **◎ 採用**。Photos APIの制限対象外で公式フルサポート。「雑に置くドロップボックス」思想に合致し、Driveアプリから消せばフレームからも消える＝外出先キュレーション可。注意: カメラロール自動収集は無い／自前OAuthクライアントは「テスト」状態だとトークン7日失効（rclone標準クライアント使用か本番公開で回避） |
| **Apple: Mac＋osxphotos定期エクスポート→rsync/Syncthing** | 完全自動（launchd） | 完全保持・フル解像度 | ◎ 全自動収集が欲しい場合の本命。公式APIは無いが全処理ローカルで規約・仕様変更リスク最小。条件はMacの定期起動のみ |
| Google Photos Picker API | 半手動（都度選択） | 保持 | ○ Phase 2の追加経路 |
| スマホ→Pi直接転送（PhotoSync/Syncthing） | 完全自動 | 保持 | ○ Mac無し環境の最適解 |
| Apple: iCloud共有アルバム非公式Web API | 自動 | **位置情報剥奪・2048px縮小** | × DIYフォトフレーム定番だがgeoタグ必須の本アプリでは致命的。不採用 |
| Apple: pyicloud等非公式ライブラリ | 自動 | 保持 | × 2FA・高度なデータ保護で頻繁に破損。非推奨 |
| Amazon Photos | — | — | × 公開API無し（Drive APIは2020年新規終了、Drive自体2023年末終了）。選択肢外 |

どの経路も最終的に「photos/にファイルが入る」に収斂するため、server.pyの変更は不要。

### 【採用・2026-07-28実装】Drive直接参照（server.py内蔵・rclone不要・ダウンロード無し索引化）

server.pyがサービスアカウントでDriveの `GeoFrame` フォルダを直接監視する。**当初想定していた「ローカルキャッシュへの自動ミラー」ではなく、索引と表示用derivativeを分離した方式**で実装されている。

1. **索引化**: Drive API v3の `files.list` を `imageMediaMetadata(time,location)` フィールド指定で呼び、**ファイルを一切ダウンロードせずに**撮影時刻・GPS位置だけを `catalog.db` に索引する（`index_drive`関数。コメント「Drive一覧のimageMediaMetadataでダウンロード無しに索引化」）。Drive上の全写真がまず軽量にDBへ載る
2. **derivative生成**: 索引のうち「位置情報があり、まだ表示用ファイルを作っていない」写真（`lat IS NOT NULL AND display IS NULL`）だけを対象に、`fetch_derivatives`関数が一時ファイルとしてダウンロード→2560px JPEGへ変換→`derivatives/`ディレクトリへ保存し、**変換直後に一時ファイルを削除**する
3. **オリジナルはローカルに一切保持しない**。原本はDriveにのみ存在し、ローカルに残るのは表示用に変換した2560px JPEG（`derivatives/{drive_id}.jpg`）だけ。**Driveに写真を置く/消すだけで画面に反映**され、rclone・cronは不要
4. Drive側で削除された写真は次回索引時にDBから削除され、対応するderivativeファイルも掃除される

セットアップ（1回だけ）:

1. https://console.cloud.google.com → 新規プロジェクト作成（名前任意、例: geoframe）
2. APIとサービス → ライブラリ → **Google Drive API** を有効化
3. APIとサービス → 認証情報 → 認証情報を作成 → **サービスアカウント**（ロール設定は不要）
4. 作成したサービスアカウント → キー → 鍵を追加 → **JSON** → ダウンロードし、`app/drive_key.json` として保存
5. サービスアカウントのメールアドレス（`xxx@<project>.iam.gserviceaccount.com`）をコピーし、**DriveのGeoFrameフォルダを「共有」→ このアドレスへ「閲覧者」権限で共有**
6. `pip install google-auth google-api-python-client`
7. `python server.py` → `drive_key.json`が置かれていれば起動時のOFFメッセージ（`[drive] drive_key.json なし → Drive直接参照OFF（ローカルのみ）`）が出ず、同期サイクルのログに `[drive] 索引: リモート{件数} 追加{n} 削除{n}` が出れば索引が動いている（差分が無いサイクルではこのログ自体出ない）。認証・索引・取得の各段階で失敗すると、それぞれ `[drive] 認証エラー: ...`／`[drive] 索引エラー: ...`／`[drive] 取得エラー: ...` がログに出る

設定: `GEOFRAME_DRIVE_FOLDER`（フォルダ名/ID、既定 GeoFrame）、`GEOFRAME_DRIVE_POLL_MIN`（監視間隔分、既定10）、`GEOFRAME_DRIVE_KEY`（キーJSONパス）。drive_key.jsonが無ければ従来どおりローカルフォルダのみで動く。**キーJSONは共有・公開厳禁**（Driveの閲覧権限そのもの）。

### 【代替】rclone方式のセットアップ手順（ディスプレイ機側）

```bash
# 1. rclone導入
curl https://rclone.org/install.sh | sudo bash
# 2. リモート設定（対話式。remote名: gdrive, type: drive, scope: drive.readonly推奨）
rclone config
# 3. 動作確認
rclone ls gdrive:GeoFrame
# 4. 定期同期（crontab -e で10分毎。--delete-during でDrive側の削除を反映）
*/10 * * * * rclone sync gdrive:GeoFrame <GeoFrame>/photos/gdrive --delete-during -q
```

**重要**: syncの同期先は必ず `photos/gdrive`（サブフォルダ）にすること。`photos/` 直下を指定すると--delete-duringが手動配置した写真を全削除する。server.pyはサブフォルダも走査するのでこれで問題ない。フロントは5分毎にmanifestを再取得するため、同期された写真は再起動なしで表示に加わる。

運用: スマホ/PCからDriveの `GeoFrame` フォルダに写真を放り込むだけ。iPhoneのHEICもpillow-heif導入済みならそのまま表示される。

### GPS位置の時刻推定（2026-07-28実装）

カメラ写真（Sony等）にGPSが無くても、**撮影時刻が近いGPS付き写真（同行iPhone）から位置を借りて推定**する。実写真で検証済み（DSC04055 → 1分後のiPhone写真から鳴門町土佐泊浦と推定）。表示には「≈（時刻から推定）」を付記。

- 許容差: `GEOFRAME_INFER_MIN`（デフォルト15分）
- カメラ時計のズレ補正: `GEOFRAME_INFER_OFFSET_MIN`（分、±）— 推定が全然当たらない場合はカメラの時計がズレている（UTC設定含む）疑いがあるので、ここで補正する
- `python server.py --organize` で「GPSなし・推定も不可」の写真を `_excluded/no_gps/` へ一括退避できる（RAW/動画/サイドカーの退避は実施済み）

## 5. ADS-B（tar1090）連携 — 実装済み（2026-07-28）

自宅で稼働中のtar1090（`GEOFRAME_ADSB_URL`で変更可）の `aircraft.json` をserver.pyが取り込み、以下を実装。実フィードでE2E検証済み（JAL5 JFK→羽田便が鎌倉の写真とMEMORY LINK発火を確認）。

- **常時レーダー表示**: 地図上に機影（針路回転・コールサイン・FL表示）＋AIR TRAFFICパネル（ルート・距離）
- **レア機体アラート**: 型式ウォッチリスト（AN-124, B747-8, C-17等）／軍・政府系オーナー／緊急スコーク7500/7600/7700 → マゼンタ警報＋機体へフライト＋機体写真（planespotters）
- **MEMORY LINK（行き先×写真マッチ）**: 便のcallsign→adsbdb APIで到着空港を解決し、写真の撮影地点が空港から80km以内（`GEOFRAME_MATCH_KM`）なら「この便は〇〇行き、あなたはそこに行ったことがある」→ 琥珀色演出でその写真を表示
- 設計注意: adsbdbは無料APIのため**新規問い合わせは1ポーリング3件に制限し結果は永続キャッシュ**（404も負キャッシュ）。機体情報が薄いフィード（r/t無し）でもadsbdbのhex照会で補完
- クールダウン: レア30分/機体、マッチ2時間/便（同じ演出の連発防止）

## 6. AIレイヤ（Gemini API + OpenClaw）— Phase 3方針

利用可能と確認した手札: **Gemini API**（従量）と**ローカル稼働中のOpenClaw**。

**現状: 未着手。** `catalog.db`のSCHEMAには`ai_caption`列が定義済み（`title TEXT, ai_caption TEXT, -- Phase3(Gemini)用`）だが、この列に値をUPDATE/INSERTする実装コードはserver.py中に存在しない。つまりGemini連携は起動しておらず、`ai_caption`は常時NULLのままである。以下は当初からの構想であり、実装はこれから。

- **Gemini本命用途は「写真の自動タイトル・解説」**: 現状タイトルはファイル名。Gemini（vision）でサムネイルからタイトルと一言解説を生成し、EXIF地名と合成すれば表示品質が一段上がる。夜間バッチ＋永続キャッシュで実質コストほぼゼロ
- Gemini第2用途: イベントナレーション（「AN-124が接近中。世界最大級の輸送機…」等）を発火時に1回生成
- **OpenClawは「通知と対話の出入口」**: レア機体・MEMORY LINK発火時にserver.pyからOpenClawへwebhook → 普段使いのチャットに写真付き通知。逆方向は「いまの飛行機なに？」「昨日の巡回どこ行った？」をチャットから照会。地理空間MCPをOpenClawに繋げば対話モード（Phase 3）を自前実装なしで実現できる
- 忖度なしの注意: OpenClawはインターネット非公開（LAN内）運用を厳守すること。公開インスタンスの情報漏えい事例が報告されている。GeoFrameのserver.pyも同様にLAN内限定（認証なし設計のため）

## 7. ハードウェア留意（RPi400 / N100）

- **N100ミニPCを主ターゲット推奨**。WebGLのflyTo演出が余裕で動き、開発機とほぼ同じ挙動になる
- **RPi400はPi 4世代のGPU（VideoCore VI）**。無印モードでフレーム落ちの可能性が高いため、`?low` 軽量モード（実装済み: pitch無し・エフェクト削減・飛行短縮）で起動すること。それでも重い場合はflyToをやめてクロスフェード切替に落とす改修が次の手
- どちらもChromiumキオスク起動: `chromium --kiosk --noerrdialogs --disable-session-crashed-bubble "http://localhost:8080/?low"`
