# 設計検討: 写真カタログDB（SQLite）とメタデータ分離アーキテクチャ

> [!IMPORTANT]
> 本文には「DBはDriveから再構築可能」とした初期設計が残っていますが、現在の `catalog.db` は手動GPS修正・お気に入り・有料生成したAI派生データも含みます。運用上は再構築不可能な一次データとしてバックアップしてください。

作成日: 2026-07-28 / ユーザー提案「アップロード前にローカル解析→DB登録→Drive登録で、ピンポイント取得と集計を可能に」の評価と設計

---

## 1. 提案の評価（忖度なし）

**方向性は正しい。** メタデータ（小さい・検索したい）とブロブ（大きい・たまに読む）の分離はカタログ＋オブジェクトストアの王道パターンで、写真が増えるほど効く。「全部ミラー」は600枚・7GBの現在は成立するが、1万枚・100GBでは破綻する。集計（都道府県制覇、月別、旅行単位）もディレクトリ走査＋JSONキャッシュでは限界が来る。SQLite導入は賛成。

ただし、原案のまま実装すべきでない点が2つある。

**修正提案①: 「アップロード前解析」を必須ゲートにしない。**
解析→DB登録→アップロードの直列パイプラインにすると、写真は必ずMacの解析ツールを通らないと表示されなくなり、「スマホからDriveに雑に放り込む」という当初要件が死ぬ。DBに無い写真が発生した瞬間に「DBとDriveどちらが正か」問題も生まれる。あるべき姿は:

- **Drive＝ブロブの唯一の真実、DB＝再構築可能な索引（キャッシュの昇格版）**
- 索引化は「誰が最初に見つけてもよい」— サーバーがDrive監視で自動索引化するので、事前解析なしの写真も勝手にDBに入る
- Mac側の事前解析ツールは「任意の前処理」（大量初回投入の高速化、将来の手動キュレーション）に格下げ

これを可能にする鍵が調査で確認できた: **Drive API v3の `imageMediaMetadata` が、ファイル一覧取得だけで撮影時刻・GPS（location.latitude/longitude/altitude）・カメラ情報を返す**。つまりブロブをダウンロードせずに全ライブラリを索引化できる。「アップロード前に解析しないと位置がわからない」という前提自体が不要（HEICで欠ける場合や時刻推定が必要な場合のみ、その1枚をダウンロードして自前解析にフォールバック）。

**修正提案②: ローカルに置くのはオリジナルではなく表示用derivative。**
ピンポイント取得＋LRUキャッシュより先にやるべき最適化がこれ。24MP・15MBのJPEGをRPi400のブラウザでデコードさせるのは遷移のたびに重い。取得時に**表示用2560px JPEG（1〜2MB）＋サムネイル400px**を生成してローカル保持すれば、(a)容量1/10でLRU不要になる規模が大幅に伸びる（1万枚でも15GB程度）、(b)RPi400の描画が軽くなる、(c)オリジナルはDriveにだけ在る＝ローカル消失が怖くなくなる。

## 2. 採用アーキテクチャ

```
[スマホ/PC] --雑に置く--> [Google Drive: GeoFrame/]  ←ブロブの真実
                              │ files.list (imageMediaMetadata: GPS/時刻)  ※DL不要
                              ▼
[server.py] ── 索引ループ（10分毎）
   ├─ catalog.db (SQLite) ←ここが「真実の索引」。ただし全行Driveから再構築可能
   │    位置なし写真は時刻推定(既存ロジック)をSQLで再計算
   ├─ 未取得/更新ブロブを必要に応じてDL → derivatives/ に2560px+thumb生成
   ├─ 地理院enrich（地名/標高）・将来: Gemini(タイトル/解説)もDB列に追記
   └─ /api/photos, /api/stats … はすべてSQLite読み出しに置換
[ローカル手動フォルダ photos/] も同じ索引ループが対象（source列で区別）
```

原則: **単一ライター**（DBに書くのはserver.pyのみ）。Mac事前解析ツールを作る場合もDBへ直接書かず、Driveへ`メタデータ.jsonl`を置いてサーバーに食わせる。DB破損時は削除すれば全再構築される（冪等）。

## 3. スキーマ（v1 → 実装確定）

以下は設計時点のv1案（現行の実装スキーマは後述）。

```sql
CREATE TABLE photos (
  id INTEGER PRIMARY KEY,
  source TEXT NOT NULL,              -- 'drive' | 'local'
  drive_id TEXT UNIQUE,              -- DriveファイルID（ピンポイント取得キー）
  path TEXT,                         -- ローカル/derivativeの相対パス
  name TEXT NOT NULL,
  md5 TEXT,                          -- 変更検知
  taken_at TEXT,                     -- ISO8601（EXIF）
  lat REAL, lon REAL,
  loc_source TEXT,                   -- 'exif' | 'inferred' | 'drive_meta' | NULL
  inferred_from INTEGER REFERENCES photos(id),  -- 推定の錨（追跡可能に）
  place TEXT, pref TEXT, muni_cd TEXT, elev REAL,   -- 地理院enrich
  title TEXT, ai_caption TEXT, ai_at TEXT,          -- Phase3: Gemini
  status TEXT NOT NULL DEFAULT 'ok', -- 'ok'|'no_geo'|'error'
  indexed_at TEXT, fetched_at TEXT   -- derivative取得済みか
);
CREATE INDEX idx_photos_time ON photos(taken_at);
CREATE INDEX idx_photos_geo  ON photos(lat, lon);
CREATE INDEX idx_photos_pref ON photos(pref);
```

実装ではこのv1案からスキーマが変化している。実際に採用されているスキーマは `app/geoframe/store.py` の `SCHEMA` 定数を正とする。

```sql
CREATE TABLE IF NOT EXISTS photos(
  id INTEGER PRIMARY KEY,
  source TEXT NOT NULL,             -- 'drive' | 'local'
  drive_id TEXT UNIQUE,             -- DriveファイルID
  path TEXT UNIQUE,                 -- ローカル原本の相対パス（drive行はNULL）
  display TEXT,                     -- 表示用ファイル（derivative等）の相対パス
  name TEXT NOT NULL,
  md5 TEXT, mtime REAL,
  taken_at TEXT,                    -- 'YYYY-MM-DD HH:MM'
  lat REAL, lon REAL,
  loc_source TEXT,                  -- 'exif' | 'drive_meta' | 'inferred' | NULL
  inferred_from INTEGER,
  place TEXT, elev REAL,
  title TEXT, ai_caption TEXT,      -- Phase3(Gemini)用
  indexed_at TEXT, fetched_at TEXT
);
CREATE INDEX IF NOT EXISTS idx_photos_time ON photos(taken_at);
```

さらに起動時のマイグレーション（同ファイル200〜208行目、`ALTER TABLE photos ADD COLUMN`）で以下3列が追加される。

```
muni_cd TEXT, pref TEXT, city TEXT
```

**実装で採用された変更（v1案からの進化）:**
- `display`（表示用ファイルの相対パス）列を新設。それに伴い`path`はローカル原本専用（drive行はNULL）に役割が絞られ、`UNIQUE`制約が付いた
- `mtime REAL`列を新設し、`md5`と併用して変更検知を行う
- `city`列を、`pref`・`muni_cd`と一緒にマイグレーションで追加（v1案では`pref`・`muni_cd`のみだった）

**未実施のまま残っている提案（v1案にあったが実装されていない）:**
- `status TEXT NOT NULL DEFAULT 'ok'`列 — 未実装
- `ai_at TEXT`列（Gemini処理日時） — 未実装（`ai_caption`列のみ存在）
- `idx_photos_geo (lat, lon)`インデックス — 未作成
- `idx_photos_pref (pref)`インデックス — 未作成
- 実際に存在するインデックスは`idx_photos_time (taken_at)`のみ

`.cache.json` の geo:/exif2:/drivemd5: キーはこのDBに統合して廃止（adsbdb系キャッシュのみ残す）。

## 4. これで可能になる活用（提案の狙いの具体化）

- **ピンポイント取得**: 「この空港80km圏の写真」= `WHERE 距離式 < 80km` → drive_idでその場でDL。MEMORY LINKの全ライブラリ化（現状はローカルにある写真のみが対象）
- **自分統計（Phase 1.5がSQL数本になる）**: 都道府県制覇マップ `SELECT pref, COUNT(*)`、月別撮影ヒート、年間移動距離、時間帯分布
- **旅行クラスタ**: `app/geoframe/photos.py` の `assign_trips` が撮影時刻の間隔（48時間超）で旅行を分割し、日付・主な地名・boundsを付与する。
- **再生ローテーション**: `app/static/js/scheduler.js` の `buildPlayOrder` が各tripから `TRIP_MAX` 件ずつ取り出し、大きな旅だけに再生が偏らないよう周回位置を保存する。
- **`--organize`オプション**: `app/geoframe/drive.py` の `organize_photos` が、ローカルの表示不能写真（GPS無し・推定も不可）を `_excluded/no_gps/` へ退避する。
- **表示モードの多様化**: 「1年前の今日」「この場所の過去写真」「未訪問県の可視化」
- **Gemini解説のバッチ管理**: `WHERE ai_caption IS NULL` を夜間に順次処理、結果はDBに永続

## 5. 移行計画

| 段階 | 内容 | 規模 |
|---|---|---|
| M1 | catalog.db導入・索引ループ（Drive imageMediaMetadata＋ローカルEXIF）・/api/photosのDB化・時刻推定のDB化（完了） | server.py改修 中 |
| M2 | derivative生成・表示をderivativeに切替（完了。ただし下記注記参照） | 小〜中 |
| M3 | /api/stats＋統計画面（Phase 1.5合流・完了） | 中 |
| M4 | オンデマンドDL＋LRU（ライブラリがディスクを超えたら。当面不要の可能性大） | 中 |

※M2について: 実装されているのは表示用2560px JPEGの生成（`make_derivative`関数）のみ。設計時に想定していた400pxサムネイルの生成は未実装。

リスク: imageMediaMetadataのGPSがHEICで欠ける可能性（→DL解析フォールバックで吸収、実写真で要検証）。SQLite同時書き込み（→単一ライター原則とWALで回避）。

## 5.5 セキュリティ

実装では、機密ファイルがHTTP経由で誤って配信されないよう `app/geoframe/handlers.py` が配信可能な静的パスをホワイトリスト化し、ドットファイルとディレクトリ一覧を拒否する。

## 6. 結論

提案採用。ただし「アップロード前解析の必須化」ではなく、Driveのブロブを元にサーバーが索引を自動構築し、ローカルには表示用derivativeを保持する。写真の再検出はできるが、手動修正・評価・AI派生データは再構築できないため、DB自体は一次データとして扱う。
