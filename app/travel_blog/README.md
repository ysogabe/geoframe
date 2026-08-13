# GEOFRAME旅行ブログ編集機能

GeoFrameの写真カタログと旅程JSONを照合し、旅行ブログをレビューして静的HTMLへ書き出すMVPです。

## 使い方

1. GeoFrameサーバーを通常どおり起動する。
2. `http://localhost:8080/blog-admin` を開く（既存の `/admin` からも移動可能）。
3. 日別の本文、写真選択、キャプションを確認・編集する。
4. 「下書き保存」で編集状態を保存する。
5. 「HTMLを書き出す」で `GEOFRAME_DATA_DIR/blog_exports/<trip_id>/` にHTML、Markdown、manifest、EXIF除去済み画像を生成する。

開発時にこのMacだけへ公開する場合は、`GEOFRAME_HOST=127.0.0.1` を付けて起動します。家庭内LANへ公開する既存動作は既定値 `0.0.0.0` のままです。

## 旅程の追加

`plans/` に既存JSONと同じ形式で `<trip_id>.json` を追加します。`trip_id` は英小文字・数字・ハイフンのみです。写真の候補期間は各 `segments[].date`、`start_local`、`end_local` で決まります。

下書きは `GEOFRAME_DATA_DIR/travel_blog/drafts/`、出力は `GEOFRAME_DATA_DIR/blog_exports/` に生成され、Gitの追跡対象外です。元写真と `catalog.db` の写真行は変更しません。
