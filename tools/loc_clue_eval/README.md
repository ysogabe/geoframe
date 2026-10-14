# 撮影地の手がかり判定の検証データ

位置なし写真の撮影地候補（[app/geoframe/loc_suggest.py](../../app/geoframe/loc_suggest.py)）は、
Decision（gpu-eval の d3-lite）が「撮影地の手がかりがある」と判定した写真だけを Gemini へ送る。
この関門の基準（`clue ≥ 0.2` または `kind = landmark`）を決めたときのデータと、測り直すための
スクリプトを置く。仕組み全体は [photo-pipeline.md](../../docs/architecture/photo-pipeline.md) を参照。

| ファイル | 内容 |
|---|---|
| `labels.json` | 位置なし写真64枚と、目視で付けた「撮影地の手がかりがあるか」（`clue`） |
| `baseline-2026-10-13.json` | 基準を決めたときの Decision の判定（`clue` 0〜1、`kind`） |
| `eval_decision.py` | ラベルと突き合わせ、現在の基準・しきい値ごと・kind ごとの結果を出す |

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

本番のデータを読む2つは、本番機の `/opt/geoframe` で、サーバーと同じ venv（`app/.venv`）の
Python で実行する（`geoframe` ユーザーは開発用の worktree を読めない）。

```bash
# 保存済みの判定で（DBもDecisionも使わない）
python3 tools/loc_clue_eval/eval_decision.py --scores tools/loc_clue_eval/baseline-2026-10-13.json

# ai_worker が photo_loc_clues に保存した判定で（本番のデータを読むので geoframe ユーザーで）
sudo -u geoframe env GEOFRAME_DATA_DIR=/var/lib/geoframe app/.venv/bin/python tools/loc_clue_eval/eval_decision.py --from-db

# Decision に64枚を問い合わせて（質問文・画像の縮小は loc_suggest.py のものを使う）
sudo -u geoframe env GEOFRAME_DATA_DIR=/var/lib/geoframe \
    app/.venv/bin/python tools/loc_clue_eval/eval_decision.py --save /tmp/clue-$(date +%F).json
```

DB は読み取り専用で開く。`loc_suggest.py` の質問文や基準を変えたら Decision に問い合わせる形で流し、
取りこぼし（手がかりありなのに送らない写真）と適合率（送った写真のうち手がかりありの割合）を
このファイルの結果と比べる。`--save` で残した判定は、次回 `--scores` で比べられる。
`--from-db` では、位置が決まって判定対象から外れた写真は数に入らない。
