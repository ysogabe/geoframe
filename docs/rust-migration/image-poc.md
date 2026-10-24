# 画像処理の技術PoC（P0 / T0.4）

- 状態: 計測済み（2026-10-24）
- 追跡: [GEOFRAME #54](https://github.com/ysogabe/geoframe-ambient-display/issues/54)（Rust単一バイナリ化）。受入条件のAC5（HEICなど純Rustで完結しない依存の明示と検証）とAC7（秘密・位置情報・家庭写真を載せない）を受け持つ
- 計画: [plan.md](plan.md)の「画像ライブラリ」「HEIC変換のバックエンドと配布形態」への答え。配布形態は[build.md](build.md)と合わせて読む
- コード: [`crates/geoframe-imgpoc`](../../crates/geoframe-imgpoc)（`publish = false`。本体の`crates/geoframe`には重い依存を入れていない）

この文書には数値と集計だけを書く。ファイル名・写真の内容・EXIFの値・位置情報は載せていない（計測に使った入力は連番に付け替え、EXIFには合成の値を入れた）。

## 1. 結論

| 項目 | 結論 |
| --- | --- |
| JPEGデコード | `image` 0.25.10（`default-features = false`、`jpeg`）。内部はzune-jpegで、`zune-jpeg`を直接使っても差は5%程度（§3.2） |
| 向き補正 | `image`の`orientation()`でEXIFのOrientationを読み、`apply_orientation`で当てる。HEICはデコーダがirot/imirを適用済みなので**当てない**（二重に回る）。**縮小してから向きを当てる**（結果は同じで、速く、メモリも減る） |
| 縮小 | **`fast_image_resize` 6.1の`CatmullRom`**（Pillowの`thumbnail`既定＝BICUBICと同じ系統）。`image::imageops::resize`は同じフィルタで**14倍遅い**ので使わない |
| JPEG出力 | **`jpeg-encoder` 0.7.1、4:2:0、品質85、ハフマン表の最適化は付けない**。出力サイズは現行Pythonと同じ（平均+0.03%）。`image`の`JpegEncoder`は4:4:4固定で、エンコードが2.6倍遅く、出力が11%大きい |
| ICC | **sRGBへ変換して、プロファイルは付けない**（`moxcms`）。現行Pythonは捨てるだけで、広色域の写真の色が薄くなる（§5） |
| EXIF・GPS | 出力に**残さない**。現行Pythonと同じで、どのエンコーダでも自然にそうなる（§5）。位置・撮影時刻の抽出は別に必要（P2） |
| HEIC | **第一候補は純Rustの`zune-heic` 0.5.0**。合成HEIC 50枚でlibheifとバイト単位で同じ出力になり、muslの静的バイナリに入る。実機のHEICで確かめるまでの**逃げ道は`libheif-rs`**（システムのlibheif）。純Rustの他の2つ（`heic-rs`・`heif-oxide`）は今回の入力で色が合わず外す（§3.4） |
| 配布 | **[build.md](build.md)のmusl静的ビルドの方針は変えない**。HEICを`zune-heic`にする限りCライブラリが増えない。musl標準のアロケータは約15〜25%遅いので`mimalloc`を付ける（§3.5）。libheifが必要になっても、zigでmuslの静的ビルドができた（§3.5、§4） |

Pythonとの比較（検証環境の2コア。1枚あたりの中央値）:

| 入力 | 現行Python（Pillow 13＋pillow-heif） | Rust（推奨構成） |
| --- | --- | --- |
| JPEG 約12MP | 312 ms | **220〜234 ms**（縮小→向き補正／向き補正→縮小。glibc、musl＋mimallocも同程度） |
| JPEG 約48MP | 1,120 ms | **568 ms** |
| HEIC 約12MP | 1,279 ms | **871 ms**（zune-heic、2コアを使う）／1,242 ms（libheif 1.23） |
| 最大RSS（JPEG 12MP、1枚ずつ） | 178 MiB | 約170〜200 MiB（musl＋mimallocは183、縮小→向き補正は173） |

JPEGは約25%速い程度で、**Rustにする理由は速度ではない**（PythonのPillowも縮小・エンコードはCのSIMD実装）。単一バイナリ化と、Python・pillow-heif・libheifの同梱が要らなくなることが利点になる。

## 2. 方法

### 2.1 入力

検証環境（lxc-geoframe: Ubuntu 24.04、glibc 2.39、2コア、メモリ4 GiB）にある本番コピーの`derivatives/`（長辺2560のJPEG）から、無作為に100枚を選んだ（Google Driveの原本は使わない。ユーザー指示）。**写真は検証環境の外へ出していない**。計測後に消した。

実機のカメラ原本に近づけるため、[`scripts/prepare_data.py`](../../crates/geoframe-imgpoc/scripts/prepare_data.py)で次のように作った。

| 種類 | 枚数 | 内容 |
| --- | --- | --- |
| JPEG | 50 | 長辺4032へ拡大（Lanczos）し、輝度ノイズ（σ=3）を足した。品質92、平均2.7 MB。12枚に1枚（4枚）は拡大せず2560のまま |
| HEIC | 50 | 同様の寸法・ノイズ。pillow-heif（x265）で品質85、平均6.4 MB。**単一画像で、iPhoneのような512×512のグリッド分割ではない** |
| 向き | 各50 | EXIF Orientationを1/3/6/8の順に付けた。表示すると元の向きになるよう、画素を逆変換してから付けた。HEICはpillow-heifがirot/imirとして書く |
| ICC | 各17 | 約3枚に1枚にDisplay P3相当のICCを付け、画素もP3へ変換した。システムにP3のプロファイルが無いため、原色とsRGBの転送曲線から自前で組み立てた（[`icc_profiles.py`](../../crates/geoframe-imgpoc/scripts/icc_profiles.py)） |
| EXIF | 各50 | 合成のGPS・撮影時刻・機種名（出力に残らないことの確認用） |
| 正解 | 各50 | 向き補正後、sRGBの長辺2560のPNG（ICC付きの入力は変換前の画素から作る） |

拡大＋ノイズは実写のJPEG/HEICとは中身が違う（ファイルサイズは近づけたが、デコード時間は実写と同じとは限らない）。HEICの寸法・ビットレートも実機とは異なる（§7）。

大きい入力の目安として、約48MP（8064×6048）のJPEG（11 MB）とHEIC（28 MB、ノイズ多め）を1枚ずつ作った（[`prepare_big.py`](../../crates/geoframe-imgpoc/scripts/prepare_big.py)）。

### 2.2 計測

- 現行Python: 検証環境の`/opt/geoframe-pyref`のvenv（Pillow 13.0.0、pillow-heif 1.8.1＝libheif 1.23.6＋libde265 1.1.3）で、`app/geoframe/util.py`の`make_derivative`と同じ手順（`exif_transpose`→`thumbnail((2560, 2560))`→`quality=85`で保存）を同じ入力に行った（[`run_python_ref.py`](../../crates/geoframe-imgpoc/scripts/run_python_ref.py)）。
- Rust: [`geoframe-imgpoc`](../../crates/geoframe-imgpoc)をreleaseでビルドし、同じ入力ディレクトリを処理した。時間は1枚ごとにデコード・向き補正・縮小・ICC・エンコードの段階別に測る（ファイルの読み込みは含めない）。最大RSSは`/proc/self/status`の`VmHWM`。各設定は2回走らせ、2回目（ページキャッシュが温まった状態）を記録した（[`bench.sh`](../../crates/geoframe-imgpoc/scripts/bench.sh)、[`bench_builds.sh`](../../crates/geoframe-imgpoc/scripts/bench_builds.sh)）。
- 並列: `--jobs 2`で2枚を同時に処理した。**処理量**は枚/秒（2コアの全体）。
- 画質: [`compare.py`](../../crates/geoframe-imgpoc/scripts/compare.py)で、輝度のPSNRとSSIM（一様7×7窓）を測る。基準は「現行Pythonの出力」と「正解」の2つ。数値の一致は求めず、目安にする。サンプルは設定ごとに16〜17枚（SSIMが重いため）。
- 出力のEXIF・GPS・ICCの有無は[`check_outputs.py`](../../crates/geoframe-imgpoc/scripts/check_outputs.py)で全出力を集計した。

CPUの条件: 検証環境の2コアを他の作業と競合させず、1設定ずつ順に測った。ビルド方式の比較（§3.5）は、HEICの`zune-heic`が`--heic-threads 1`でも2コア近く（CPU 166%）を使う点に注意（§3.4）。

## 3. 結果

### 3.1 全体と現行Pythonとの違い

計測した全26設定（JPEG17・HEIC9、各50枚、計1,300枚）で、Rustの出力は**現行Pythonと同じ寸法**になった（不一致は0）。向き補正（1/3/6/8）と、HEICのirotの二重適用が無いことを含む。

### 3.2 JPEG（50枚、ICCは捨てる＝Pythonと同じ条件、`--jobs 1`）

比べる部品だけを変えた。基準（太字）は「`image`でデコード・`fast_image_resize`のCatmullRom・`jpeg-encoder`の4:2:0」。画質は、ICCなしの16枚の中央値。

**縮小**

| 縮小 | 1枚 中央値 | 縮小の時間 | 最大RSS | PSNR（対Python） | PSNR（対正解） |
| --- | --- | --- | --- | --- | --- |
| **`fir`:CatmullRom** | **234 ms**（p95 334） | 34 ms | 202 MiB | 47.7 dB | 42.4 dB |
| `fir`:Lanczos3 | 255 ms | 46 ms | 198 MiB | 45.9 | 42.5 |
| `fir`:Bilinear | 233 ms | 36 ms | 190 MiB | 42.0 | 40.5 |
| `fir`:Box | 224 ms | 26 ms | 198 MiB | 41.6 | 40.3 |
| `image`:CatmullRom | 696 ms | 466 ms | 329 MiB | 48.0 | 42.4 |
| `image`:Lanczos3 | 787 ms | 581 ms | 327 MiB | 46.0 | 42.6 |
| `image`:Triangle | 625 ms | 412 ms | 284 MiB | 42.0 | 40.5 |
| 現行Python（Pillow） | 312 ms（p95 371） | - | 178 MiB | - | 42.6 |

- Pillowの`thumbnail`の既定はBICUBIC（`a = -0.5`＝CatmullRom）。CatmullRomが現行Pythonの出力に最も近く（PSNR 47.7 dB、最小42.0 dB）、正解との差も−0.2 dBだけ。
- BilinearとBoxは速さの利点がほとんどなく、画質が約2 dB落ちる。

**デコード・向き・エンコード**

| 項目 | 設定 | 1枚 中央値 | 段階の時間 | 出力の平均 | 最大RSS | PSNR（対正解） |
| --- | --- | --- | --- | --- | --- | --- |
| デコーダ | `image`（基準） | 234 ms | decode 95 ms | 764,042 B | 202 MiB | 42.4 |
| デコーダ | `zune-jpeg`直接 | 224 ms | decode 90 ms | 764,042 B | 189 MiB | 42.4 |
| エンコーダ | **`jpeg-encoder` 4:2:0（基準）** | 234 ms | encode 60 ms | **764,042 B** | 202 MiB | 42.4 |
| エンコーダ | `jpeg-encoder` 4:2:0＋ハフマン最適化 | 257 ms | encode 77 ms | 746,981 B | 202 MiB | 42.4（Pillowでは） |
| エンコーダ | `jpeg-encoder` 4:4:4 | 277 ms | encode 96 ms | 869,980 B | 157 MiB | 42.1 |
| エンコーダ | `image`の`JpegEncoder`（4:4:4） | 335 ms | encode 157 ms | 845,599 B | 174 MiB | 42.1 |
| 現行Python | Pillow（4:2:0、品質85） | 312 ms | - | 763,834 B | 178 MiB | 42.6 |
| 順序 | 向き補正→縮小（基準） | 234 ms | orient 25 ms | - | 202 MiB | 42.4 |
| 順序 | **縮小→向き補正** | **220 ms**（p95 261） | orient 10 ms | 764,019 B | **173 MiB** | 42.4 |
| 向きの読み取り | `image`（基準） / `kamadak-exif` | 234 / 241 ms | - | 同じ | 202 / 190 MiB | 同じ |

- **重要: `jpeg-encoder`の「4:2:0＋ハフマン最適化」の出力を、zune-jpeg 0.5.15（`image`のデコーダ）が正しく読めない。** Pillowは正しく読む。実際の出力10枚を`image`で読み直すと、Pillowの読み取り結果とのPSNRが4〜8 dBだった（正しい出力は52〜59 dB）。miharashiは`image`でderivativeを読むので、**ハフマン最適化は付けない**。4:4:4では起きない。上流への報告はしていない。
- 4:2:0・ハフマン最適化なし・品質85の出力サイズは、現行Pythonと同じ（平均+0.03%）。`image`の`JpegEncoder`は4:4:4固定で、同じ品質値でも14%大きい。
- 出力の読み戻し: 推奨構成の出力は、`image`・`zune-jpeg`・Pillowのどれで読んでも同じ画素になる（再エンコードを挟んだ比較で55 dB）。

**ICC**（ICC付きの17枚）

| 方針 | 1枚 中央値 | 追加の時間 | 出力の平均 | PSNR（対正解）の中央値／最小 |
| --- | --- | --- | --- | --- |
| 捨てる（現行Python） | 234 ms | - | 764,042 B | 38.7 dB／28.6 dB |
| そのまま埋め込む | 256 ms | - | 764,906 B（+864 B） | 画素は同じ。色管理するビューアでだけ正しい |
| **sRGBへ変換** | 244 ms | 15 ms（p95 20 ms） | 766,562 B | **41.3 dB／37.2 dB** |

**並列（`--jobs 2`）と入力サイズ**

| 構成 | 処理量 | 1枚 中央値 | 最大RSS |
| --- | --- | --- | --- |
| Python、`--jobs 2` | 5.48枚/秒 | 371 ms | 167 MiB |
| Rust（`image`）、`--jobs 2` | 7.20枚/秒 | 271 ms | 309 MiB |
| Rust（`zune-jpeg`直接）、`--jobs 2` | 7.16枚/秒 | 282 ms | 278 MiB |
| 約48MP、Python | - | 1,120 ms | 475 MiB |
| 約48MP、Rust（縮小→向き補正でも同程度） | - | 568 ms | 215 MiB |

ほかのJPEGの種類（プログレッシブ、4:4:4、4:2:2、ハフマン最適化、グレースケール、CMYK、途中で切れたもの）の合成ファイルは、`image`でも`zune-jpeg`でもデコードできた。**途中で切れたJPEGも、エラーにならず部分的な画像になる**（Pillowは既定でエラー）。CMYKの色の正しさは確かめていない（§7）。

### 3.3 HEICのデコーダ以外（出力側）

HEICの入力でも、縮小・エンコード・ICCは§3.2と同じコードを通る。ICC付きの17枚は、捨てるとPSNR（対正解）が中央値38.4 dB／最小32.9 dB、sRGBへ変換すると41.4 dB／38.8 dB。変換の時間は14 ms。

### 3.4 HEICのデコード（50枚、ICCは捨てる、`--jobs 1`）

| 候補 | 種類 | 1枚 中央値（p95） | 処理量 `--jobs 2` | 最大RSS（jobs 1／2） | 出力 |
| --- | --- | --- | --- | --- | --- |
| 現行Python（pillow-heif、libheif 1.23.6） | C（wheelに同梱） | 1,279 ms（1,469） | 1.40枚/秒 | 193／196 MiB | - |
| `libheif-rs`＋システムのlibheif 1.17.6 | C、動的リンク | 1,401 ms（1,708） | 1.26枚/秒 | 234／314 MiB | 100%同じ寸法 |
| `libheif-rs`＋同梱libheif 1.23.0（zigでmusl静的） | C＋C++、静的 | 1,242 ms（1,453） | **1.48枚/秒** | 141／216 MiB | 同上 |
| **`zune-heic` 0.5.0** | 純Rust | **871 ms**（1,053） | 1.20枚/秒 | 226／338 MiB | **libheifと50/50バイト一致** |
| `heic-rs` 0.1.4 | 純Rust | 1,306 ms | 1.48枚/秒 | 226／364 MiB | **色が合わない**（正解に対して28 dB） |
| `heif-oxide` 0.1.0 | 純Rust | 1,510 ms | 計測せず | 317 MiB | **色が合わない**（同28 dB） |
| `heif-convert`（外部コマンド） | C、サブプロセス | 1,723 ms（2,042） | 1.04枚/秒 | 154／241 MiB（親のみ） | JPEG再圧縮が1回増え、−0.3 dB |

- **処理量は並列2枚で1.2〜1.5枚/秒に収まり、候補間の差は小さい。** 実機ではderivative生成を1〜2枚ずつ裏で進める用途なので、どれでも足りる（本番の約6,500枚を一括で作り直すと、1.3枚/秒でも約1.4時間）。
- `zune-heic`は`set_num_threads(1)`でも、1枚の処理が**CPU 166%**（2コア近く）になった。単一画像の中でも並列化する（デブロッキングなど）ためで、1枚あたりの遅延は短い（871 ms）が、2枚同時の処理量はlibheifと変わらない（1.20 vs 1.26枚/秒）。50枚のCPU時間（user＋sys）は`zune-heic`が約69秒、libheif 1.17.6が約68秒で、ほぼ同じ。
- libheif 1.23.0はCPU時間（約60秒）・RSS（141 MiB）が最も小さい。新しい版の効果で、`zune-heic`より処理量で上回る。
- `zune-heic`の出力は、libheifと**全50枚でバイト単位で同じ**（デコード結果が同一で、以降の処理が同じ）。作者は「libheif 1.17とビット単位で同じ」と書いており、それと整合する。ICC付き17枚でもICCの取り出しが一致した。
- `heic-rs`と`heif-oxide`は、同じ入力で正解に対して約28 dB（libheifは38 dB）。色変換（行列・範囲の既定）の違いが疑われるが原因は調べていない。実機のHEICで再評価する価値はあるが、今回は候補から外す。なお`heic`クレートはAGPL-3.0なので最初から外した。
- 約48MPのHEIC（28 MB、単一画像）: libheif 1.17.6は5.16秒・441 MiB、`zune-heic`は約3.0〜3.1秒・278 MiB、`heic-rs`は4.74秒・363 MiB。**現行Python（pillow-heif）は「Decoding the input data did not give a decompressed image」で失敗した**（libheif 1.23.6＋libde265 1.1.3。原因は調べていない。実機の48MP HEICはグリッド分割なのでこの入力とは構造が違う）。`zune-heic`は既定のmdat上限（16 MiB）を超えるファイルを拒否するので、`set_hevc_max_mdat_size`で明示する必要がある（PoCは128 MiB）。
- 純RustのHEICデコーダは、2026年10月時点で少なくとも5つある。`zune-heic`（MIT OR Apache-2.0 OR Zlib）はzune-jpegと同じ作者で、`image`と同じ系統。

### 3.5 ビルド方式（glibc／musl静的／同梱libheif）

検証環境のglibc 2.39で、同じ入力を処理した。JPEGは`--jobs 1`、HEICは`zune-heic`。

| ビルド | 要る共有ライブラリ（`ldd`） | JPEG 中央値（jobs 1／処理量 jobs 2） | JPEG 最大RSS | HEIC zune 中央値（jobs 1／処理量 jobs 2） |
| --- | --- | --- | --- | --- |
| glibc（検証環境でビルド） | libc・libm・libgcc_s | 235 ms／7.04枚/秒 | 202 MiB | 871 ms／1.20枚/秒 |
| glibc（**開発機のDebian 13、glibc 2.41でビルド**） | 同上 | 253 ms／6.82枚/秒 | 200 MiB | 894 ms／1.22枚/秒 |
| musl静的 | なし（static） | 273 ms／6.17枚/秒 | **112 MiB** | 1,086 ms／1.05枚/秒 |
| musl静的＋`mimalloc` | なし（static） | **222 ms／7.73枚/秒** | 183 MiB | 939 ms／1.11枚/秒 |

- **開発機（glibc 2.41）でビルドしたglibcバイナリは、検証環境のglibc 2.39でそのまま動いた。** 要求するglibcは、必須が`GLIBC_2.34`まで。`objdump -T`は`GLIBC_2.39`（`pidfd_spawnp`・`pidfd_getpid`。Rust 1.99の`std::process::Command`が参照する）も出すが、`readelf -V`では**弱参照（`WEAK`）**で、古いglibc（2.34〜2.38）でも起動を妨げない見込み（そのglibcでは試していない）。要求バージョンは`objdump -T`の最大値ではなく、`readelf -V`の`WEAK`の有無も見て判断する。
- musl標準のアロケータは**約15〜25%遅い**（JPEG 273 vs 235 ms、`zune-heic` 1,086 vs 871 ms）がRSSは小さい。`mimalloc`を付けるとglibcと同等以上（JPEG 222 ms）になる代わりに、RSSはglibc並み（`--jobs 2`で352 MiB）。[build.md](build.md)の「画像処理を入れた段階で計測し、問題なら`mimalloc`」の予告どおり。
- 開発機のglibcビルドと検証環境のビルドの差は誤差の範囲。

## 4. HEICの依存とライセンス

### 4.1 システムのlibheifを使う場合（`libheif-rs`、feature `heic`）

| 項目 | 内容 |
| --- | --- |
| Ubuntu 24.04のパッケージ | `libheif1` 1.17.6。**HEVCのデコーダはプラグインで、`libheif-plugin-libde265`が別に要る**。このパッケージが無いとHEICは読めない（プラグインのパスを空にして再現: `Unsupported codec`）。今回の検証環境も、HEVCのプラグインは入っていなかった（AV1のaomだけ） |
| ビルド時 | `libheif-dev`、`pkg-config`。`--no-install-recommends`だと`zlib1g-dev`も要る（`.pc`がzlibを要求する） |
| `libheif-rs` 3.0.0 | 既定の`latest`はlibheif 1.23のAPIを要求する。**`default-features = false, features = ["v1_17"]`にする**（Ubuntu 24.04のlibheif 1.17.6に合わせる）。1.17ではセキュリティ上限（`SecurityLimits`、1.19以降）のAPIが使えない。入力の寸法はデコードの前に`handle.width()`/`height()`で自分で制限する |
| 実行時 | PoCのバイナリは`libheif.so.1`・`libgcc_s`・`libm`・`libc`に動的リンクし、libheifが`libsharpyuv`・`libz`・`libstdc++`を引く。HEVCのプラグイン（`libheif-libde265.so`→`libde265.so.0`）は実行時に`dlopen`される。`ldd`には出ない |

### 4.2 libheifを自前で組み込む場合

| 方法 | 結果 |
| --- | --- |
| `libheif-sys`の`embedded-libheif`（glibc） | **libheif本体は静的になるが、自己完結しない。** cmakeが見つけたシステムの`libde265`・`libx265`・`libaom`・`libdav1d`・`libsharpyuv`・`libnuma`・`libstdc++`に動的リンクした（`ldd`で10個）。`libheif-sys`のbuild.rsがデコーダ・エンコーダを全て有効にして探すため、`libx265`（GPL）まで引き込んだ。libde265を静的ライブラリで用意し、`pkg-config`の探索先を絞っても、libheifが`x265`・`aom`・`dav1d`を要求したままになり**ビルドが通らなかった**（build.rsを直さずには避けられない） |
| **zig cc＋`embedded-libheif`＋静的libde265（musl）** | **通った。** `ziglang`（pip）の`zig cc`/`zig c++`をC/C++コンパイラにし、libde265 1.0.15をzigで静的ビルドして、`libheif-sys`が同梱するlibheif 1.23.0とリンクした。**完全に静的**（`ldd`は動的実行ファイルではないと答える）で、7.4 MB。検証環境で動いた。手順: ①コンパイラのラッパーで`--target=`を落とす（cc crateが付けるzigが解釈できない引数）、②`RUSTFLAGS=-C link-self-contained=no`（Rustのcrtとzigのcrtが重複する）、③`PKG_CONFIG_ALLOW_CROSS=1`と、静的libde265だけを置いた`PKG_CONFIG_LIBDIR`。zigの探索範囲にシステムのx265などが無いので、GPLのエンコーダも入らない |
| `cargo zigbuild`でglibcの下限を固定 | 今回は試していない（[build.md](build.md)の§6の案。必要になったら使う） |

### 4.3 ライセンス

| 部品 | ライセンス | 注意 |
| --- | --- | --- |
| libheif | LGPL-3.0-or-later（サンプルアプリはMIT） | **静的リンクで配布するなら、利用者が再リンクできるように、オブジェクトファイルまたはソースを提供する義務がある**。動的リンクならこの義務は軽い。**このリポジトリはバイナリを配布していない**ので今は問題にならないが、GitHubリリースなどでバイナリを配るなら、`libheif`・`libde265`のLGPLの扱いを決めてから |
| libde265 | LGPL-3.0-or-later | 同上 |
| x265（HEVCエンコーダ） | GPL-2.0-or-later | **デコードには要らない。** 同梱libheifのglibcビルドが引き込んだ（§4.2）。リンクしないこと |
| libaom・dav1d | BSD-2-Clause | AVIFのデコーダ。HEICには要らない |
| `libheif-rs`・`libheif-sys` | MIT | ラッパー。`libheif-sys`は同梱するlibheifのLGPLを引き継ぐ |
| `zune-heic`・`zune-jpeg` | MIT OR Apache-2.0 OR Zlib | 純Rust。配布時の追加の義務なし |
| `image`・`fast_image_resize` | MIT OR Apache-2.0 | |
| `jpeg-encoder` | (MIT OR Apache-2.0) AND IJG | IJGは表示義務（READMEなどへの記載） |
| `moxcms` | BSD-3-Clause OR Apache-2.0 | |
| `kamadak-exif` | BSD-2-Clause | |
| `heic`（不採用） | AGPL-3.0-only | ネットワーク経由で使わせるサーバーでは、ソース公開の義務が及ぶ。使わない |

HEVC（H.265）には特許プールがある。個人の家庭サーバーで読むだけなら通常は問題にしないが、**バイナリを配布する場合は確認する**（法的な助言ではない）。

## 5. ICC・EXIF・GPS の扱い

### 5.1 現行Pythonの挙動（合成画像で確認）

`make_derivative`の呼び出しを、合成のJPEG・HEIC（Orientation 1/3/6/8、GPS・機種名のEXIF、ICC付き）に行った。

| 項目 | 現行Python |
| --- | --- |
| EXIF・GPS | **出力に残らない**（`save`にexifを渡さないため）。EXIFのタグ数は0 |
| ICC | **捨てる**。画素の変換もしない。広色域（Display P3など）の写真は、sRGBとして表示され、彩度が下がる |
| 向き | `exif_transpose`で当てる。HEICはpillow-heifが読み込み時にOrientationを1へ戻し、libheifがirot/imirを適用する |
| 縮小 | `thumbnail`（BICUBIC、`reducing_gap=2.0`）。長辺が2560以下なら何もしない |
| サブサンプリング | 4:2:0（`subsampling=2`）、JFIFあり |

### 5.2 Rustの推奨構成

- **EXIF・GPS**: どのエンコーダも、渡さない限りEXIFを書かない。全出力（28ディレクトリ、現行Pythonの100枚を含む計1,400枚）を集計して、EXIF・GPSが残っているものは0だった（ICCが残っていたのは、`keep`を指定した2設定の34枚だけ）。単体テストも、全ての部品の組み合わせで「出力にEXIFのヘッダも機種名の文字列も残らない」ことを確かめている。**ログ・エラー文にも画像の内容を出さない**（PoCは寸法と件数だけ）。
- **ICC**: 推奨は`srgb`（変換して付けない）。理由: ①miharashi・`image`のデコーダはICCを使わず、`keep`では色管理されない表示側で色が薄くなる、②現行Pythonが捨てているのと違い、変換すれば正解に近づく（PSNR 38.7→41.3 dB、最悪28.6→37.2 dB）、③費用は15 ms/枚。広色域の色は、sRGBの外側がクリップされる。ICCが読めない・壊れているときは、捨てる側（`drop`）へ落とし、ログに件数だけ残す。**これは現行Pythonからの意図した変更**（広色域の写真の見え方が変わる）。
- **向き**: JPEGは`Decoder`のOrientationを当てる。HEICは当てない（libheifと`zune-heic`のどちらもirot/imir/clapを適用する）。PoCのテストはHEICのirot（向き6）が1回だけ当たることを確かめている。
- **位置・撮影時刻の抽出**はderivativeの生成とは別で、P2で行う。`kamadak-exif`（JPEGのAPP1・TIFF）が使える。HEICのEXIFは`zune-heic`の`exif_data()`、または`libheif-rs`のメタデータブロックから取り出す。

## 6. 推奨構成（P2で使うcrateとfeature、配布形態）

```toml
# crates/geoframe の画像処理（P2）。バージョンはPoCで測ったもの
image = { version = "0.25.10", default-features = false, features = ["jpeg"] }
fast_image_resize = "6.1.0"                  # 既定のfeature（rayonなし）。並列は自分で制御する
jpeg-encoder = "0.7.1"                       # ハフマン最適化は使わない（zune-jpegが誤読する）
kamadak-exif = "0.6.1"                       # 位置・撮影時刻の抽出
moxcms = "0.8.1"                             # imageが依存する版に揃える
zune-heic = "0.5.0"                          # HEIC（純Rust）
zune-core = "0.5"                            # DecoderOptions
mimalloc = "0.1"                             # musl向けのグローバルアロケータ

[features]
# 実機のHEICで問題が出たときの逃げ道。既定では外す（CIとmuslビルドにlibheifを要求しない）
heic-libheif = ["dep:libheif-rs"]
# libheif-rs = { version = "3.0.0", default-features = false, features = ["v1_17"], optional = true }
```

- **処理**: 縮小→向き補正→（ICCがあれば）sRGBへ変換→JPEG（品質85、4:2:0）。縮小の寸法は[`target_dims`](../../crates/geoframe-imgpoc/src/lib.rs)（Pillowの`thumbnail`と同じ丸め）。
- **並列**: 1枚の処理は単一スレッドで、CPU時間の大半はデコードとエンコード。2コア・4 GiBでは**同時に1〜2枚**に絞る（`--jobs 2`で最大RSSが約300〜390 MiB。約48MPで1枚あたり約220〜280 MiB）。tokioのworkerを塞がないよう`spawn_blocking`か専用スレッドで行う。
- **入力の制限**: デコードの前に寸法で拒否する（PoCは2億画素。Pillowの`MAX_IMAGE_PIXELS`は約8,900万画素で警告、その2倍でエラー）。`zune-heic`の`set_hevc_max_mdat_size`も明示する。途中で切れたJPEGは`image`・`zune-jpeg`だと部分的な画像になるので、厳格に扱うかをP2で決める。
- **HEICの方針**: 既定は`zune-heic`。P2の実機のHEICで、`zune-heic`と`libheif-rs`の出力が同じか（今回の合成入力では50/50が同じ）を確かめる。問題が出た形式だけlibheifに回すか、全面的にlibheifへ切り替える。
- **配布**: **[build.md](build.md)のmusl静的ビルドを標準のまま変えない。** `zune-heic`なら、HEICを含めても純RustでC依存が増えない（muslの静的バイナリ7.0 MBで`ldd`は動的実行ファイルではない）。`libheif-rs`に切り替える場合は、次のどちらか。
  1. システムのlibheifへ動的リンクするglibcビルド（検証環境のglibc 2.39に合わせる。`libheif-plugin-libde265`をインストールしておく）。
  2. zig ccで同梱libheifとlibde265を静的にリンクしたmusl静的ビルド（§4.2。LGPLの再リンクの扱いに注意）。
- **musl標準のアロケータは遅い**（§3.5）。画像処理を入れるP2から`mimalloc`を付ける。

## 7. P2に残す確認事項

**実機のHEIC固有の構造は、今回の合成HEICでは確かめられていない。** 今回のHEICは、pillow-heif（x265）が書いた単一画像で、iPhoneなどの実機のファイルとは構造が違う。P2のDrive取り込みで、実機のHEICについて次を確かめる。

- **グリッド分割**: iPhoneのHEICは512×512のタイルのグリッド。`zune-heic`・libheifのタイル結合・クロップ（`clap`）・並列化（`zune-heic`はタイルを並列に復号する）の結果が一致するか。
- **HDR・ゲインマップ**: ゲインマップ付きのHEIC（iPhone 12以降）、10ビット（Main 10）、PQ・HLGの転送特性。8ビットのSDR（SDRベース画像）へ落とす結果が妥当か。libheifも`zune-heic`もゲインマップは使わない。
- **深度・補助画像**: ポートレートの深度、アルファ、マット。主画像だけを取り出せているか。
- **Live Photo・連写・シーケンス**: Live Photoは静止画（HEIC）と動画（MOV）が別ファイル。Driveの`image/heic`の扱い、複数の最上位画像を持つファイル（`msf1`・`hevc`のブランド）、サムネイル画像が主画像として選ばれないか。
- **向き**: 実機のHEICは、irot/imirと同時にEXIFにもOrientationを持つ。libheifとzune-heicがEXIF側を無視してirot/imirだけを当てること（今回は合成入力で確認）を、実機で再確認する。
- **色の記述**: 実機はICC（Display P3）またはnclx（`colr`）。**今回のICCは`prof`形式のみ。nclxだけのファイル（Androidなど）は、`libheif-rs`の`color_profile_nclx`・`zune-heic`の扱いがどうなるかを確かめる**。nclxだけならICCを持たず、sRGB以外の原色の画像を誤って扱う恐れがある。
- **`heic-rs`・`heif-oxide`の色の不一致**の原因（行列・範囲）が、実機のファイルでも出るか。
- **EXIFの抽出**: HEICの`Exif`ブロックから位置・撮影時刻を取り出す方法（`zune-heic`の`exif_data()`は先頭のオフセットを含むかなど）。
- **CMYK・グレースケールのJPEG**: 色の正しさ（Adobeの反転CMYKなど）。デコードできることしか確かめていない。
- **`zune-jpeg`の誤読**: `jpeg-encoder`の「4:2:0＋ハフマン最適化」の出力は誤読される。上流（`jpeg-encoder`または`zune-jpeg`）への報告と、`image`の更新で直るかの確認。
- **本番に近い負荷**: 本番の約6,500枚の一括再生成の時間とメモリ、実機のJPEG原本（拡大＋ノイズではないもの）でのデコード時間。
- **`jobs`の既定値**と、`zune-heic`がHEIC1枚で2コアを使うことを考えた、同時処理枚数の決め方。

## 8. 再現手順

検証環境で、本番のコピーは読むだけにし、作業は`/var/tmp/`の下（mode 0700）で行った。計測が終わったら、入力・出力・一時バイナリ・使い捨てのvenvを全て消した。

1. `prepare_data.py`で入力100枚と正解を作る。`prepare_big.py`で48MPを1枚ずつ。`gen_fixtures.py`で単体テスト用の合成HEIC（750〜3,267バイト。`tests/fixtures/`に置いてある）。
2. `run_python_ref.py`で現行Pythonのderivativeと時間を測る。
3. `cargo build --release -p geoframe-imgpoc --features heic,heic-zune,heic-rs,heic-oxide`（検証環境のglibc。`libheif-dev`・`zlib1g-dev`・`libheif-plugin-libde265`が要る）。musl静的は開発機で`--target x86_64-unknown-linux-musl --features heic-zune`（`mimalloc`を付けるなら`,mimalloc`）。
4. `bench.sh`・`bench_builds.sh`で時間とRSS、`quality.sh`（`compare.py`）で画質、`check_outputs.py`でメタデータを集計する。

テスト: `cargo test -p geoframe-imgpoc`（合成画像だけ。向き補正・縮小の規則・EXIFを残さないこと・ICCの扱い・入力の拒否）。HEIC（`--features heic`）は、合成HEICで、向き6が1回だけ当たることとEXIFが残らないことを確かめる。CIの`imgpoc-heic`ジョブがlibheif付きで走らせる。

### 検証環境に残した変更

計測のため、検証環境に次を入れた（`/var/lib/geoframe`・`geoframe-pyref.service`・`/var/lib/geoframe-pyref`は変えていない。pillow-heifは自前のlibheifを同梱するので、pyrefへの影響もない）。

- rustup（minimalプロファイル＋clippy）。
- apt: `libheif-dev`・`libheif-plugin-libde265`・`libheif-examples`・`libde265-dev`・`libnuma-dev`・`zlib1g-dev`・`pkg-config`・`g++`・`cmake`・`python3-venv`。**`libheif-plugin-libde265`が入ったため、検証環境はUbuntu 24.04の標準の状態（HEVCを読めない）ではなくなった。** 標準の状態の再現は、プラグインのパスを空にして確かめた（§4.1）。
