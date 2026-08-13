# ambient_core の契約 — 次フェーズ（1A / 1B / 1C / 1D）へ

## この文書の位置づけ

次フェーズは 4 人が並列で実装に入ります。**この文書に書かれた契約は、
実装フェーズ中に切り直さない前提**で書かれています。

型・シグネチャは既に凍結されていて、中身がスタブなだけです。
スタブを実装で置き換えるのが次フェーズの仕事であって、**シグネチャを
変えるのは仕事ではありません**。変える必要が出たら、独断で変えずに
他の 3 レーンへ影響を確認してください。

**ヘッダのコメントが正典です。** この文書はその要約と、複数のヘッダに
またがる話（レーン分担・決めていないこと）を扱います。食い違ったら
ヘッダを信じ、この文書を直してください。

前提として `docs/PORTABILITY.md` を先に読んでください。`ambient_core` に
書いてよいもの／いけないものはあちらが正典です。

---

## 1. 全レーン共通の規律

| 規律 | 理由 |
|---|---|
| 標準ライブラリのみに依存 | Tab5 (ESP32-P4) へそのまま持っていく層だから |
| `std::filesystem` 禁止 | ESP32 の VFS が POSIX セマンティクスを満たさない。ファイル I/O は `IStateStore` の背後だけ |
| `std::thread` / `std::mutex` / `<future>` / `<condition_variable>` 禁止 | Core は単一スレッド前提。並行性は Renderer / Transport の責務 |
| RTTI 禁止 (`-fno-rtti`) | `typeid` / `dynamic_cast` は**コンパイルが通らない**。仮想関数 / `std::variant` + `std::visit` / タグ enum を使う |
| 例外を制御フローに使わない | ESP32 は緊急用プールが 0。ヒープ枯渇時の `throw` が `abort()` になる。戻り値 / `std::optional` で表現する |
| `<format>` を避ける | コードサイズ。Tab5 の flash 予算に効く |
| 大きな静的バッファ・深い再帰を避ける | Tab5 のスタックは小さい（FreeRTOS タスクごとに数 KB） |
| 写真のピクセルデータを Core に持たせない | 画像バッファはサイズが桁違いで確保戦略もプラットフォーム依存。Core は識別子とメタデータだけ |

**検証**: 変更したら必ず実行してください。

```bash
./native-display/tools/check-esp-portability.sh
```

`ambient_core` を Tab5 の実ツールチェーン（riscv32 GCC 14.2.0）でコンパイルします。
実機には接続しません。**ヘッダを足したら、それを include する `.cpp` が
最低 1 本あるようにしてください。** どの `.cpp` からも include されない
ヘッダはゲートの対象にならず、「通ったつもり」になります。

---

## 2. 乱数 — 最重要

### 2.1 標準の分布クラスを使わない

`std::uniform_int_distribution` などは **規格が実装アルゴリズムを規定していません**。
同じ seed から Mac (libc++) と ESP32 (libstdc++) で違う値が出ます。
バグではなく規格どおりの挙動です。

エンジンも自前です（xoshiro128\*\*）。`std::mt19937` は移植性はありますが
状態が 2.5KB あり Tab5 の RAM / NVS 予算に対して重すぎます。

`ambient/core/rng.hpp` の `belowN()` / `unit()` を使ってください。

- `belowN(n)` は棄却法で**剰余バイアスを除去済み**。`next() % n` と書かないこと
- `unit()` は **24bit 仮数**。IEEE 754 binary32 で厳密に表現できる値しか返さないので、
  Mac と Tab5 で完全に同じビット列になる

### 2.2 用途ごとに別の型

```cpp
using SchedulerRng  = Rng<rng_domain::Scheduler>;   // 別の型
using TransitionRng = Rng<rng_domain::Transition>;  // 別の型
```

取り違えて渡すと**コンパイルエラー**になります。これは意図です。

共有すると、**一方の呼び出し回数を変えただけでもう一方の乱数列が丸ごとずれます**。
「トランジションを 1 種類増やしただけで写真の再生順が変わった」という、
原因追跡の極めて困難な結合が生まれます。

用途を増やすときは `rng_domain` にタグを足して `using` を 1 行足してください。
既存の用途と共有してはいけません。

### 2.3 ★消費順序が契約である ← ここが最も落とし穴

**「Params を core 側で導出する」だけでは不十分です。**

同じ seed から同じシーケンスを得るには、**全消費者が同じ順序で・同じ回数**
引く必要があります。1 箇所でも引く回数が変わると、以降の乱数列が全部ずれます。

そして **この違反は Mac 上のテストでは絶対に検出できません。**
Mac 側は常に自己整合するからです。発覚するのは **Tab5 に載せた最終フェーズ**で、
そこで原因を特定するのは極めて困難です。

現時点で確定している消費仕様:

| 関数 | 引くもの | 回数 |
|---|---|---|
| `photoPriority()` | `SchedulerRng::unit()` | 写真 1 枚につき **常に 1 回**（入力によらず） |
| `shuffleTrips()` | `SchedulerRng::belowN()` | **groups.size() - 1 回**（0 or 1 グループなら 0 回） |
| `buildPlayOrder()` | 上記の合計 | `(groups - 1) + そのラウンドで評価した写真数` |
| `syncSchedule()` | `SchedulerRng::belowN(kNewInsertWindow)` | **新着 1 枚につき 1 回** |
| `pickTransition()` | `TransitionRng::belowN()` | **常に 1 回**（重みが全 0 でも 1 回） |

呼ぶ順序（`buildPlayOrder`）:

1. `shuffleTrips`（トリップ順のシャッフル）
2. 各写真の `photoPriority` ← **シャッフル後**のグループ順、グループ内は配列順
   - ★グループ内の**全**写真を評価する。`TRIP_MAX` で絞るのは評価の**後**。
     「上位 6 枚だけ評価」に変えると消費回数が変わり、列がずれる

**対策（必須）**: `Rng::draws()` と `Rng::steps()` を golden trace テストで固定してください。

- `draws()` — 論理呼び出し回数。`belowN()` が棄却で何回エンジンを回しても +1。
  **仕様に書くのはこちら。**
- `steps()` — エンジン前進回数。エンジン実装が変わっていないことの検出用。

`tests/unit/test_rng.cpp` の冒頭に、書くべきテストの一覧があります。

**消費順序を変えるときは、この表・ヘッダのコメント・golden trace テストを
同時に直してください。** 片方だけ直すと契約が嘘になります。

### 2.4 状態のシリアライズ

`RngState` は `SchedulerSnapshot` に含めて永続化します（再起動をまたいだ再現性）。
`save()` / `restore()` で往復してください。

`TransitionRng` は保存しません（演出の見た目にしか効かないため）。
保存が必要になったら、そのときに決めてください（現時点では**未決定**）。

---

## 3. 永続化 — 容量に上限がある

`IStateStore` (`ambient/core/state_store.hpp`) の背後でしかファイルに触りません。

**Mac のファイルは実質無制限。Tab5 の NVS はパーティション 20KB、
しかも hogo-meter が既に 4 つの namespace を使用中です。**

「無限に書ける」前提で書くと、Mac では全部通り、Tab5 で初めて落ちます。
そこで次の 2 つをインターフェースに出してあります。

- `capacity()` — 総量 / 使用量 / 1 値あたり上限 / **キー長上限**
- `StoreStatus` — `OutOfSpace` / `ValueTooLarge` / `KeyTooLong` を戻り値で表現

**`write()` の戻り値を必ず見てください**（`[[nodiscard]]` を付けてあります）。
`OutOfSpace` を握り潰す実装は、Tab5 で静かにデータを失います。

**キー長の上限は 15 文字**です。旧実装の `geoframe.play.v1` は 16 文字あり
**Tab5 では通りません**。`ambient/core/photo/snapshot.hpp` の `kSnapshotKey`
は `"gf.play.v1"` にしてあります。

テスト用に `MemoryStateStore` を Core に置いてあります。容量を明示的に
設定できるので、**Mac 上で `OutOfSpace` 経路をテストできます**。

`flush()` は NVS の `nvs_commit()` に対応します。**`write()` ごとに commit すると
フラッシュ寿命を削ります。** まとめてから flush してください。

---

## 4. 時刻 — スリープ復帰

`ambient/core/clock.hpp`。

### 4.1 単調時刻と壁時計を混ぜない

| | 用途 |
|---|---|
| `steadyNow()` → `SteadyTime` (ms) | 経過時間の計測。トランジションの progress、クールダウン、トースト猶予、ポーリング間隔 |
| `wallNow()` → `WallTime` (epoch 秒) | 「いつの出来事か」。地震の `at`、写真の撮影日時、お世話の記録時刻 |

**別の型**なので取り違えるとコンパイルエラーになります。
`WallTime` は**秒**、`SteadyTime` / `Millis` は**ミリ秒**です。

### 4.2 macOS の単調時計はスリープ中に停止する

`mach_absolute_time`（= `std::chrono::steady_clock` の実体）は
**システムスリープ中に止まります**。ノートを閉じて 8 時間後に開くと、
単調時刻は 8 時間ぶん進んでいません。

このままだとポーリング予定・トースト・クールダウンが実時間とずれます。
トーストに至っては 8 時間表示され続けえます。

**検知は Core の `TimeJumpDetector` が行います。** 壁時計の進みと単調時刻の
進みの差 (skew) を見るだけの純粋ロジックです。

```cpp
auto jump = detector.update(clock.steadyNow(), clock.wallNow());
if (jump) { /* ポーリング予定・トースト・クールダウンを組み直す */ }
```

- `skew > 0` — 壁だけ進んだ = 単調時計が止まっていた（スリープ復帰）
- `skew < 0` — 壁が巻き戻った / 飛んだ（NTP 初回同期、手動時刻変更）

**`IClock` の実装は生の値を返すだけにしてください。** 跳躍判定などのポリシーを
実装側に持ち込むと、Mac と Tab5 でポリシーが分岐してテストできなくなります。

既定しきい値は 2000ms です。壁時計が秒精度である以上 1000ms 程度のずれは
通常運転で普通に起きるので、これより小さくすると誤検知で再スケジュールが暴発します。

テスト用に `FakeClock` を Core に置いてあります。`simulateSleep()` が
「壁だけ進み単調が止まる」を再現します。

---

## 5. 優先度アービター — レベルトリガであってイベントキューではない

`ambient/core/state/`。

### 5.1 なぜイベントキューにしないのか

表示すべきものの優先順位は 7 段あり、**そのうち 6 つは状態（レベル）条件**です。
「Red である」「記録が保留中である」「通信が切れている」は真であり続けます。

イベントキュー方式にすると、次の 2 つを手で書く羽目になります。

1. レベル条件をエッジ（変化した瞬間）に変換する
2. **上位に抑止されたイベントを、抑止が解けたときに再配達する仕組み**

2 がまさに防ぎたいバグの温床です。「地震アラート中に Red になった。
アラートが明けたら Red を出す」を再配達キューで正しく書くのは難しく、
取りこぼしても普段は表に出ません（次の変化で復帰してしまうので、
「たまに出ない」になって再現条件を掴めません）。

**レベルトリガなら再配達は要りません。** 毎 tick 全員に「今どうしたい？」と聞き、
一番強いものを選ぶだけです。抑止が解けた tick で自動的に選ばれます。

真にエッジなのは**地震だけ**（新しい地震 ID が現れる瞬間）で、
これは `AlertEngine` が **エッジ→レベルの変換器**として吸収します。
境界がコード上の 1 箇所に固定されます。

### 5.2 `decide()` は純粋関数

```cpp
std::optional<Decision> decide(std::span<const Claim> claims, SteadyTime now) noexcept;
```

副作用なし、メンバ状態なし、隠れた入力なし。何度呼んでも同じ答えです。
これで「claim をこう並べたらこれが勝つ」を表で書けます。

選択規則（**契約**）:

1. `activeAt(now)` が false の claim（`notBefore` 前 / `expiresAt` 後）は候補外
2. 残りのうち `priority` が最大のものが勝つ
3. **同一優先度は span 内で先に現れたものが勝つ**（安定選択）
   - 呼び出し側が claim を集める順序で結果が決まるので、
     **source の登録順を安定させてください**
   - 「同点なら sourceId が小さい方」のような暗黙規則を後から足さないこと

`nullopt` = 有効な claim が 1 つも無い。**通常は起きません**（写真の source が
常に claim を出す前提）。フォールバック表示は `StateMachine` のポリシーで、
`decide()` の責務ではありません。ここで既定値を返すと異常が見えなくなります。

状態を持つ層は `StateMachine` に分けてあります。**ここに優先順位のロジックを
書かないでください。** 混ぜるとテストが履歴依存になり決定性が失われます。

### 5.3 `IClaimSource::collect()` は const

条件が真である**間ずっと**同じ claim を出し続けてください。
「1 回出して消える」実装にすると、上位に抑止されている間に claim が消滅し、
結局イベントキューと同じ再配達問題を抱えます。

`collect()` は `const` です。入力の取り込み（例: `AlertEngine::ingest()`）は
別のメソッドで行ってください。

### 5.4 表示状態

`Photo` / `Env` / `Degraded` / `Care` / `CareDue` / `RecordedAwaitingServer` / `Alert`

**`RecordedAwaitingServer` は企画書に無いが必要と判明した状態です。**

お世話記録を送信して**成功した直後**の状態です。記録の状態色（Red/Yellow/Green）は
**サーバ側が計算して返す**ため、POST が 200 で返っても
**次の config poll（最大 30 秒後）まで色は Red のまま**です。

> ★「記録したら即 Green」は成立しません。

この状態は「ローカルの記録時刻を経過時間表示に反映しつつ、色はサーバ待ち」を
表現します。次の poll が届いたら `Care` / `CareDue` へ落ちます。

---

## 6. お世話記録の保留 — 1 つの不変条件

`ambient/core/care/pending_record.hpp`。

> **保留は「取り消しボタンへのヒット」でのみ破棄される。
> それ以外のすべての離脱経路は、離脱前に確定する。**

**「3 つの確定トリガ」として実装してはいけません。** 経路は後から増えます
（設定画面への遷移、無操作タイムアウト、画面ロック……）。増えるたびに
「ここでも確定を呼ぶ」を思い出す運用は、必ず 1 箇所忘れます。
忘れるとユーザが押した記録が**黙って消えます**。しかも画面に出ないので
「たまに記録されない」というバグになり、再現条件が掴めません。

実装は次の形になっています。

- 離脱の出口は `leave(PendingExit)` **1 箇所だけ**
- 中身は「理由が `UndoHit` なら破棄、それ以外は確定」の 1 行
- **既定が「確定」**なので、書き忘れる余地がない

**新しい離脱経路を足すときは、`PendingExit` に値を足して `leave()` を
呼ぶだけです。** `discardsPending()` に条件を足さないでください。
破棄する経路は永久に 1 つだけです。

既知の離脱経路（これで全部とは限りません。だから不変条件として書いています）:
トースト満了 / 別カードのタップ / 設定画面への遷移 / 無操作タイムアウト / アプリ終了。

猶予は 2 秒（`constants::kToastTotal`、出典 hogo-meter `tab5/ui.h:26`）。

---

## 7. トランジション

`ambient/core/transition.hpp`。

### 7.1 遷移と滞在は別の型

- `TransitionKind`（5 種、600〜1000ms）: CrossFade / FadeThroughBlack / Slide / Wipe / Blinds
- `DwellEffect`（約 20 秒）: KenBurns

同じ enum に押し込んで同じ `progress` で駆動すると、**種別ごとに `progress` の
意味が変わります**。CrossFade の 0.5 は「A と B が半々」、KenBurns の 0.5 は
「1 枚のズーム途中」です。消費側（Renderer）が全種別の分岐を持つ羽目になります。

そもそも直交した概念です（KenBurns しながら CrossFade で抜けることがある）。

### 7.2 ★進捗は経過時間から算出する ← 最重要級

Tab5 は Mac より遥かに低い fps でしか描けない可能性が高いです。

| | フレーム数駆動 | 時間駆動 |
|---|---|---|
| Mac 60fps | 0.5 秒で完了 | 600ms で完了 |
| Tab5 15fps | **2.0 秒かかる**（間延びする） | 600ms で完了（カーブを疎にサンプルするだけ） |

これを規約ではなく**型で強制**しています。

```cpp
class Progress {
  Progress() = delete;                                    // 既定構築なし
  static constexpr Progress fromElapsed(Millis elapsed, Millis duration);  // 唯一のファクトリ
};
```

**フレーム番号を渡す経路が存在しません。** `Progress` に別のファクトリを
善意で足さないでください。

### 7.3 blend / 領域コピーのタグ分けと重みテーブル

Tab5 のハードウェアアクセラレータの blend 性能は **未測定**です。

| 分類 | 演出 |
|---|---|
| `CompositeClass::Blend` | CrossFade / FadeThroughBlack / KenBurns |
| `CompositeClass::RegionCopy` | Slide / Wipe / Blinds |

実測して blend が重いと分かったら、**`TransitionWeights` の数値だけ書き換えて**
blend 群の出現頻度を下げてください。`pickTransition()` の実装も Renderer の
コードも変えません。

`pickTransition()` は**重みが全て 0 でも乱数を 1 回引きます**。
引く回数を入力依存にすると「重みを 0 にしたら以降の乱数列がずれた」という
事故になるためです。

---

## 8. HTTP — ステータスで分岐してから parse する

`ambient/core/http.hpp`。

### 8.1 `IHttpClient` が core にある理由

移植したい**純粋なポリシー**（送信キュー管理、リトライ規律、優先制御）は
I/O を含まない判断ロジックなので core にあるべきです（`docs/PORTABILITY.md` が
明言しています）。そのポリシーが `IHttpClient` に依存する以上、
依存される抽象も core になければなりません。

curl (Mac) / HTTPClient (Tab5) という具体は transport 層の実装詳細です。

副次的効果として、**fixture 駆動のテストと実通信が同一のクライアントコードを
通ります**。「fixture でだけ動く経路」が原理的に生まれません。

### 8.2 ★エラー応答は JSON とは限らない

GeoFrame server の Host 検証で拒否された 403、静的ファイルのホワイトリスト外 403、
404 は **HTML のエラーページ**が返ります。

「どんな応答でも JSON parse する」実装は、**最も診断が必要な場面でクラッシュします**。
403 が返っているのに「JSON parse エラー」しか出ない、という最悪の体験になります。

そこで `IHttpClient` の戻り値は **ステータスコード + 生のボディ**です。
**パース済み JSON を返してはいけません。**

**パーサへ渡してよいのは `successBody()` の戻り値だけ**にしてください。

```cpp
if (auto body = successBody(response)) {   // 2xx かつトランスポート成功のときだけ
  parse(*body);
}
```

`response.body` に直接パーサを掛けるコードは、レビューで落としてください。

### 8.3 呼び出しの規律（hogo-meter から移植）

- **1 回の `update()` で `fetch()` を呼ぶのは最大 1 回**
- 未送信の record キューがあれば、**record 送信を config poll より優先**

1 tick の停止時間を 1 リクエストぶんに抑えるための規律です。
「まとめて 3 本投げる」は絶対にしないでください（描画が止まります）。

---

## 9. GeoFrame のデータ型 — 実測が正典

**企画書ではなく `ambient/core/model/` のコメントが正です。**

### `/api/photos`

- **裸の JSON 配列**。`{items: [...]}` ではない。
  パーサが`{items:...}`を期待していると多数の写真があっても0件として動き、
  しかもエラーにならない
- **`time` は `"YYYY-MM-DD HH:MM"` の文字列**。`takenAt` ではない。区切りは半角スペース
- **`file` は先頭スラッシュ「無し」の相対パス**

### ★URL 組み立ての非対称

| | 先頭スラッシュ |
|---|---|
| `/api/photos` の `file` | **無し** |
| `/api/admin/photos` の `thumb` | **あり** |

素朴に共通化すると、前者はパスが繋がらず、後者は `//` になり、**どちらも 404** です。
型で分けてあります（`PhotoRelPath` / `AdminAbsPath`）。
**共通の `joinUrl(base, std::string)` を作らないでください。**

#### ★この2つの型は対称ではありません（セキュリティレビューを受けた変更）

当初は対称な集約体でしたが、`AdminAbsPath` **だけ**が検証付きのクラスになっています。
理由は 404 ではなく **SSRF** です。

`thumb` の値は**認証の無い LAN 上のサーバ**から来ます。GeoFrame server は `0.0.0.0` に
バインドし認証を持たないため、LAN 上の任意の端末が応答を偽装できます。`AdminAbsPath` は
`base` の直後にそのまま連結されるので、先頭が `/` でないと **authority 部を乗っ取られます**。

| サーバが返す `thumb` | 生成される URL | 実際の接続先 |
|---|---|---|
| `"@evil.example/x.jpg"` | `http://192.0.2.5:8080@evil.example/x.jpg` | RFC 3986はauthorityを**最後の`@`**で分割 → **evil.example** |
| `".evil.example/x.jpg"` | `http://geoframe.local.evil.example/x.jpg` | **evil.example の DNS 配下** |
| `"//evil.example/x.jpg"` | プロトコル相対 | **evil.example** |

`PhotoRelPath` 側は `joinUrl` が**先に `'/'` を入れてから**連結するので、値が何であっても
authority 部に食い込めません。だから検証が要るのは `AdminAbsPath` だけです。

**したがって使い方が違います:**

```cpp
PhotoRelPath rel{"derivatives/x.jpg"};        // 集約体。既定構築できる
auto abs = AdminAbsPath::make("/api/...");    // std::optional を返す。失敗しうる
if (!abs) { /* 不正な thumb。どう扱うかは呼び出し側の判断 */ }
```

- `AdminAbsPath` は**既定構築できません**。構造体のメンバに持つなら
  `std::optional<AdminAbsPath>` にするか、コンストラクタで初期化すること
- 例外は使いません（`docs/PORTABILITY.md` の規律）。`make()` が `std::nullopt` を返します
- **未決定**: `/api/admin/photos` のパーサ担当は「不正な `thumb` を持つエントリを捨てるか、
  サムネイル無しとして扱うか」を決める必要があります

### `/api/env`

- トップレベルは `quakes` / `weather` / `weekly` の 3 キー。**`ok` フィールドは無い**
- quake の **`mag` と `maxi` は文字列**。`maxi` は震度なので **`"5-"` `"5+"` がありうる**
  - ★**`std::stoi` は使えません。** `parseIntPrefix()` を使ってください
- **`time` は `"HH:MM"` の 5 文字で日付が落ちています。** 24 時間窓なので
  「昨日 23:50」と「今日 23:50」が区別できません。
  **日跨ぎの同定には必ず `at` を使ってください**
- `at` は ISO 8601 固定幅。サーバ側で解析可能性が保証済みなので、
  substring → int の素朴なパーサで十分です。
  **`std::get_time` を使わないこと**（重く、ESP32 のロケール対応が薄い）。
  **`std::mktime` も使わないこと**（TZ 環境変数に依存する）

### `/api/roomenv`

- items のキーは `entity` / `value` / `unit` / `class` / `name`
- **`value` は常に文字列**（数値ではない）
- **`ok:false` は Home Assistant が未設定であることを意味します。**
  ★**障害ではなく「機能なし」**です。**Degraded にも STALE にもしないでください。**
  HA を使っていない家庭では常にこの状態で、そこで永続的に「通信異常」を出す端末は
  壊れて見えます

### `/api/presence`

- `night` は `true` / `false` / **`null`** の 3 値。`std::optional<bool>` で表現します
- `value_or(false)` で潰さないでください。null（判定不能）と false（昼）は別の情報です

---

## 10. 定数 — 値を変えない

`ambient/core/constants.hpp`。すべて稼働中の実装から移植した値で、
実運用で挙動が確認されています。各定数に出典（ファイルと行）を書いてあります。

`src/core/constants.cpp` に `static_assert` を並べてあるので、
値を変えるとビルドが落ちます。変えるなら出典側と同時に変え、この文書に記録してください。

特に注意:

- `RECORD_MAX_RETRIES = 5` は **初回 1 回 + 再試行 4 回 = 総試行 5 回**。「5 回再試行」ではない
- 地震のクールダウン 10 分は **グローバル**。同一イベント ID ごとではない
  （群発地震でイベントが連鎖しないためのもの。ID ごとに読み替えると意味を失う）
- `LABEL_MAX_BYTES = 18` は UTF-8 の**バイト数**であって文字数ではない（全角 6 文字）
- **記録の状態（0-3）と点灯要否はサーバが計算して返す。**
  Mac 側でしきい値計算を再実装しないでください。二重管理になり必ずずれます

---

## 11. レーンごとの担当ファイル

### 1A — 状態機械（優先度アービター、Clock）

| ファイル | 状態 |
|---|---|
| `src/core/state/arbiter.cpp` | `decide()` 実装済み。**選択規則を変えないこと** |
| `src/core/state/state_machine.cpp` | 実装済み。**薄いまま保つこと** |
| `src/core/state/alert_engine.cpp` | ★**スタブ。`ingest()` を実装する** |
| `src/core/clock.cpp` | `TimeJumpDetector` / `FakeClock` 実装済み |
| `src/core/care/pending_record.cpp` | 実装済み。**不変条件を壊さないこと** |
| `tests/unit/test_arbiter.cpp` | 7 段の網羅、レベルトリガの検証、群発クールダウン |
| `tests/unit/test_clock.cpp` | 跳躍後の再スケジュール |
| `tests/unit/test_care_pending.cpp` | 送信キュー連携、`RecordedAwaitingServer` 遷移 |

各 `IClaimSource` の実装（写真 / お世話 / 環境 / 通信状態）も 1A です。

`IClock` の **Mac 実装は Core の外**（app / platform 層）に置いてください。

### 1B — 写真スケジューラ、スナップショット永続化、乱数

| ファイル | 状態 |
|---|---|
| `src/core/photo/scheduler.cpp` | `photoPriority()` / `groupByTrip()` / `shuffleTrips()` 実装済み。`buildPlayOrder()` / `peekNext()` / `markShown()` / `syncSchedule()` が★**スタブ** |
| `src/core/photo/snapshot.cpp` | ★**全部スタブ。形式を決めるところから** |
| `src/core/model/photo.cpp` | `joinUrl()` 実装済み。`signature()` / `indexOf()` が★**スタブ** |
| `src/core/rng.cpp` | 実装済み（ヘッダ内テンプレート）。**エンジンを差し替えないこと** |
| `tests/unit/test_rng.cpp` | ★**golden trace テストを最優先で書く** |
| `tests/unit/test_photo_scheduler.cpp` | 全網羅、消費回数、スナップショット往復 |
| `tests/unit/test_state_store.cpp` | 容量超過、上限適用後の健全性 |

### 1C — SDL Renderer（描画骨格、テキスト、入力）

`src/render_sdl/**` が担当範囲です（**Core は変更しません**）。
Core 側で参照するのは `ambient/core/transition.hpp` と `ambient/core/state/display_state.hpp`。

| ファイル | 状態 |
|---|---|
| `src/core/transition.cpp` | `pickTransition()` 実装済み |
| `tests/unit/test_transition.cpp` | イージングカーブ、遷移と滞在の同時進行 |

イージングカーブなど演出の中身は Renderer 側に置いてよいですが、
**`Progress` を経由してください**。フレーム数駆動にしないこと。

### 1D — fixture供給層（`IHttpClient` のfixture実装）

fixture 実装そのものは **Core の外**（`tests/` か transport 層）に置いてください。
`ambient_core` は `IHttpClient` の抽象だけを持ちます。

| ファイル | 状態 |
|---|---|
| `src/core/model/parse.cpp` | `parseIntPrefix()` 実装済み。日時パーサが★**スタブ** |
| `src/core/http.cpp` | `successBody()` 実装済み |
| `tests/unit/test_parse.cpp` | 日時パース、日跨ぎの地震同定 |
| `tests/unit/test_http.cpp` | ★**403 の HTML 応答**、タイムアウト |

**`WeeklyDay` の形は旧実装の消費側コードから復元したもので、
サーバ応答を直接実測していません。** fixture を採取したときに確定させ、
ずれていたらこの文書に記録してください。

### 共有ファイル（変更前に他レーンへ確認）

- `native-display/CMakeLists.txt` — ソースを足すときは追記のみ
- `src/core/constants.hpp` — 値を変えない
- `docs/CORE_CONTRACTS.md`（この文書）

### テストを書くときの注意（一度は必ず踏むもの）

**enum を `CHECK(a == b)` で比較したい場合**、その enum に対する
`toString()` を `ambient::core` に用意してください。

doctest は値を文字列化するとき**非修飾の** `toString(x)` を呼びます。
`ambient::core` の enum は ADL でこちらの `toString` を拾うので、
無いと `"{?}"` としか出ず、失敗時に何と何を比べたのか分かりません。

`unit_tests` には `DOCTEST_CONFIG_DOUBLE_STRINGIFY` を定義してあります
（`CMakeLists.txt` に理由を書いてあります）。これが**無いとコンパイルが落ちます**。
外さないでください。

**ループの中で `CHECK` を撃たないこと。** 数万件の assertion が積み上がり
テストが遅くなります。違反を集計してから 1 回だけ検査してください
（`tests/unit/test_rng.cpp` が例です）。

---

## 12. ★決めていないこと（誰がいつ決めるか）

ここに挙げたものは**意図的に未決定**です。実測前に決めると根拠のない値が
固定されるためです。決めたらこの節から外して、該当する節へ移してください。

| 未決定の項目 | 決める人 | いつ | 現状 |
|---|---|---|---|
| **Tab5 の NVS で ambient が使える実バイト数** | Tab5 移植担当 | 移植時 | パーティション 20KB のうち hogo-meter の 4 namespace を引いた残り。**未実測** |
| **各遷移の実際の duration** | 2A / 2B | 演出実装時 | 600〜1000ms の範囲だけ確定。演出ごとの値は未定 |
| **`TransitionWeights` の最終値** | Tab5 移植担当 | blend 性能の実測後 | 現在は均等 `{1,1,1,1,1}`。偏らせる根拠がまだ無い |
| **`TransitionRng` の状態を永続化するか** | 2A | 必要になったら | 現在は保存しない（演出の見た目にしか効かないため） |
| **`sourceId` の値の割り当て** | 1A | source 実装時 | `AlertEngine::kSourceId` のみ確定。他は各 source が定数で持ち、衝突しないよう 1A が管理 |
| **不正な `thumb` を持つ admin エントリの扱い** | admin パーサ担当 | 実装時 | `AdminAbsPath::make()` が `nullopt` を返したとき、エントリを捨てるかサムネイル無しとして扱うか |

### Wave 1 で決着したもの（この節から外した）

**スナップショットの形式・`shown` の上限・上限超過時に何を捨てるか（1B が決定）**

判断の全文は `src/core/photo/snapshot.hpp` 冒頭にあります。要点:

- 形式は**長さ前置きの行指向テキスト**。ID は認証の無い LAN サーバ由来なので改行・コロンの混入を
  前提とし、エスケープではなく長さ前置きで扱う。数値は10進 ASCII（エンディアン依存を持ち込まない）
- 捨て方は**選択肢 A**（`round` の古い順、同点は `photoId` 昇順で決定的）
- **★上限機構だけでは Tab5 問題は解決しない**という測定結果が出ています。全網羅を1巡で終えるには
  `maxShownEntries >= 写真総数 N` が必要で、
  N 未満にすると**飢餓は起きないが全網羅の周期がクーポンコレクタ化**し、28ラウンド → 約160ラウンド
  （**約5.7倍**、21時間 → 約5日）に劣化する
- したがって本命は**選択肢 (b)（Tab5 では LittleFS の `IStateStore`）**。`kTab5ProvisionalLimits = 256`
  は「LittleFS が使えない場合の縮退運転」として据え置き。ただし 4096 バイトから逆算した値であり、
  もはや根拠なしの仮値ではない

**`kAlertHoldDuration` = 60000（1A が決定）**

暫定値 20000 は**取り違え**でした。実測すると `app/static/js/events.js:130` の
`EQ_EVENT_MS = 60000` が地震用で、20000 は同ファイルの `EVENT_MS`（**ADS-B 航空機イベント用**）です。
この表が「`config.js` にある」としていたのも誤りで、実際は `events.js` です。

⚠️ 60秒の根拠は上流では「防災レイヤをじっくり見せる」ですが、**地図表示は本企画の対象外**
（企画書 §1.3）なので、**値は移植できたが根拠は移植できていません**。体験を見て調整してよい。

**`ClaimPriority` の並び順（1A が決定）**

企画書 §2.3 と突合し、`Alert` と `RecordedAwaitingServer` を**入れ替え**ました。

```
Photo=0, Env=1, Degraded=2, Care=3, CareDue=4, Alert=5, RecordedAwaitingServer=6
```

⚠️ **`Alert` はもう最大値ではありません。** `priority == ClaimPriority::Alert` を「最上位」の意味で
書いたコードは誤りになります。根拠は `claim.hpp` の `static_assert` にコメントで固定済み。

`Degraded` は**動かしていません**。企画書 rank 1 は「初期設定・OTA・致命的エラー」ですが、
それに対応する状態が `DisplayStateKind` に存在しないためです。Degraded を最強にすると
「データが古いだけで地震アラートを抑止する」ことになり、Alert の根拠を反転させます。
**rank 1 相当が必要と判明したら、Degraded を流用せず新しい段を足してください。**

**`WeeklyDay` のキー（fixture 採取で確定）**

`/api/env` の `weekly` は7件の配列。キーは `date` / `code` / `pop` / `temp_min` / `temp_max`。
⚠️ **`pop` / `temp_min` / `temp_max` は `null` をとります**（実測: 翌日分がすべて `null`）。
JMA の解析に失敗した場合は `weekly` 自体が空配列になります。

**`parse.cpp` の日時系の担当**

この文書は当初 1D としていましたが、**1B が実装しました**（写真スケジューラが記念日判定と
撮影時刻順で必要とするため）。担当の重複でしたが、実装は1つです。

---

## 13. 完了の確認（変更したら毎回）

```bash
cmake -S native-display -B native-display/build -G Ninja
cmake --build native-display/build          # ★警告ゼロを維持すること
ctest --test-dir native-display/build --output-on-failure
./native-display/tools/check-esp-portability.sh
./native-display/build/ambient_display --screenshot /tmp/check.png
```

`unit_tests` のリンク行に **SDL / curl が含まれていないこと**も確認してください。

```bash
grep 'LINK_LIBRARIES' -A0 native-display/build/build.ninja | grep unit_tests
# または
otool -L native-display/build/unit_tests   # libc++ と libSystem だけのはず
```

このリンク行が層分離の継続的な検証です。SDL や curl を足したくなったら、
**リンク行を直す前に設計側を疑ってください。**
