# Home Assistant連携

GEOFRAMEは、Home Assistantの在宅情報を使った画面制御と、室内環境センサーの簡易表示に
対応しています。この連携は任意です。

## 設定

Home Assistantで長期アクセストークンを発行し、`app/.env`またはサービスの環境変数へ
次の値を設定します。

```dotenv
HOME_ASSISTANT_API_KEY=<長期アクセストークン>
GEOFRAME_HA_URL=https://home-assistant.example.test
GEOFRAME_HA_MOTION_ENTITY=binary_sensor.display_motion
GEOFRAME_HA_ENV_ENTITIES=sensor.room_temperature,sensor.room_humidity,sensor.room_co2
```

`HOME_ASSISTANT_API_KEY`の代わりに`GEOFRAME_HA_TOKEN`も使用できます。

## 対応する動作

- `GEOFRAME_HA_MOTION_ENTITY`には、在室中に状態が`on`または`home`になるエンティティを
  指定します。
- `GEOFRAME_HA_ENV_ENTITIES`には、カンマ区切りで複数のエンティティを指定します。
  GEOFRAMEは値、単位、`device_class`、`friendly_name`を使って室内環境表示を作ります。
- 温度、湿度、二酸化炭素、気圧、揮発性有機化合物には標準の表示ラベルがあります。
- `GEOFRAME_SLEEP_CMD`と`GEOFRAME_WAKE_CMD`を使うと、在室状態とOS固有の画面制御を
  接続できます。コマンドを手動確認してから設定してください。

## 画面ON/OFFの手動オーバーライド（任意）

Home Assistantへ`input_select`（選択肢は`auto` / `force_on` / `force_off`）を作り、
`GEOFRAME_HA_SCREEN_MODE_ENTITY`へそのエンティティIDを設定すると、外出先からもHA経由で
画面の電源を固定できます。GEOFRAMEはこれを既存のポーリングで読むだけで、書き込み用の
HTTPエンドポイントは持ちません（無認証の口を増やさないため）。

```dotenv
GEOFRAME_HA_SCREEN_MODE_ENTITY=input_select.geoframe_screen_mode
GEOFRAME_HA_SCREEN_DECAY_MIN=5
GEOFRAME_SCREEN_STATUS_CMD=/opt/geoframe/deploy/kiosk/screen-ctl.sh status
GEOFRAME_HA_SCREEN_RECONCILE_SEC=90
```

- 未設定のときは従来どおり人感センサー任せの自動制御だけが動きます。
- `force_on` / `force_off`は明示操作（`auto`へ戻す）でのみ解除されます。人感センサーの反応では
  解除されません。
- HAへ疎通できない状態が`GEOFRAME_HA_SCREEN_DECAY_MIN`（分）続いた場合は、オーバーライドを
  捨てて`auto`へ戻します。
- `GEOFRAME_SCREEN_STATUS_CMD`を設定すると、意図した状態と実機の画面状態を定期的に照合します。
  このコマンドは正常な`off`でも終了コード1を返すため、GEOFRAMEは標準出力の`on`/`off`だけを見ます。
- 判断の根拠は[ADR 0002](../decisions/0002-screen-power-override-via-ha.md)と
  [ADR 0003](../decisions/0003-screen-control-state-machine.md)にあります。

## 通信の安全性

アクセストークンにはHome Assistantを操作できる強い権限があるため、Gitへコミットせず、
ブラウザにも返さないでください。接続にはHTTPSを推奨します。信頼できるLAN内で平文HTTPを
使う場合は、名前解決の偽装によるトークン漏えいを避けるため、`GEOFRAME_HA_URL`へ固定IPを
指定します。GEOFRAMEは平文HTTPとホスト名の組み合わせを拒否します。

本番環境で`GEOFRAME_DEBUG_API=1`を設定しないでください。Home Assistantのエンティティ一覧を
含む、ローカル開発専用の診断APIが有効になります。

## エンティティIDの確認

Home Assistantの開発者ツールで状態を確認します。実値を含む全エンティティ一覧をリポジトリへ
保存せず、用途が明確で安定したエンティティだけを設定してください。
