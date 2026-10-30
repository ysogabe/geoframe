# 撮影地の手がかり判定の検証データ

位置なし写真の撮影地候補（Rustの`crates/geoframe/src/ai/location_clue.rs`・`location_suggest`）は、
Decision（gpu-eval の d3-lite）が「撮影地の手がかりがある」と判定した写真だけを Gemini へ送る。
この関門の基準（`clue ≥ 0.2` または `kind = landmark`。Rustでは`location_clue.rs`の`CLUE_MIN`）を決めたときのデータを置く。仕組み全体は [photo-pipeline.md](../../docs/architecture/photo-pipeline.md) を参照。

| ファイル | 内容 |
|---|---|
| `labels.json` | 位置なし写真64枚と、目視で付けた「撮影地の手がかりがあるか」（`clue`） |
| `baseline-2026-10-13.json` | 基準を決めたときの Decision の判定（`clue` 0〜1、`kind`） |

## ラベルの作り方と限界

- 2026-10-13 時点の位置なし写真から、撮影月ごとに約25枚に1枚の割合で64枚を抜いた（乱数の種は7）。
- 「国や都市を推定できる手がかり（有名な建物や遺跡、読める看板の文字、その土地特有の街並みや
  建築）が写っているか」を、Decision を流す前に Claude Code が縮小画像を見て付けた。1人分のラベルで、
  手がかりありの19枚はエジプト・中国の旅行に偏っている。基準は暫定値として扱う。
- 写真は ID で指すので、本番のカタログ（`/var/lib/geoframe/catalog.db`）でしか画像を引けない。
  位置を手動で決めた後も ID は変わらないので、ラベルはそのまま使える。

## 2026-10-13 の結果

```
clue の AUC = 0.968
clue≥0.2 or landmark   選択 21  手がかりあり 18/19  適合率0.86
```

landmark と判定された9枚はすべて手がかりあり。nature の8枚に手がかりありは無かった。

## 測り直す

測り直すスクリプト（`eval_decision.py`）は、旧Python版の`app/geoframe/loc_suggest.py`の質問文・画像の縮小・合否の判定を
そのまま使っていたので、Python版の削除（P5b）と一緒に消した（旧版は P5a のマージのコミット
[`a804623`](https://github.com/ysogabe/geoframe/tree/a804623029ad67d415692d08ea3182fd765eb2a4/tools/loc_clue_eval)
に残っている）。Rustの基準を測り直すときは、`labels.json`の`clue`と`baseline-2026-10-13.json`の判定を、
Rustの`location_clue`が使う質問文・縮小・合否の判定に流して比べるスクリプトを新しく書く。
