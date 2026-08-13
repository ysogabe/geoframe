// GeoFrame Ambient Display — エントリポイント。
//
// この段階での到達点:
//   1. ビルドが通る
//   2. 1280x720 の論理解像度で窓が出る（拡縮してもレターボックスで比率維持）
//   3. 日本語1行と FPS が出る
//   4. fixture 画像が cover で全画面表示され、その上にテキストが重なる
//   5. ★ネットワーク非依存で PHOTO / CARE / CARE_DUE / ALERT / ENV を描画できる
//      （--screen / --screen all --screenshot <dir>）
//
// 後続フェーズはここに状態機械（PHOTO / CARE / CARE_DUE ...）を足していくが、
// 状態機械そのものは ambient_core に置くこと。このファイルは
// 「SDL のイベントループを core の状態機械へ繋ぐ配線」に留めるのが望ましい。
//
// ★現時点では core の decide() がまだ動いていないので、表示状態は
//   --screen / 数字キーで **人間が** 選ぶ。後続フェーズでここが
//   「core が決めた DisplayStateKind を受け取る」に置き換わる。
//   描画側（ambient/render/screens.hpp）はその日が来ても変わらない形にしてある。

#include <SDL.h>
#include <SDL_image.h>
#include <unistd.h>  // getpid（一時ファイル名の衝突回避のみに使う）

#include <algorithm>
#include <array>
#include <atomic>
#include <chrono>
#include <condition_variable>
#include <cstdio>
#include <cstdlib>
#include <ctime>
#include <exception>  // ポーラースレッドの例外境界（std::exception）
#include <filesystem>
#include <fstream>
#include <iterator>
#include <memory>
#include <mutex>
#include <optional>
#include <sstream>
#include <string>
#include <thread>
#include <utility>
#include <vector>

#include "ambient/core/clock.hpp"
#include "ambient/core/constants.hpp"
#include "ambient/core/env/environment_model.hpp"
#include "ambient/core/model/env.hpp"
#include "ambient/core/model/parse.hpp"
#include "ambient/core/model/photo.hpp"
#include "ambient/core/model/presence.hpp"
#include "ambient/core/model/roomenv.hpp"
#include "ambient/core/rng.hpp"
#include "ambient/core/state/alert_engine.hpp"
#include "ambient/core/state/arbiter.hpp"
#include "ambient/core/state/claim.hpp"
#include "ambient/core/state/display_state.hpp"
#include "ambient/core/state/state_machine.hpp"
#include "ambient/core/transition.hpp"
#include "ambient/core/version.hpp"
#include "ambient/render/display.hpp"
#include "ambient/render/image.hpp"
#include "ambient/render/image_worker.hpp"
#include "ambient/render/screens.hpp"
#include "ambient/render/sdl_runtime.hpp"
#include "ambient/render/text.hpp"
#include "ambient/render/transitions.hpp"
#include "ambient/transport/curl_http_client.hpp"
#include "ambient/transport/fixture_http_client.hpp"
#include "ambient/transport/geoframe_client.hpp"
#include "ambient/transport/version.hpp"

namespace {

namespace fs = std::filesystem;

using ambient::core::DisplayStateKind;

constexpr int kLogicalWidth = 1280;
constexpr int kLogicalHeight = 720;

// FPS を計測する窓の長さ（秒）。瞬間値は暴れるので一定時間の平均を取る。
constexpr double kFpsWindowSeconds = 0.5;

// --screenshot 指定時、FPS が実測値になるまで空回しする最低フレーム数。
// これに加えて FPS の計測窓が1回閉じるまで待つ。
constexpr int kScreenshotWarmupFrames = 5;

// --screen all の1画面あたりの描画フレーム数。
// macOS ではウィンドウがマップされるまで数フレームかかり、早すぎる読み出しは
// 黒画像になることがある。イベントも毎フレーム汲むこと。
constexpr int kBatchWarmupFrames = 8;

// --screen が受け付ける名前と、対応する表示状態・保存ファイル名。
// ★ここは「名前 → DisplayStateKind」の対応表であって、表示すべき状態を
//   選ぶ判断ではない（それは後続フェーズで core の decide() が行う）。
struct ScreenChoice {
  const char* name;
  DisplayStateKind kind;
  const char* file_stem;
};

constexpr std::array<ScreenChoice, 5> kScreenChoices{{
    {"photo", DisplayStateKind::Photo, "01-photo"},
    {"care", DisplayStateKind::Care, "02-care"},
    {"care_due", DisplayStateKind::CareDue, "03-care-due"},
    {"alert", DisplayStateKind::Alert, "04-alert"},
    {"env", DisplayStateKind::Env, "05-env"},
}};

// 写真をどこから取ってくるか。
//
// ★既定は None。理由:
//   --screenshot / --screen all / --run-seconds は **この実装自身の検証手段**
//   であり、既定でネットワークに触ると、証拠として撮るスクリーンショットが
//   毎回変わってしまう。「スイッチが切れている」状態が既定であって、
//   3 つ目のモードではない。
//
//   fixture  … FixtureHttpClient（assets/fixtures/ のファイル）を使う。
//              ★実通信と **同一のクライアントコード** を通る。
//              ただし derivatives/*.jpg に対応する fixture は存在しないので、
//              画像の取得は 404（HTML）になる。これは意図であり、
//              「取得に失敗しても落ちない・巡回を止めない」経路の検証になる。
//   network  … CurlHttpClient で実サーバへ繋ぐ。
enum class PhotoSource { None = 0, Fixture, Network };

// 環境情報（/api/env・/api/roomenv・/api/presence）をどこから取るか。
// ★写真とは別のスイッチにしてある。既定 None の理由は PhotoSource と同じで、
//   --screenshot / --screen all を決定的に保つため。
enum class EnvSource { None = 0, Fixture, Network };

// --inject-index の指定。
struct QuakeInjectSelection {
  bool all = true;      // 応答まるごと（実サーバが返す形）
  std::size_t index = 0;  // all == false のときの添字
};

struct Options {
  fs::path asset_dir{AMBIENT_ASSET_DIR};
  fs::path font_path;      // 空なら asset_dir から導出
  fs::path photo_path;     // 空なら asset_dir から導出
  long font_face_index = 0;
  std::string screenshot_path;  // 空なら通常のインタラクティブ実行
  int window_width = kLogicalWidth;
  int window_height = kLogicalHeight;
  double run_seconds = 0.0;  // 0 以下なら無効（ESC / ウィンドウクローズで終了する通常動作）
  DisplayStateKind screen = DisplayStateKind::Photo;
  bool screen_all = false;   // --screen all（5画面を連続してスクリーンショット）
  int care_item_count = 6;   // お世話カードの枚数（hogo-meter は 3 or 6）
  bool show_help = false;

  // --- 写真の巡回表示 -------------------------------------------------------
  PhotoSource photo_source = PhotoSource::None;
  // ★IP リテラルを使うこと。GeoFrame server は Host ヘッダを検証しており、
  //   IP リテラルは無条件で通る。ホスト名だと 403（本文は HTML）になりうる。
  std::string server_url = "http://127.0.0.1:8080";
  double photo_interval_ms = 8000.0;
  double manifest_interval_ms = 30000.0;  // constants::kPollInterval と同じ
  // デコード後の RGBA バイト数の上限（MB）。0 = ImageWorker の既定（32MB）。
  // ★小さくすると「予算超過 → 一時除外」の経路を実機の写真で踏める。
  double decode_budget_mb = 0.0;

  // --- トランジション（Wave 2B）---------------------------------------------
  // フレームレートの上限（0 = 無制限）。
  // ★Tab5 は Mac より遥かに低い fps でしか描けない可能性が高い。
  //   進捗は経過時間から出るので、fps を落とすと「同じカーブを疎にサンプルする」
  //   だけになる。どの演出が 15fps で見られるかは実機を待たずにここで判る。
  double transition_fps_cap = 0.0;
  // 演出の抽選に使う seed。同じ seed なら同じ演出列になる。
  std::uint64_t transition_seed = 20260730ULL;
  // 演出の見本を順に再生する（空 = 通常動作）。
  std::string transition_demo;
  // 滞在効果。KenBurns は写真レイヤだけを描く（理由は run() 内のコメント）。
  bool dwell_ken_burns = false;
  // 描画側が申告する blend 能力を強制的に false にする（Tab5 の縮退の予行演習）。
  bool force_no_blend = false;

  // --- 環境情報と地震（Wave 3B）----------------------------------------------
  EnvSource env_source = EnvSource::None;
  // ★/api/env はサーバが 300 秒キャッシュするので、これより短くしても
  //   新しい値は出てこない。hogo-meter の POLL_INTERVAL（30 秒）に合わせてある。
  double env_interval_ms = 3000.0 * 10.0;  // = constants::kPollInterval (30000)
  // 環境カットの周期と長さ（企画書 §9 の ENV_CARD_INTERVAL_MIN / PHOTO_DURATION_SEC）。
  double env_card_interval_ms = 10.0 * 60.0 * 1000.0;
  double env_card_duration_ms = 20.0 * 1000.0;

  // 合成地震の注入（assets/fixtures/quakes-inject.json）。
  // 通常応答では高震度が保証されないため、専用fixtureでアラート経路を通す。
  fs::path inject_quake_path;
  QuakeInjectSelection inject_selection;
  double inject_at_seconds = 2.0;
};

void print_usage(const char* argv0) {
  std::printf(
      "使い方: %s [オプション]\n"
      "\n"
      "  --assets <dir>       アセットのルート（既定: %s）\n"
      "  --font <path>        フォントファイル（.ttf / .ttc）\n"
      "  --font-index <n>     フォントの face index（.ttc 用、既定 0）\n"
      "  --photo <path>       全画面 cover 表示する画像\n"
      "  --window-size <WxH>  初期ウィンドウサイズ（既定 1280x720）\n"
      "                       16:9 以外を指定するとレターボックスの確認ができる\n"
      "  --screenshot <path>  1枚だけ描画して保存し終了（.png / .bmp）\n"
      "                       --screen all のときは <path> を **ディレクトリ** として扱う\n"
      "  --run-seconds <N>    N秒間描画してから自動終了する（ウィンドウは出る）。\n"
      "                       終了時に総フレーム数・平均FPS・最小FPSを stderr へ出す。\n"
      "                       最初の1秒（起動コスト）を除いた定常状態の値も別途出す。\n"
      "                       --screenshot と併用可: N秒回してから最後に1枚保存する。\n"
      "  --screen <name>      描画する表示状態（既定 photo）\n"
      "                         photo / care / care_due / alert / env\n"
      "                         all … 5画面を連続で描画してスクリーンショットへ落とす\n"
      "                               （--screenshot <dir> が必須。--run-seconds は無視）\n"
      "  --care-items <3|6>   CARE 画面のカード枚数（既定 6。hogo-meter は 3 or 6）\n"
      "\n"
      "  --photo-source <s>   写真をどこから取るか（既定 none）\n"
      "                         none    … 取りに行かない（fixture 画像を静止表示）\n"
      "                         fixture … assets/fixtures/ を IHttpClient 実装で読む\n"
      "                                   （実通信と同一のクライアントコードを通る）\n"
      "                         network … --server の GeoFrame server から取得する\n"
      "                       ★既定が none なのは、--screenshot 等の検証手段を\n"
      "                         決定的に保つため。写真を巡回させたいときは明示する。\n"
      "  --server <url>       GeoFrame server（既定 http://127.0.0.1:8080）\n"
      "                       ★IP リテラルで指定すること。サーバは Host ヘッダを\n"
      "                         検証しており、ホスト名だと 403 になりうる。\n"
      "  --photo-interval <ms> 写真を切り替える間隔（既定 8000）\n"
      "  --manifest-interval <ms> /api/photos を取り直す間隔（既定 30000）\n"
      "  --decode-budget-mb <N> デコード後 RGBA の上限（既定 32）。超える写真は\n"
      "                       「一時除外」になり、巡回は止まらない。小さくすると\n"
      "                       実機の写真でその経路を確認できる。\n"
      "\n"
      "  --transition-fps-cap <N> 描画を N fps に制限する（既定: 無制限 = vsync 任せ）\n"
      "                       ★Tab5 は Mac より遥かに低い fps でしか描けない可能性が\n"
      "                         高い。進捗は経過時間から出るので、fps を落としても\n"
      "                         演出の長さもカーブも変わらず、**サンプルが疎になる**\n"
      "                         だけになる。15 を指定すると Tab5 相当の見え方を\n"
      "                         実機を待たずに目視評価できる。\n"
      "  --transition-demo <n> 演出の見本を順に再生して終了する\n"
      "                         crossfade / fade_through_black / slide / wipe /\n"
      "                         blinds / kenburns / all\n"
      "                       --screenshot <dir> を併せると進捗 0.05〜0.95 の\n"
      "                       途中経過を PNG で保存する（0 と 1 では演出が見えない）。\n"
      "  --transition-seed <N> 演出の抽選 seed（既定 20260730）。同じ seed なら\n"
      "                       同じ演出列・同じパラメータになる。\n"
      "  --dwell <none|kenburns> 滞在効果（既定 none）。★kenburns を選ぶと写真の\n"
      "                       キャプションが出なくなる（run() 内の申し送りを参照）。\n"
      "  --no-blend           blend が使えない端末のふりをする。CrossFade /\n"
      "                       FadeThroughBlack / KenBurns が抽選から外れ、\n"
      "                       フォールバックが Wipe になる経路を Mac で確認できる。\n"
      "\n"
      "  --env-source <s>     室内環境・天気・在室をどこから取るか（既定 none）\n"
      "                         none / fixture / network\n"
      "                       ★none 以外にすると、表示状態を core の decide() が\n"
      "                         決めるようになる（数字キーの手動切り替えは無効）。\n"
      "  --env-interval <ms>  /api/env・/api/roomenv・/api/presence の取得間隔\n"
      "                       （既定 30000）。★/api/env はサーバが 300 秒\n"
      "                         キャッシュするので、短くしても新しい値は出ない。\n"
      "  --env-card-interval <ms> 環境カットを挟む周期（既定 600000 = 10 分）\n"
      "  --env-card-duration <ms> 環境カットを出し続ける長さ（既定 20000）\n"
      "\n"
      "  --inject-quake <path> 合成地震を注入する（例 assets/fixtures/quakes-inject.json）\n"
      "                       通常応答では高震度が保証されないため、専用fixtureで\n"
      "                         アラート経路を確認する。\n"
      "  --inject-index <n|all> 注入する地震（既定 all = 応答まるごと）\n"
      "                       0 始まりの添字。quakes-inject.json では\n"
      "                       4=震度5- / 5=震度5+ / 8=震度7。\n"
      "  --inject-at <sec>    起動から何秒後に注入するか（既定 2）\n"
      "  --help               このヘルプ\n"
      "\n"
      "例:\n"
      "  # 5画面を /tmp/screens へ 01-photo.png .. 05-env.png として保存\n"
      "  %s --screen all --screenshot /tmp/screens\n"
      "\n"
      "  # 実サーバの写真を巡回表示する（読み取りのみ。POST は一切しない）\n"
      "  %s --photo-source network --server http://127.0.0.1:8080\n"
      "\n"
      "  # 耐久試験（写真を速く回してキャッシュを churn させる）\n"
      "  %s --photo-source network --photo-interval 1000 --run-seconds 1800\n"
      "\n"
      "  # 6 演出を 15fps で見比べ、途中経過を保存する（Tab5 の見え方の予行演習）\n"
      "  %s --transition-demo all --transition-fps-cap 15 --screenshot /tmp/demo15\n"
      "\n"
      "  # 実センサーの値を ENV 画面に出す（読み取りのみ）\n"
      "  %s --env-source network --env-card-interval 4000 --env-card-duration 3000 \\\n"
      "     --run-seconds 6 --screenshot /tmp/env.png\n"
      "\n"
      "  # 震度 5+ の合成地震を注入して ALERT を出す\n"
      "  %s --inject-quake assets/fixtures/quakes-inject.json --inject-index 5 \\\n"
      "     --inject-at 1 --run-seconds 4 --screenshot /tmp/alert.png\n"
      "\n"
      "操作: ESC またはウィンドウを閉じると終了。数字キー 1〜5 で画面を切り替え\n"
      "      （1=PHOTO 2=CARE 3=CARE_DUE 4=ALERT 5=ENV）。★--env-source / \n"
      "      --inject-quake を指定すると表示状態は core の decide() が決めるので、\n"
      "      数字キーは効かない。\n"
      "      クリックで論理座標をログ出力。CARE 画面ではヒットしたカードの\n"
      "      スロット番号も出す（記録の送信は後続フェーズ）。\n"
      "      写真の巡回中は N キーで次の写真へ即座に進む。\n"
      "      --inject-quake 指定時は Q キーで次の合成地震を 1 件ずつ注入する。\n",
      argv0, AMBIENT_ASSET_DIR, argv0, argv0, argv0, argv0, argv0, argv0);
}

// 正の数値オプションを1つ読む。
bool parse_positive_double(const std::string& value, const char* flag, double& out) {
  char* parse_end = nullptr;
  const double parsed = std::strtod(value.c_str(), &parse_end);
  const bool parsed_whole_value =
      !value.empty() && parse_end != nullptr && *parse_end == '\0';
  if (!parsed_whole_value || !(parsed > 0.0)) {
    std::fprintf(stderr, "[app] %s は正の数値で指定してください。\n", flag);
    return false;
  }
  out = parsed;
  return true;
}

// 引数の値を1つ取り出す。足りなければ false。
bool take_value(int argc, char** argv, int& index, const char* flag, std::string& out) {
  if (index + 1 >= argc) {
    std::fprintf(stderr, "[app] %s には値が必要です。\n", flag);
    return false;
  }
  ++index;
  out = argv[index];
  return true;
}

bool parse_options(int argc, char** argv, Options& options) {
  for (int i = 1; i < argc; ++i) {
    const std::string arg = argv[i];
    std::string value;

    if (arg == "--help" || arg == "-h") {
      options.show_help = true;
      return true;
    }
    if (arg == "--assets") {
      if (!take_value(argc, argv, i, "--assets", value)) return false;
      options.asset_dir = value;
    } else if (arg == "--font") {
      if (!take_value(argc, argv, i, "--font", value)) return false;
      options.font_path = value;
    } else if (arg == "--font-index") {
      if (!take_value(argc, argv, i, "--font-index", value)) return false;
      char* parse_end = nullptr;
      const long index = std::strtol(value.c_str(), &parse_end, 10);
      // 値全体を消費できたかまで見る。--run-seconds と同じ流儀。
      // ★空文字列を明示的に弾くこと: strtol("") は 0 を返し parse_end が
      //   即 '\0' を指すため、下の全消費チェックだけでは通ってしまう。
      //   --run-seconds は「正の秒数」制約が空文字列も巻き添えで弾くが、
      //   face index は 0 が正当な値なのでその保険が効かない。
      const bool parsed_whole_value =
          !value.empty() && parse_end != nullptr && *parse_end == '\0';
      if (!parsed_whole_value || index < 0) {
        std::fprintf(stderr,
                     "[app] --font-index は 0 以上の整数で指定してください（例: 0）。\n");
        return false;
      }
      options.font_face_index = index;
    } else if (arg == "--photo") {
      if (!take_value(argc, argv, i, "--photo", value)) return false;
      options.photo_path = value;
    } else if (arg == "--screenshot") {
      if (!take_value(argc, argv, i, "--screenshot", value)) return false;
      options.screenshot_path = value;
    } else if (arg == "--run-seconds") {
      if (!take_value(argc, argv, i, "--run-seconds", value)) return false;
      char* parse_end = nullptr;
      const double seconds = std::strtod(value.c_str(), &parse_end);
      const bool parsed_whole_value = parse_end != nullptr && *parse_end == '\0';
      if (!parsed_whole_value || !(seconds > 0.0)) {
        std::fprintf(stderr,
                     "[app] --run-seconds は正の秒数で指定してください（例: 5）。\n");
        return false;
      }
      options.run_seconds = seconds;
    } else if (arg == "--screen") {
      if (!take_value(argc, argv, i, "--screen", value)) return false;
      if (value == "all") {
        options.screen_all = true;
      } else {
        bool matched = false;
        for (const ScreenChoice& choice : kScreenChoices) {
          if (value == choice.name) {
            options.screen = choice.kind;
            matched = true;
            break;
          }
        }
        if (!matched) {
          std::fprintf(stderr,
                       "[app] --screen の値が不正です: %s\n"
                       "      photo / care / care_due / alert / env / all のいずれかを"
                       "指定してください。\n",
                       value.c_str());
          return false;
        }
      }
    } else if (arg == "--care-items") {
      if (!take_value(argc, argv, i, "--care-items", value)) return false;
      if (value != "3" && value != "6") {
        std::fprintf(stderr, "[app] --care-items は 3 か 6 で指定してください。\n");
        return false;
      }
      options.care_item_count = (value == "3") ? 3 : 6;
    } else if (arg == "--photo-source") {
      if (!take_value(argc, argv, i, "--photo-source", value)) return false;
      if (value == "none") {
        options.photo_source = PhotoSource::None;
      } else if (value == "fixture") {
        options.photo_source = PhotoSource::Fixture;
      } else if (value == "network") {
        options.photo_source = PhotoSource::Network;
      } else {
        std::fprintf(stderr,
                     "[app] --photo-source は none / fixture / network の"
                     "いずれかで指定してください: %s\n",
                     value.c_str());
        return false;
      }
    } else if (arg == "--server") {
      if (!take_value(argc, argv, i, "--server", value)) return false;
      if (value.empty()) {
        std::fprintf(stderr, "[app] --server が空です。\n");
        return false;
      }
      options.server_url = value;
    } else if (arg == "--photo-interval") {
      if (!take_value(argc, argv, i, "--photo-interval", value)) return false;
      if (!parse_positive_double(value, "--photo-interval", options.photo_interval_ms)) {
        return false;
      }
    } else if (arg == "--manifest-interval") {
      if (!take_value(argc, argv, i, "--manifest-interval", value)) return false;
      if (!parse_positive_double(value, "--manifest-interval",
                                 options.manifest_interval_ms)) {
        return false;
      }
    } else if (arg == "--decode-budget-mb") {
      if (!take_value(argc, argv, i, "--decode-budget-mb", value)) return false;
      if (!parse_positive_double(value, "--decode-budget-mb", options.decode_budget_mb)) {
        return false;
      }
    } else if (arg == "--transition-fps-cap") {
      if (!take_value(argc, argv, i, "--transition-fps-cap", value)) return false;
      if (!parse_positive_double(value, "--transition-fps-cap", options.transition_fps_cap)) {
        return false;
      }
    } else if (arg == "--transition-seed") {
      if (!take_value(argc, argv, i, "--transition-seed", value)) return false;
      char* parse_end = nullptr;
      const unsigned long long parsed = std::strtoull(value.c_str(), &parse_end, 10);
      const bool parsed_whole_value =
          !value.empty() && parse_end != nullptr && *parse_end == '\0';
      if (!parsed_whole_value) {
        std::fprintf(stderr, "[app] --transition-seed は 0 以上の整数で指定してください。\n");
        return false;
      }
      options.transition_seed = static_cast<std::uint64_t>(parsed);
    } else if (arg == "--transition-demo") {
      if (!take_value(argc, argv, i, "--transition-demo", value)) return false;
      options.transition_demo = value;
    } else if (arg == "--dwell") {
      if (!take_value(argc, argv, i, "--dwell", value)) return false;
      if (value == "none") {
        options.dwell_ken_burns = false;
      } else if (value == "kenburns") {
        options.dwell_ken_burns = true;
      } else {
        std::fprintf(stderr, "[app] --dwell は none / kenburns で指定してください: %s\n",
                     value.c_str());
        return false;
      }
    } else if (arg == "--no-blend") {
      options.force_no_blend = true;
    } else if (arg == "--env-source") {
      if (!take_value(argc, argv, i, "--env-source", value)) return false;
      if (value == "none") {
        options.env_source = EnvSource::None;
      } else if (value == "fixture") {
        options.env_source = EnvSource::Fixture;
      } else if (value == "network") {
        options.env_source = EnvSource::Network;
      } else {
        std::fprintf(stderr,
                     "[app] --env-source は none / fixture / network の"
                     "いずれかで指定してください: %s\n",
                     value.c_str());
        return false;
      }
    } else if (arg == "--env-interval") {
      if (!take_value(argc, argv, i, "--env-interval", value)) return false;
      if (!parse_positive_double(value, "--env-interval", options.env_interval_ms)) {
        return false;
      }
    } else if (arg == "--env-card-interval") {
      if (!take_value(argc, argv, i, "--env-card-interval", value)) return false;
      if (!parse_positive_double(value, "--env-card-interval",
                                 options.env_card_interval_ms)) {
        return false;
      }
    } else if (arg == "--env-card-duration") {
      if (!take_value(argc, argv, i, "--env-card-duration", value)) return false;
      if (!parse_positive_double(value, "--env-card-duration",
                                 options.env_card_duration_ms)) {
        return false;
      }
    } else if (arg == "--inject-quake") {
      if (!take_value(argc, argv, i, "--inject-quake", value)) return false;
      options.inject_quake_path = value;
    } else if (arg == "--inject-index") {
      if (!take_value(argc, argv, i, "--inject-index", value)) return false;
      if (value == "all") {
        options.inject_selection.all = true;
      } else {
        char* parse_end = nullptr;
        const long index = std::strtol(value.c_str(), &parse_end, 10);
        const bool parsed_whole_value =
            !value.empty() && parse_end != nullptr && *parse_end == '\0';
        if (!parsed_whole_value || index < 0) {
          std::fprintf(stderr,
                       "[app] --inject-index は 0 以上の整数か all で"
                       "指定してください。\n");
          return false;
        }
        options.inject_selection.all = false;
        options.inject_selection.index = static_cast<std::size_t>(index);
      }
    } else if (arg == "--inject-at") {
      if (!take_value(argc, argv, i, "--inject-at", value)) return false;
      if (!parse_positive_double(value, "--inject-at", options.inject_at_seconds)) {
        return false;
      }
    } else if (arg == "--window-size") {
      if (!take_value(argc, argv, i, "--window-size", value)) return false;
      int width = 0;
      int height = 0;
      if (std::sscanf(value.c_str(), "%dx%d", &width, &height) != 2 || width <= 0 ||
          height <= 0) {
        std::fprintf(stderr, "[app] --window-size は WxH 形式（例 1600x900）で指定してください。\n");
        return false;
      }
      options.window_width = width;
      options.window_height = height;
    } else {
      std::fprintf(stderr, "[app] 不明なオプション: %s\n", arg.c_str());
      return false;
    }
  }
  return true;
}

// 見つからないアセットは、豆腐や黒画面で黙って動くより即座に落とす。
bool require_file(const fs::path& path, const char* what, const char* remedy) {
  std::error_code ec;
  if (fs::is_regular_file(path, ec)) {
    return true;
  }
  std::fprintf(stderr,
               "\n[app] ★エラー★ %sが見つかりません。\n"
               "  探した場所: %s\n"
               "  → %s\n\n",
               what, path.c_str(), remedy);
  return false;
}

// 入力座標の逆変換が正しく効いているかを起動時に確認してログへ出す。
//
// ウィンドウの四隅と中心を論理座標へ変換する。期待値:
//   * 左上   -> (0, 0) 付近
//   * 右下   -> (1280, 720) 付近
//   * 中心   -> (640, 360) 付近
//   * レターボックスの黒帯にかかる隅は範囲外の値（負値や 1280/720 超）
// ウィンドウを 16:9 以外にして実行すると、黒帯ぶんのズレがここに現れる。
void log_input_mapping_selfcheck(const ambient::render::Display& display) {
  int window_w = 0;
  int window_h = 0;
  SDL_GetWindowSize(display.window(), &window_w, &window_h);

  const struct {
    const char* label;
    int x;
    int y;
  } probes[] = {
      {"左上  ", 0, 0},
      {"右上  ", window_w - 1, 0},
      {"中心  ", window_w / 2, window_h / 2},
      {"左下  ", 0, window_h - 1},
      {"右下  ", window_w - 1, window_h - 1},
  };

  std::fprintf(stderr, "[app] 入力座標マッピング自己確認（ウィンドウ %dx%d pt）\n",
               window_w, window_h);
  for (const auto& probe : probes) {
    const ambient::render::LogicalPoint point = display.window_to_logical(probe.x, probe.y);
    const bool inside = point.x >= 0.0F && point.y >= 0.0F &&
                        point.x <= static_cast<float>(kLogicalWidth) &&
                        point.y <= static_cast<float>(kLogicalHeight);
    std::fprintf(stderr, "[app]   %s window=(%4d,%4d) -> logical=(%8.1f,%8.1f) %s\n",
                 probe.label, probe.x, probe.y,
                 static_cast<double>(point.x), static_cast<double>(point.y),
                 inside ? "内側" : "← 黒帯（範囲外）");
  }
}

// ---------------------------------------------------------------------------
// 表示に使うサンプルデータ
// ---------------------------------------------------------------------------
//
// ★Phase 0 は **ネットワーク完全遮断** で 5 画面を出せることが完了条件なので、
//   ここでは通信も JSON パースも行わない。
//
//   ambient_render_sdl / ambient_display は JSON ライブラリをリンクしていない
//   （CMakeLists.txt の層構造コメント参照）ので、fixture を実行時に読むことも
//   できない。したがって assets/fixtures/ の実物を **目で見て写した代表値** を
//   ここに直書きする。fixture と対応が取れるよう、出典を各所に書いてある。
//
//   ⚠️ 地名・座標は合成値（プライバシー対応済み）。実在の地名ではない。
//
//   ★ここで組み立てているのは「描画のための結論」であって生データではない。
//     CareStatus（Green/Yellow/Red）はサーバが計算して返す値、EnvLevel は
//     しきい値判定の結果を写したもので、しきい値そのものはどこにも書かない。
//     後続フェーズではこの関数が「core の model → View Model」の変換に変わる。
ambient::render::ScreenData make_sample_screen_data(int care_item_count) {
  using ambient::render::CareCard;
  using ambient::render::CareStatus;
  using ambient::render::EnvLevel;
  using ambient::render::EnvMetric;
  using ambient::render::EnvWeeklyDay;

  ambient::render::ScreenData data;

  // --- PHOTO: assets/fixtures/api/photos.json の先頭エントリ相当 ------------
  data.photo.title = "A町せせらぎ";
  data.photo.place = "A県 A市 A町";
  data.photo.taken_at = "2024-01-15 09:00 撮影";
  data.photo.clock = "14:26";

  // --- CARE: hogo-meter の CareItem スロット（app_types.h）------------------
  // slot 0=トイレ / 1=目薬 / 2=ご飯。slot 3-5 は Tab5 の 6 項目モード専用で、
  // 用途はダッシュボードのラベルで決まる中立スロット。
  data.care.heading = "お世話の記録";
  data.care.hint = "カードをタップで記録 / 2秒間は取り消せます";
  data.care.footer = "経過時間と状態色は hogo-meter サーバの値です（端末側では計算しません）";
  const CareCard all_cards[] = {
      {0, "トイレ", "12時間04分 経過", "最終 02:22", CareStatus::Red},
      {1, "目薬", "6時間30分 経過", "最終 07:56", CareStatus::Yellow},
      {2, "ご飯", "1時間12分 経過", "最終 13:14", CareStatus::Green},
      {3, "お水", "2時間40分 経過", "最終 11:46", CareStatus::Green},
      {4, "あそび", "記録なし", "最終 --:--", CareStatus::None},
      {5, "体重", "20時間05分 経過", "最終 昨日 18:21", CareStatus::Yellow},
  };
  const std::size_t care_count =
      (care_item_count <= 3) ? 3U : std::size(all_cards);
  data.care.cards.assign(all_cards, all_cards + care_count);

  // --- CARE_DUE: 写真の上へ重ねる期限通知 -----------------------------------
  data.care_due.status = CareStatus::Red;
  data.care_due.headline = "トイレ のお世話が期限を過ぎています";
  data.care_due.detail = "前回の記録から 12時間04分 経過しています";
  data.care_due.elapsed = "12:04";
  data.care_due.action_hint = "画面をタップするとお世話画面へ切り替わります";

  // --- ALERT: assets/fixtures/quakes-inject.json の FIXTURE-0006 ------------
  // ★maxi は "5+"。整数化すると弱強の別が静かに消えるので、
  //   文字列のまま View Model へ渡して、そのまま描く。
  data.alert.title = "地震速報";
  data.alert.intensity = "5+";
  data.alert.magnitude = "M6.1";
  data.alert.place = "己県北部";
  data.alert.occurred = "2026-07-30 14:00 発生";
  data.alert.note = "落ち着いて身の安全を確保してください（合成フィクスチャの表示例です）";

  // --- ENV: assets/fixtures/api/roomenv.json と api/env.json ----------------
  data.env.heading = "室内環境と天気";
  data.env.hint = "Home Assistant 経由 / 14:26 取得";
  data.env.metrics = {
      EnvMetric{"室温", "22.5", "℃", EnvLevel::Normal, false},
      EnvMetric{"湿度", "48.0", "%", EnvLevel::Normal, false},
      EnvMetric{"CO2", "650", "ppm", EnvLevel::Normal, false},
  };
  // env.json の weather
  data.env.weather_area = "仮想北部";
  data.env.weather_text = "晴れ時々くもり";
  data.env.weather_temps = "最高 25℃";
  data.env.weather_pop = "降水確率 30%";
  // env.json の weekly（先頭日は pop / temp_min / temp_max が null）
  data.env.weekly = {
      EnvWeeklyDay{"6/2", "--", "--"}, EnvWeeklyDay{"6/3", "24", "17"},
      EnvWeeklyDay{"6/4", "25", "18"}, EnvWeeklyDay{"6/5", "26", "16"},
      EnvWeeklyDay{"6/6", "23", "17"}, EnvWeeklyDay{"6/7", "24", "18"},
      EnvWeeklyDay{"6/8", "25", "16"},
  };

  return data;
}

// 開発用の HUD（FPS と直近クリック）。実運用の画面要素ではないので
// スクリーンショットの一括取得（--screen all）では描かない。
void draw_dev_hud(SDL_Renderer* renderer, const ambient::render::Font& hud_font,
                  double fps, const ambient::render::LogicalPoint& last_click,
                  bool has_click) {
  ambient::render::TextStyle hud_style;
  hud_style.color = SDL_Color{150, 240, 190, 255};

  char buffer[128];
  if (fps > 0.0) {
    std::snprintf(buffer, sizeof(buffer), "%.1f FPS", fps);
  } else {
    std::snprintf(buffer, sizeof(buffer), "-- FPS");
  }
  ambient::render::draw_text(renderer, hud_font, buffer, kLogicalWidth - 24, 8,
                             hud_style, ambient::render::TextAlign::kRight);

  if (has_click) {
    std::snprintf(buffer, sizeof(buffer), "click(logical) %.1f, %.1f",
                  static_cast<double>(last_click.x),
                  static_cast<double>(last_click.y));
    ambient::render::draw_text(renderer, hud_font, buffer, kLogicalWidth - 24,
                               kLogicalHeight - 34, hud_style,
                               ambient::render::TextAlign::kRight);
  }
}

// 1フレーム描画する。present はしない（スクリーンショットを present 前に撮るため）。
void render_frame(ambient::render::Display& display,
                  const ambient::render::ScreenFonts& fonts,
                  const ambient::render::Font& hud_font,
                  const ambient::render::Texture* photo,
                  DisplayStateKind screen,
                  const ambient::render::ScreenData& data,
                  double fps,
                  const ambient::render::LogicalPoint& last_click,
                  bool has_click,
                  bool show_hud) {
  SDL_Renderer* renderer = display.renderer();

  SDL_SetRenderDrawColor(renderer, 0, 0, 0, 255);
  SDL_RenderClear(renderer);

  const ambient::render::ScreenLayout layout{kLogicalWidth, kLogicalHeight};
  // ★「どの画面を出すか」の判断はここに書かない。screen は呼び出し側
  //   （今は --screen / 数字キー、後続フェーズでは core の decide()）が決める。
  // ★photo は「今表示すべき背景テクスチャ」。--photo-source で取得した写真か、
  //   取得していなければ fixture の静止画。draw_screen 側は区別しない。
  ambient::render::draw_screen(renderer, screen, fonts, photo, data, layout);

  if (show_hud) {
    draw_dev_hud(renderer, hud_font, fps, last_click, has_click);
  }
}

// --run-seconds の終了サマリ用に、フレームごとの所要時間を集計する。
//
// 画面右上に出す HUD の FPS（kFpsWindowSeconds 窓の平均）とは別系統。
// あちらは「今何 FPS で動いているか」を見せるための一定時間平均で、
// こちらは「実行全体を通しての平均・最悪ケース」を終了時にまとめて出すためのもの。
struct RunTimingStats {
  Uint64 start_counter = 0;
  Uint64 prev_frame_end_counter = 0;
  long total_frames = 0;
  double total_elapsed_seconds = 0.0;
  double max_frame_seconds = 0.0;  // 最長フレーム時間。この逆数が最小 FPS になる。

  // 最初の1秒（ウィンドウ生成やテクスチャ転送などの起動コストが乗る区間）を
  // 除いた集計。定常状態の FPS を見るため。
  long post_warmup_frames = 0;
  double max_frame_seconds_post_warmup = 0.0;
};

constexpr double kRunStatsWarmupSeconds = 1.0;

void record_frame_timing(RunTimingStats& stats, double counter_frequency) {
  const Uint64 now = SDL_GetPerformanceCounter();
  const double frame_seconds =
      static_cast<double>(now - stats.prev_frame_end_counter) / counter_frequency;
  stats.prev_frame_end_counter = now;
  stats.total_elapsed_seconds =
      static_cast<double>(now - stats.start_counter) / counter_frequency;
  ++stats.total_frames;

  if (frame_seconds > stats.max_frame_seconds) {
    stats.max_frame_seconds = frame_seconds;
  }
  if (stats.total_elapsed_seconds > kRunStatsWarmupSeconds) {
    ++stats.post_warmup_frames;
    if (frame_seconds > stats.max_frame_seconds_post_warmup) {
      stats.max_frame_seconds_post_warmup = frame_seconds;
    }
  }
}

void print_run_seconds_summary(const RunTimingStats& stats) {
  std::fprintf(stderr, "\n[app] ===== --run-seconds サマリ =====\n");
  std::fprintf(stderr, "[app] 総フレーム数         : %ld\n", stats.total_frames);
  std::fprintf(stderr, "[app] 経過秒数             : %.3f 秒\n", stats.total_elapsed_seconds);

  if (stats.total_elapsed_seconds > 0.0) {
    const double avg_fps =
        static_cast<double>(stats.total_frames) / stats.total_elapsed_seconds;
    std::fprintf(stderr, "[app] 平均 FPS（起動コスト込み）: %.1f\n", avg_fps);
  }
  if (stats.max_frame_seconds > 0.0) {
    std::fprintf(stderr, "[app] 最小 FPS（起動コスト込み）: %.1f （最長フレーム %.1f ms）\n",
                 1.0 / stats.max_frame_seconds, stats.max_frame_seconds * 1000.0);
  }

  const double post_warmup_elapsed = stats.total_elapsed_seconds - kRunStatsWarmupSeconds;
  std::fprintf(stderr, "[app] --- 最初の %.0f 秒（起動コスト）を除いた定常状態 ---\n",
               kRunStatsWarmupSeconds);
  if (post_warmup_elapsed > 0.0 && stats.post_warmup_frames > 0) {
    const double avg_fps_post =
        static_cast<double>(stats.post_warmup_frames) / post_warmup_elapsed;
    std::fprintf(stderr, "[app] フレーム数           : %ld（経過 %.3f 秒）\n",
                 stats.post_warmup_frames, post_warmup_elapsed);
    std::fprintf(stderr, "[app] 平均 FPS             : %.1f\n", avg_fps_post);
    if (stats.max_frame_seconds_post_warmup > 0.0) {
      std::fprintf(stderr, "[app] 最小 FPS             : %.1f （最長フレーム %.1f ms）\n",
                   1.0 / stats.max_frame_seconds_post_warmup,
                   stats.max_frame_seconds_post_warmup * 1000.0);
    }
  } else {
    std::fprintf(stderr,
                 "[app] --run-seconds が %.0f 秒以下のため計測できませんでした"
                 "（もっと長く回してください）。\n",
                 kRunStatsWarmupSeconds);
  }
  std::fprintf(stderr, "[app] ================================\n\n");
}

// ===========================================================================
// 写真の巡回表示（Wave 2C）
// ===========================================================================
//
// 層の分担:
//
//   ManifestPoller   … /api/photos を **別スレッド** で取り直す（app 層）
//   ImageWorker      … 画像の DL / probe / 予算判定 / decode / 縮小（render 層）
//   PhotoSlideshow   … 「次にどれを出すか」の巡回（app 層。純粋な配線）
//   LivePhotoTextures… RGBA -> SDL_Texture（メインスレッド専用）
//
// ★ManifestPoller が別スレッドである理由:
//   fetch はサーバが落ちていると接続タイムアウトぶん（既定 2 秒）ブロックする。
//   これを描画ループで呼ぶと 60fps が崩れ、「サーバが落ちても描画は続く」という
//   要件も満たせない。
//
// ★スレッドは run() の中で join される（unique_ptr の破棄がデストラクタを呼ぶ）。
//   SDL のシャットダウンより前に必ず終わる。

// ---------------------------------------------------------------------------
// ManifestPoller — /api/photos を別スレッドで取り直す
// ---------------------------------------------------------------------------
class ManifestPoller {
 public:
  struct Update {
    ambient::core::PhotoManifest manifest;
    ambient::transport::GeoFrameStatus status =
        ambient::transport::GeoFrameStatus::Ok;
    std::string message;
  };

  // http はこのオブジェクトが所有し、**このスレッドからだけ** 使う
  // （libcurl の easy handle はスレッド間で共有できない）。
  ManifestPoller(std::unique_ptr<ambient::core::IHttpClient> http,
                 ambient::transport::GeoFrameConfig config, double interval_ms)
      : http_(std::move(http)), config_(std::move(config)), interval_ms_(interval_ms) {
    thread_ = std::thread(&ManifestPoller::thread_main, this);
  }

  ~ManifestPoller() {
    {
      const std::lock_guard<std::mutex> lock(mutex_);
      stop_ = true;
    }
    cv_.notify_all();
    if (thread_.joinable()) {
      thread_.join();
    }
  }

  ManifestPoller(const ManifestPoller&) = delete;
  ManifestPoller& operator=(const ManifestPoller&) = delete;

  // 新しい結果があれば true を返して out へ移す（ノンブロッキング）。
  bool take(Update& out) {
    const std::lock_guard<std::mutex> lock(mutex_);
    if (!pending_.has_value()) {
      return false;
    }
    out = std::move(*pending_);
    pending_.reset();
    return true;
  }

  [[nodiscard]] std::size_t poll_count() const {
    const std::lock_guard<std::mutex> lock(mutex_);
    return poll_count_;
  }

 private:
  void thread_main() {
    ambient::transport::GeoFrameClient client{*http_, config_};

    for (;;) {
      Update update;

      // ★★ 例外の境界（この 1 周期だけを失敗にする）★★
      //
      //   本プロジェクトは「例外を制御フローに使わない」規律で書かれているが、
      //   `reserve` / `std::string` の確保のように **規律の外から飛んでくる**
      //   例外がある。スレッド本体から例外が逃げると std::terminate に直行し、
      //   **プロセス全体が落ちる**（描画も含めて）。ここで受け止める。
      //
      //   ★try は for(;;) の **内側** に置くこと。ループ全体を囲むと、1 回の
      //     例外でポーリングが黙って永久に止まり、画面が二度と更新されない。
      //   ★catch から抜けたあとも下の wait_for を必ず通ること。catch の中で
      //     continue すると sleep を飛ばしてサーバを叩き続ける（ホットスピン）。
      //   ★catch の中で確保しない（fprintf は C++ のヒープを使わない）。
      //     ヒープ枯渇そのものからの復帰は保証できない。ここが担保するのは
      //     「一時的な bad_alloc で std::terminate に行かないこと」までである。
      //   ★この境界は app 層にだけ置く。ambient_core には入れないこと
      //     （Tab5 は緊急用プールが 0 で、ヒープ枯渇時の throw が abort になる）。
      try {
        const ambient::transport::GeoFrameResult result =
            client.fetchPhotos(update.manifest);
        update.status = result.status;
        update.message = result.message;
      } catch (const std::exception& ex) {
        std::fprintf(stderr,
                     "[manifest-poller] この周期を破棄しました（例外: %s）\n", ex.what());
        update = Update{};  // 途中まで作った manifest を渡さない
        update.status = ambient::transport::GeoFrameStatus::TransportFailed;
        update.message = "例外により 1 周期を破棄";
      } catch (...) {
        std::fprintf(stderr, "[manifest-poller] この周期を破棄しました（不明な例外）\n");
        update = Update{};
        update.status = ambient::transport::GeoFrameStatus::TransportFailed;
        update.message = "不明な例外により 1 周期を破棄";
      }

      {
        const std::lock_guard<std::mutex> lock(mutex_);
        ++poll_count_;
        // ★取りこぼしても構わない（次の poll で最新が来る）。溜めない。
        pending_ = std::move(update);
        if (stop_) {
          return;
        }
      }

      std::unique_lock<std::mutex> lock(mutex_);
      cv_.wait_for(lock, std::chrono::milliseconds(static_cast<long long>(interval_ms_)),
                   [this] { return stop_; });
      if (stop_) {
        return;
      }
    }
  }

  std::unique_ptr<ambient::core::IHttpClient> http_;
  ambient::transport::GeoFrameConfig config_;
  double interval_ms_;

  mutable std::mutex mutex_;
  std::condition_variable cv_;
  bool stop_ = false;
  std::optional<Update> pending_;
  std::size_t poll_count_ = 0;
  std::thread thread_;
};

// ===========================================================================
// 環境情報のポーリング（Wave 3B）
// ===========================================================================
//
// ★ManifestPoller と同じく **別スレッド** である。理由も同じ:
//   fetch はサーバが落ちていると接続タイムアウト（既定 5 秒）ぶんブロックする。
//   描画ループで呼ぶと 60fps も「サーバが落ちても描画は続く」も同時に失う。
//
// ★このスレッドは **パースまで**しかしない。AlertEngine / EnvironmentModel への
//   投入（ingest）はメインスレッドで行う。Core は単一スレッド前提であり、
//   collect() は描画ループから呼ばれるためである（docs/PORTABILITY.md）。
//
// ★1 周期で 3 本の GET を投げる。実サーバへの負荷は
//   3 / env_interval（既定 30 秒）= 0.1 req/s。実測値は終了時に出す。
class EnvPoller {
 public:
  struct Update {
    ambient::core::EnvSnapshot env;
    ambient::core::RoomEnvSnapshot room;
    ambient::core::PresenceSnapshot presence;
    bool env_ok = false;
    bool room_ok = false;       // NotConfigured も「取得できた」に含める
    bool presence_ok = false;
    bool room_not_configured = false;
    std::string message;        // 最初の失敗の診断（ユーザ向け文言ではない）

    // ★「初回かどうか」はここに持たせない。持たせると
    //   「取得したが失敗した周期」でフラグだけが消費され、次の成功応答が
    //   firstLoad=false で入る = 過去 24 時間ぶんの地震が一斉発火する。
    //   判定は **実際に ingest する側**（メインスレッド）が持つ。
  };

  EnvPoller(std::unique_ptr<ambient::core::IHttpClient> http,
            ambient::transport::GeoFrameConfig config, double interval_ms)
      : http_(std::move(http)), config_(std::move(config)), interval_ms_(interval_ms) {
    thread_ = std::thread(&EnvPoller::thread_main, this);
  }

  ~EnvPoller() {
    {
      const std::lock_guard<std::mutex> lock(mutex_);
      stop_ = true;
    }
    cv_.notify_all();
    if (thread_.joinable()) {
      thread_.join();
    }
  }

  EnvPoller(const EnvPoller&) = delete;
  EnvPoller& operator=(const EnvPoller&) = delete;

  bool take(Update& out) {
    const std::lock_guard<std::mutex> lock(mutex_);
    if (!pending_.has_value()) {
      return false;
    }
    out = std::move(*pending_);
    pending_.reset();
    return true;
  }

  [[nodiscard]] std::size_t poll_count() const {
    const std::lock_guard<std::mutex> lock(mutex_);
    return poll_count_;
  }

  // ★実サーバへ投げた GET の実数。ポーリング周期の設定ミスは
  //   「表示は正常に見えるがログを読むまで気づけない」種類の事故になるので、
  //   必ず数えて終了時に出す（Wave 2 の LRU 事故と同じ轍を踏まない）。
  [[nodiscard]] std::size_t request_count() const {
    const std::lock_guard<std::mutex> lock(mutex_);
    return request_count_;
  }

 private:
  void thread_main() {
    ambient::transport::GeoFrameClient client{*http_, config_};

    for (;;) {
      Update update;

      // ★★ 例外の境界（ManifestPoller::thread_main と同じ理由・同じ形）★★
      //   規律の外（reserve / 文字列の確保）から飛んできた例外をここで受け、
      //   **この 1 周期だけを失敗**にする。スレッドから逃がすと std::terminate
      //   でプロセスごと落ちる。try はループの内側、catch のあとも wait_for を
      //   必ず通す（飛ばすとホットスピンになる）。core には置かないこと。
      try {
        // ★1 周期で 3 本。まとめて何本も投げない（core/http.hpp §8.3 の規律）。
        {
          const ambient::transport::GeoFrameResult result = client.fetchEnv(update.env);
          update.env_ok = result.ok();
          if (!result.ok() && update.message.empty()) {
            update.message = std::string("/api/env: ") + result.message + " (" +
                             ambient::transport::toString(result.status) + ")";
          }
        }
        {
          const ambient::transport::GeoFrameResult result = client.fetchRoomEnv(update.room);
          // ★NotConfigured は障害ではない（Home Assistant 未設定）。
          //   degraded() が false を返すのはそのため。ここで失敗扱いにしない。
          update.room_not_configured =
              result.status == ambient::transport::GeoFrameStatus::NotConfigured;
          update.room_ok = result.ok() || update.room_not_configured;
          if (!update.room_ok && update.message.empty()) {
            update.message = std::string("/api/roomenv: ") + result.message + " (" +
                             ambient::transport::toString(result.status) + ")";
          }
        }
        {
          const ambient::transport::GeoFrameResult result =
              client.fetchPresence(update.presence);
          update.presence_ok = result.ok();
          if (!result.ok() && update.message.empty()) {
            update.message = std::string("/api/presence: ") + result.message + " (" +
                             ambient::transport::toString(result.status) + ")";
          }
        }
      } catch (const std::exception& ex) {
        std::fprintf(stderr, "[env-poller] この周期を破棄しました（例外: %s）\n", ex.what());
        // 途中まで埋まった snapshot を渡さない。全 ok=false = この周期は失敗。
        update = Update{};
        update.message = "例外により 1 周期を破棄";
      } catch (...) {
        std::fprintf(stderr, "[env-poller] この周期を破棄しました（不明な例外）\n");
        update = Update{};
        update.message = "不明な例外により 1 周期を破棄";
      }

      {
        const std::lock_guard<std::mutex> lock(mutex_);
        ++poll_count_;
        request_count_ += 3;
        // 取りこぼしても構わない（次の周期で最新が来る）。溜めない。
        pending_ = std::move(update);
        if (stop_) {
          return;
        }
      }

      std::unique_lock<std::mutex> lock(mutex_);
      cv_.wait_for(lock, std::chrono::milliseconds(static_cast<long long>(interval_ms_)),
                   [this] { return stop_; });
      if (stop_) {
        return;
      }
    }
  }

  std::unique_ptr<ambient::core::IHttpClient> http_;
  ambient::transport::GeoFrameConfig config_;
  double interval_ms_;

  mutable std::mutex mutex_;
  std::condition_variable cv_;
  bool stop_ = false;
  std::optional<Update> pending_;
  std::size_t poll_count_ = 0;
  std::size_t request_count_ = 0;
  std::thread thread_;
};

// ---------------------------------------------------------------------------
// LivePhotoTextures — ★なぜ一時 PNG を経由するのか
// ---------------------------------------------------------------------------
//
// ambient::render::Texture の生成手段は `Texture::load(renderer, path, error)`
// だけで、**デコード済みのピクセルから作る factory が無い**。つまり
// ImageWorker が作った RGBA バッファをそのまま `draw_screen()` へ渡す経路が
// 存在しない。
//
// 本来の直し方は image.hpp に `Texture::from_rgba()` を足すことだが、
// このファイルは Wave 2C の所有範囲外（他レーンが同時に触りうる）なので
// 触らない。代わりに次の手を採っている:
//
//   1. 起動時に 1280x720 の PNG を一時ファイルへ **1 枚だけ** 書いて
//      Texture::load で読む。一時ファイルはすぐ削除する。
//   2. 写真が切り替わるたびに SDL_UpdateTexture() で中身を差し替える。
//      3.5MB の転送で 1〜2ms。
//
// 写真ごとに PNG を書き出して load し直す案は、デコードだけで 15ms 前後かかり
// 最小 FPS に見える形で効くので採らない。
//
// ★この方式なら draw_screen() を一切変更せずに済む。1C が作ったスクリムと
//   キャプションの意匠が `--screen photo` と完全に同じ経路で描かれる。
//
// ★テクスチャを 2 枚持つ理由: 「今の写真」と「直前の写真」の両方を
//   同時に持てるようにするため（Wave 2B のトランジションが必要とする）。
//
//   ⚠️ 2 枚しか無いので、**トランジションの実行中に次の切り替えが起きると
//      遷移元テクスチャが上書きされる**。写真の間隔（既定 8000ms）が
//      トランジションの長さ（600〜1000ms）より十分に長い前提である。
//      短い間隔で回すなら 3 枚に増やすか、トランジション中は切り替えを
//      抑止すること。
class LivePhotoTextures {
 public:
  static std::unique_ptr<LivePhotoTextures> create(SDL_Renderer* renderer, int width,
                                                   int height, std::string& error) {
    SDL_Surface* surface =
        SDL_CreateRGBSurfaceWithFormat(0, width, height, 32, SDL_PIXELFORMAT_RGBA32);
    if (surface == nullptr) {
      error = std::string("土台サーフェスを作れませんでした: ") + SDL_GetError();
      return nullptr;
    }
    SDL_FillRect(surface, nullptr, SDL_MapRGBA(surface->format, 18, 20, 24, 255));

    // 同じミリ秒に 2 プロセスが起動しても衝突しないよう PID を混ぜる。
    std::error_code ec;
    const fs::path temp_path =
        fs::temp_directory_path(ec) /
        ("ambient-photo-base-" + std::to_string(static_cast<long long>(getpid())) + "-" +
         std::to_string(SDL_GetTicks64()) + ".png");
    const int saved = IMG_SavePNG(surface, temp_path.c_str());
    SDL_FreeSurface(surface);
    if (saved != 0) {
      error = std::string("土台 PNG を書き出せませんでした: ") + IMG_GetError();
      return nullptr;
    }

    auto holder = std::unique_ptr<LivePhotoTextures>(new LivePhotoTextures());
    holder->width_ = width;
    holder->height_ = height;
    for (auto& slot : holder->slots_) {
      slot = ambient::render::Texture::load(renderer, temp_path.string(), error);
      if (slot == nullptr) {
        fs::remove(temp_path, ec);
        return nullptr;
      }
    }
    fs::remove(temp_path, ec);

    // ★SDL_CreateTextureFromSurface は **テクスチャの形式を自分で決める**。
    //   SDL_UpdateTexture はテクスチャの形式のピクセルを要求するので、
    //   RGBA32 でなければ赤と青が入れ替わった「それっぽく見える誤り」になる。
    //   ここで実際の形式を確認し、必要なら変換を挟む。
    Uint32 format = SDL_PIXELFORMAT_UNKNOWN;
    SDL_QueryTexture(holder->slots_[0]->raw(), &format, nullptr, nullptr, nullptr);
    holder->format_ = format;
    std::fprintf(stderr, "[app] 写真テクスチャ形式: %s（RGBA32=%s。%s）\n",
                 SDL_GetPixelFormatName(format),
                 SDL_GetPixelFormatName(SDL_PIXELFORMAT_RGBA32),
                 (format == SDL_PIXELFORMAT_RGBA32) ? "そのまま転送"
                                                    : "SDL_ConvertPixels を挟む");
    return holder;
  }

  // 新しい画像を「使っていない方」のテクスチャへ書き、current/previous を入れ替える。
  bool present(const ambient::render::RgbaImage& image, std::string& error) {
    if (image.width != width_ || image.height != height_) {
      error = "画像の寸法が土台テクスチャと違います";
      return false;
    }
    const std::size_t expected =
        static_cast<std::size_t>(width_) * static_cast<std::size_t>(height_) * 4U;
    if (image.pixels.size() != expected) {
      error = "画像のバイト数が寸法と一致しません";
      return false;
    }

    const int next_slot = (current_slot_ + 1) % 2;
    SDL_Texture* target = slots_[static_cast<std::size_t>(next_slot)]->raw();
    const int pitch = width_ * 4;

    int updated = 0;
    if (format_ == SDL_PIXELFORMAT_RGBA32) {
      updated = SDL_UpdateTexture(target, nullptr, image.pixels.data(), pitch);
    } else {
      scratch_.resize(expected);
      if (SDL_ConvertPixels(width_, height_, SDL_PIXELFORMAT_RGBA32,
                            image.pixels.data(), pitch, format_, scratch_.data(),
                            pitch) != 0) {
        error = std::string("SDL_ConvertPixels に失敗: ") + SDL_GetError();
        return false;
      }
      updated = SDL_UpdateTexture(target, nullptr, scratch_.data(), pitch);
    }
    if (updated != 0) {
      error = std::string("SDL_UpdateTexture に失敗: ") + SDL_GetError();
      return false;
    }

    previous_slot_ = (shown_count_ > 0) ? current_slot_ : -1;
    current_slot_ = next_slot;
    ++shown_count_;
    return true;
  }

  [[nodiscard]] const ambient::render::Texture* current() const noexcept {
    return (current_slot_ < 0) ? nullptr
                               : slots_[static_cast<std::size_t>(current_slot_)].get();
  }

  // ★Wave 2B のトランジション用（遷移元）。まだ 1 枚しか出していなければ nullptr。
  [[nodiscard]] const ambient::render::Texture* previous() const noexcept {
    return (previous_slot_ < 0) ? nullptr
                                : slots_[static_cast<std::size_t>(previous_slot_)].get();
  }

 private:
  LivePhotoTextures() = default;

  std::array<std::unique_ptr<ambient::render::Texture>, 2> slots_{};
  int current_slot_ = 1;   // 最初の present() で 0 になる
  int previous_slot_ = -1;
  int shown_count_ = 0;
  int width_ = 0;
  int height_ = 0;
  Uint32 format_ = SDL_PIXELFORMAT_UNKNOWN;
  std::vector<std::uint8_t> scratch_;
};

// ---------------------------------------------------------------------------
// PhotoSlideshow — 「次にどれを出すか」だけを持つ配線
// ---------------------------------------------------------------------------
//
// ★ここは巡回（manifest の順に 1 枚ずつ）に留めてある。
//   優先度付きの再生順（ambient/core/photo/scheduler.hpp）を噛ませるのは
//   後続 Wave の仕事。スケジューラは RngState のスナップショット永続化と
//   セットで入るので、写真パイプラインの検証と混ぜない。
//
// ★Wave 2B へ: 写真が切り替わった瞬間は update() の戻り値（PhotoSwitchEvent）で
//   取り出せる。from_key が空なら「前の写真が無い」＝トランジション不要。
//   遷移元・遷移先のテクスチャは LivePhotoTextures::previous() / current()。

struct PhotoSwitchEvent {
  std::string from_key;  // 空 = 初回（遷移元が無い）
  std::string to_key;
  std::size_t to_index = 0;
};

class PhotoSlideshow {
 public:
  PhotoSlideshow(ambient::render::ImageWorker& worker, double interval_ms,
                 double counter_frequency)
      : worker_(&worker),
        interval_ms_(interval_ms),
        counter_frequency_(counter_frequency) {}

  void set_manifest(ambient::core::PhotoManifest manifest) {
    const std::string keep = current_key_;
    manifest_ = std::move(manifest);
    // manifest が更新された＝サーバが応答している。走査のバックオフを解除して
    // すぐ見直す（失敗記録も呼び出し側が clearFailures() で消している）。
    scan_not_before_ = 0;
    // 表示中の写真が新しい manifest にも居るなら、その位置から続ける。
    // 居なければ先頭から。★添字ではなく ID で位置を取り直すのがポイント
    //   （manifest を取り直すと添字は動くが ID は動かない）。
    if (!keep.empty()) {
      if (const auto found = ambient::core::indexOf(manifest_, keep)) {
        next_index_ = (*found + 1) % std::max<std::size_t>(manifest_.items.size(), 1);
        return;
      }
    }
    next_index_ = 0;
  }

  [[nodiscard]] bool has_manifest() const noexcept { return !manifest_.items.empty(); }
  [[nodiscard]] std::size_t manifest_size() const noexcept { return manifest_.items.size(); }

  void skip_now() noexcept {
    force_next_ = true;
    scan_not_before_ = 0;
  }

  // 毎フレーム呼ぶ。切り替わったフレームだけイベントを返す。
  std::optional<PhotoSwitchEvent> update(Uint64 now_counter) {
    if (manifest_.items.empty()) {
      return std::nullopt;
    }
    // ★走査のバックオフ。1 枚も表示できていない状態（= 間隔判定が効かない）でも
    //   毎フレーム全件を舐めないようにするための門。
    if (scan_not_before_ != 0 && now_counter < scan_not_before_) {
      return std::nullopt;
    }
    request_window();

    const bool first = current_key_.empty();
    if (!first && !force_next_) {
      const double elapsed_ms =
          static_cast<double>(now_counter - last_switch_counter_) / counter_frequency_ *
          1000.0;
      if (elapsed_ms < interval_ms_) {
        return std::nullopt;
      }
    }

    // 候補を前から順に見る。Failed / Excluded は飛ばす（★一時除外であって
    // クラッシュではない）。まだ取得中のものが来たらそこで待つ。
    const std::size_t count = manifest_.items.size();
    const std::size_t scan_limit = std::min<std::size_t>(count, 64);
    for (std::size_t attempt = 0; attempt < scan_limit; ++attempt) {
      const std::size_t candidate = (next_index_ + attempt) % count;
      const ambient::core::PhotoItem& item = manifest_.items[candidate];
      const ambient::render::ImageState state = worker_->stateOf(item.id);

      if (state == ambient::render::ImageState::Failed ||
          state == ambient::render::ImageState::Excluded) {
        ++skipped_;
        next_index_ = (candidate + 1) % count;
        continue;
      }
      if (state != ambient::render::ImageState::Ready) {
        return std::nullopt;  // まだ来ていない。次のフレームで見直す。
      }

      std::shared_ptr<const ambient::render::RgbaImage> image = worker_->get(item.id);
      if (image == nullptr) {
        // 追い出された。要求し直して待つ。
        worker_->request(item.id, item.file, /*urgent=*/true);
        return std::nullopt;
      }

      PhotoSwitchEvent event;
      event.from_key = current_key_;
      event.to_key = item.id;
      event.to_index = candidate;

      previous_key_ = current_key_;
      current_key_ = item.id;
      current_index_ = candidate;
      current_image_ = std::move(image);
      next_index_ = (candidate + 1) % count;
      last_switch_counter_ = now_counter;
      force_next_ = false;
      ++shown_;

      // ★現在と直前は追い出さない（再ダウンロードを避けるための最適化）。
      //   安全機構は shared_ptr の方であって、これではない。
      std::vector<std::string> pinned;
      pinned.push_back(current_key_);
      if (!previous_key_.empty()) {
        pinned.push_back(previous_key_);
      }
      worker_->setPinned(std::move(pinned));
      return event;
    }

    // ★走査しきって候補が 1 枚も無かった（全部 Failed / Excluded）。
    //   次のフレームで同じ走査を繰り返しても結果は変わらないので、間隔ぶん
    //   待ってから見直す。これを入れないと、サーバが画像を返せない状態で
    //   毎フレーム 64 件を舐め続ける（実測: 6 秒で 34294 回）。
    last_switch_counter_ = now_counter;
    scan_not_before_ =
        now_counter + static_cast<Uint64>(interval_ms_ / 1000.0 * counter_frequency_);
    force_next_ = false;
    return std::nullopt;
  }

  [[nodiscard]] const ambient::render::RgbaImage* current_image() const noexcept {
    return current_image_.get();
  }

  [[nodiscard]] const ambient::core::PhotoItem* current_item() const noexcept {
    if (current_key_.empty() || current_index_ >= manifest_.items.size()) {
      return nullptr;
    }
    return &manifest_.items[current_index_];
  }

  [[nodiscard]] std::size_t shown() const noexcept { return shown_; }
  [[nodiscard]] std::size_t skipped() const noexcept { return skipped_; }

 private:
  // 今の候補とその先 2 枚を要求する（先読み 2 枚）。
  void request_window() {
    const std::size_t count = manifest_.items.size();
    for (std::size_t offset = 0; offset < 3 && offset < count; ++offset) {
      const ambient::core::PhotoItem& item = manifest_.items[(next_index_ + offset) % count];
      worker_->request(item.id, item.file, /*urgent=*/offset == 0);
    }
  }

  ambient::render::ImageWorker* worker_;
  double interval_ms_;
  double counter_frequency_;

  ambient::core::PhotoManifest manifest_;
  std::size_t next_index_ = 0;
  std::size_t current_index_ = 0;
  std::string current_key_;
  std::string previous_key_;
  std::shared_ptr<const ambient::render::RgbaImage> current_image_;
  Uint64 last_switch_counter_ = 0;
  Uint64 scan_not_before_ = 0;
  bool force_next_ = false;
  std::size_t shown_ = 0;
  std::size_t skipped_ = 0;
};

// 写真のメタデータから PHOTO 画面のキャプションを作る。
// ★ここは「生データ → View Model」の変換であって判断ではない
//   （ambient/render/screens.hpp の冒頭コメント）。
ambient::render::PhotoCaption make_caption(const ambient::core::PhotoItem& item) {
  ambient::render::PhotoCaption caption;
  caption.title = item.title;

  std::string place;
  for (const std::string* part : {&item.pref, &item.city, &item.place}) {
    if (part->empty()) {
      continue;
    }
    if (!place.empty()) {
      place += ' ';
    }
    place += *part;
  }
  caption.place = place;

  if (!item.time.empty()) {
    caption.taken_at = item.time + " 撮影";
  }

  // 右上の時計。★app 層なので実時刻を引いてよい（core は時計を引かない）。
  const std::time_t now = std::time(nullptr);
  std::tm local{};
#if defined(_WIN32)
  localtime_s(&local, &now);
#else
  localtime_r(&now, &local);
#endif
  char clock[8];
  std::snprintf(clock, sizeof(clock), "%02d:%02d", local.tm_hour, local.tm_min);
  caption.clock = clock;

  return caption;
}

// ===========================================================================
// 表示状態の決定（Wave 3B）— claim を集めて core の decide() に決めさせる
// ===========================================================================
//
// ★このファイルには「地震が来たら ALERT を出す」を書かない。
//   それは decide() の責務である（ambient/core/state/arbiter.hpp）。
//   ここがやるのは claim source を **安定した順序で** 並べることだけ。

// 写真の claim source。常に claim を出す（既定のアンビエント表示）。
//
// ★★ 配置についての申し送り ★★
//   docs/CORE_CONTRACTS.md §11 は「各 IClaimSource の実装は 1A（core）」と
//   している。本来 ambient_core に置くべきものだが、Wave 3B は
//   CMakeLists.txt を変更できない（= core に新しい .cpp を足せない）ため、
//   暫定的に app 層へ置いている。core へ移すときは中身をそのまま移せばよい
//   （SDL にも curl にも依存していない）。
class PhotoClaimSource final : public ambient::core::IClaimSource {
 public:
  static constexpr std::uint32_t kSourceId = 0x50'48'4F'54;  // 'PHOT'

  void collect(ambient::core::SteadyTime /*now*/,
               std::vector<ambient::core::Claim>& out) const override {
    ambient::core::Claim claim;
    claim.priority = ambient::core::ClaimPriority::Photo;
    claim.kind = DisplayStateKind::Photo;
    claim.sourceId = kSourceId;
    // ★写真が変わっても payloadId は動かさない。ここで動かすと写真 1 枚ごとに
    //   StateMachine が「別の内容へ遷移した」と報告し、遷移ログが写真の
    //   切り替えログと二重になる（写真の切り替えは slideshow が持っている）。
    claim.payloadId = 1;
    out.push_back(claim);
  }
};

// 壁時計。★app 層なので実時計を引いてよい（core は時計を引かない）。
[[nodiscard]] ambient::core::WallTime wall_now() noexcept {
  return ambient::core::WallTime{static_cast<std::int64_t>(std::time(nullptr))};
}

// ローカルタイムゾーンの UTC オフセット（分）。EnvironmentModel が
// 「取得時刻」を人が読む形にするために要る。
[[nodiscard]] int local_utc_offset_minutes() noexcept {
  const std::time_t now = std::time(nullptr);
  std::tm local{};
#if defined(_WIN32)
  localtime_s(&local, &now);
#else
  localtime_r(&now, &local);
#endif
  return static_cast<int>(local.tm_gmtoff / 60);
}

// ---------------------------------------------------------------------------
// core の結論 → 描画層の View Model
// ---------------------------------------------------------------------------
//
// ★ここは写すだけ。しきい値も判定もここには無い（screens.hpp 冒頭の契約）。

[[nodiscard]] ambient::render::EnvLevel to_render_level(ambient::core::EnvLevel level) noexcept {
  switch (level) {
    case ambient::core::EnvLevel::Normal:
      return ambient::render::EnvLevel::Normal;
    case ambient::core::EnvLevel::Caution:
      return ambient::render::EnvLevel::Caution;
    case ambient::core::EnvLevel::Warning:
      return ambient::render::EnvLevel::Warning;
  }
  return ambient::render::EnvLevel::Normal;
}

[[nodiscard]] ambient::render::EnvScreen make_env_screen(const ambient::core::EnvView& view) {
  ambient::render::EnvScreen screen;
  screen.heading = view.heading;
  screen.hint = view.hint;
  screen.metrics.reserve(view.metrics.size());
  for (const ambient::core::EnvMetricView& metric : view.metrics) {
    ambient::render::EnvMetric out;
    out.label = metric.label;
    out.value = metric.value;
    out.unit = metric.unit;
    out.level = to_render_level(metric.level);
    out.stale = metric.stale;
    screen.metrics.push_back(std::move(out));
  }
  screen.weather_area = view.weatherArea;
  screen.weather_text = view.weatherText;
  screen.weather_temps = view.weatherTemps;
  screen.weather_pop = view.weatherPop;
  screen.weekly.reserve(view.weekly.size());
  for (const ambient::core::EnvWeeklyView& day : view.weekly) {
    ambient::render::EnvWeeklyDay out;
    out.date = day.date;
    out.temp_max = day.tempMax;
    out.temp_min = day.tempMin;
    screen.weekly.push_back(std::move(out));
  }
  return screen;
}

[[nodiscard]] ambient::render::AlertScreen make_alert_screen(
    const ambient::core::QuakeItem& quake) {
  ambient::render::AlertScreen screen;
  screen.title = "地震速報";
  // ★震度は **生の文字列のまま** 渡す。"5-" / "5+" を整数化すると
  //   弱強の別が静かに消える（screens.hpp 冒頭 / constants.hpp:84-85）。
  screen.intensity = quake.maxi;
  screen.magnitude = quake.mag.empty() ? std::string{} : ("M" + quake.mag);
  screen.place = quake.place;

  // ★同定にも表示にも at（ISO 8601）を使う。time は "HH:MM" で日付が落ちている。
  if (const std::optional<ambient::core::CivilTime> civil =
          ambient::core::parseIso8601Fixed(quake.at)) {
    char buffer[64];
    // ★整数の書式なのでロケールに依存しない（%f は使っていない）。
    std::snprintf(buffer, sizeof(buffer), "%04d-%02d-%02d %02d:%02d 発生", civil->year,
                  civil->month, civil->day, civil->hour, civil->minute);
    screen.occurred = buffer;
  } else if (!quake.time.empty()) {
    screen.occurred = quake.time + " 発生";
  }
  screen.note = "落ち着いて身の安全を確保してください";
  return screen;
}

// ---------------------------------------------------------------------------
// 合成地震の注入（--inject-quake）
// ---------------------------------------------------------------------------
//
// 通常応答では高震度が保証されないため、専用fixtureでアラート経路を通す。
//   assets/fixtures/quakes-inject.json は /api/env と完全に同じ形なので、
//   **実応答と同じパーサ**（transport の parseEnvJson）を通せる。
//   ここで自前の JSON パースを書くと、その経路だけ実通信と別物になる。
class QuakeInjector {
 public:
  [[nodiscard]] bool load(const fs::path& path, std::string& error) {
    std::ifstream file(path, std::ios::binary);
    if (!file) {
      error = "注入用の地震 fixture を開けません: " + path.string();
      return false;
    }
    std::ostringstream buffer;
    buffer << file.rdbuf();
    const std::string body = buffer.str();

    ambient::core::EnvSnapshot snapshot;
    if (!ambient::transport::parseEnvJson(body, snapshot)) {
      error = "注入用の地震 fixture を /api/env として読めません: " + path.string();
      return false;
    }
    quakes_ = std::move(snapshot.quakes);
    if (quakes_.empty()) {
      error = "注入用の地震 fixture に quakes がありません: " + path.string();
      return false;
    }
    return true;
  }

  [[nodiscard]] std::size_t size() const noexcept { return quakes_.size(); }
  [[nodiscard]] const std::vector<ambient::core::QuakeItem>& all() const noexcept {
    return quakes_;
  }

  // 1 件だけ取り出す（Q キー / --inject-index N 用）。
  [[nodiscard]] const ambient::core::QuakeItem* at(std::size_t index) const noexcept {
    return (index < quakes_.size()) ? &quakes_[index] : nullptr;
  }

  [[nodiscard]] std::size_t nextIndex() noexcept {
    const std::size_t index = cursor_;
    cursor_ = (cursor_ + 1) % quakes_.size();
    return index;
  }

 private:
  std::vector<ambient::core::QuakeItem> quakes_;
  std::size_t cursor_ = 0;
};

// ===========================================================================
// トランジション（Wave 2B）
// ===========================================================================
//
// 描画の中身は ambient/render/transitions.hpp。ここは配線だけ。
//
// ★演出の抽選も duration も core が決めている。このファイルには
//   「どの演出をいつ出すか」を書かない（書いた瞬間に core の契約が二重化する）。

// ---------------------------------------------------------------------------
// FrameLimiter — ★Tab5 の低 fps を Mac 上で再現するための仕掛け
// ---------------------------------------------------------------------------
//
// 進捗は経過時間から出ている（Progress::fromElapsed）。したがって fps を
// 落としても **演出の長さもカーブも変わらず、サンプルが疎になるだけ** である。
// この性質が本当に効いているかを目視で確かめられる、というのがこの機能の主目的。
//
// vsync が有効なので、上限を 60 より上に指定しても効果は無い（下げるためのもの）。
class FrameLimiter {
 public:
  FrameLimiter(double fps_cap, double counter_frequency) noexcept
      : frequency_(counter_frequency) {
    if (fps_cap > 0.0 && counter_frequency > 0.0) {
      period_ = static_cast<Uint64>(counter_frequency / fps_cap);
      next_ = SDL_GetPerformanceCounter() + period_;
    }
  }

  [[nodiscard]] bool enabled() const noexcept { return period_ != 0; }

  // 次のフレーム境界まで待つ。上限が無ければ即座に戻る。
  void wait() noexcept {
    if (period_ == 0) {
      return;
    }
    for (;;) {
      const Uint64 now = SDL_GetPerformanceCounter();
      if (now >= next_) {
        break;
      }
      const double remain_ms =
          static_cast<double>(next_ - now) / frequency_ * 1000.0;
      // 1ms 以上あるうちは寝る。残りは回して待つ（SDL_Delay の粒度は粗い）。
      if (remain_ms > 1.5) {
        SDL_Delay(static_cast<Uint32>(remain_ms - 1.0));
      }
    }
    next_ += period_;
    // 一度でも大きく遅れたら基準を引き直す（借金を貯めない）。
    const Uint64 now = SDL_GetPerformanceCounter();
    if (next_ < now) {
      next_ = now + period_;
    }
  }

 private:
  double frequency_ = 0.0;
  Uint64 period_ = 0;
  Uint64 next_ = 0;
};

// SDL の単調カウンタを core の SteadyTime にする。
// ★app 層なので実時計を引いてよい。core は時計を引かない。
[[nodiscard]] ambient::core::SteadyTime steady_now() noexcept {
  return ambient::core::SteadyTime{static_cast<std::int64_t>(SDL_GetTicks64())};
}

// 演出のパラメータを人間が読める形にする（ログとデモの記録用）。
std::string describe_params(ambient::core::TransitionKind kind,
                            const ambient::core::TransitionParams& params) {
  char buffer[96];
  switch (kind) {
    case ambient::core::TransitionKind::Slide:
      std::snprintf(buffer, sizeof(buffer), "direction=%s",
                    ambient::core::toString(params.slide.direction));
      return buffer;
    case ambient::core::TransitionKind::Wipe:
      std::snprintf(buffer, sizeof(buffer), "angle=%u", params.wipe.angleDeg);
      return buffer;
    case ambient::core::TransitionKind::Blinds:
      std::snprintf(buffer, sizeof(buffer), "slats=%u vertical=%s",
                    static_cast<unsigned>(params.blinds.slats),
                    params.blinds.vertical ? "true" : "false");
      return buffer;
    case ambient::core::TransitionKind::CrossFade:
    case ambient::core::TransitionKind::FadeThroughBlack:
      return "(パラメータ無し)";
  }
  return "?";
}

// 1 フレームあたりの進捗差が、その演出でどれだけの「動き」になるかを言葉にする。
// ★これが低 fps 評価の中心。progress の差だけでは演出ごとの体感差が見えない。
std::string describe_step(ambient::core::TransitionKind kind,
                          const ambient::core::TransitionParams& params, double step) {
  char buffer[128];
  switch (kind) {
    case ambient::core::TransitionKind::Slide: {
      const bool horizontal =
          params.slide.direction == ambient::core::SlideDirection::FromLeft ||
          params.slide.direction == ambient::core::SlideDirection::FromRight;
      const double span = horizontal ? kLogicalWidth : kLogicalHeight;
      std::snprintf(buffer, sizeof(buffer), "画面が 1 フレームで %.0f px 動く", step * span);
      return buffer;
    }
    case ambient::core::TransitionKind::Wipe: {
      const bool horizontal = params.wipe.angleDeg == 0 || params.wipe.angleDeg == 180;
      const double span = horizontal ? kLogicalWidth : kLogicalHeight;
      std::snprintf(buffer, sizeof(buffer), "境界が 1 フレームで %.0f px 進む", step * span);
      return buffer;
    }
    case ambient::core::TransitionKind::Blinds: {
      const double span = (params.blinds.vertical ? kLogicalWidth : kLogicalHeight) /
                          static_cast<double>(std::max<unsigned>(params.blinds.slats, 1U));
      std::snprintf(buffer, sizeof(buffer),
                    "スリット 1 枚あたり 1 フレームで %.0f px 開く（%u 枚）", step * span,
                    static_cast<unsigned>(params.blinds.slats));
      return buffer;
    }
    case ambient::core::TransitionKind::CrossFade:
      std::snprintf(buffer, sizeof(buffer), "alpha が 1 フレームで %.0f/255 変わる",
                    step * 255.0);
      return buffer;
    case ambient::core::TransitionKind::FadeThroughBlack:
      std::snprintf(buffer, sizeof(buffer),
                    "黒の alpha が 1 フレームで %.0f/255 変わる（前後半それぞれ）",
                    step * 2.0 * 255.0);
      return buffer;
  }
  return "";
}

// --- デモ ------------------------------------------------------------------

struct DemoChoice {
  const char* name;
  const char* file_stem;
  // true = KenBurns（★滞在効果であって遷移ではない。kind は使わない）
  bool dwell;
  ambient::core::TransitionKind kind;
};

constexpr std::array<DemoChoice, 6> kDemoChoices{{
    {"crossfade", "1-crossfade", false, ambient::core::TransitionKind::CrossFade},
    {"fade_through_black", "2-fade-through-black", false,
     ambient::core::TransitionKind::FadeThroughBlack},
    {"slide", "3-slide", false, ambient::core::TransitionKind::Slide},
    {"wipe", "4-wipe", false, ambient::core::TransitionKind::Wipe},
    {"blinds", "5-blinds", false, ambient::core::TransitionKind::Blinds},
    {"kenburns", "6-kenburns", true, ambient::core::TransitionKind::CrossFade},
}};

// 進捗のどこでスクリーンショットを撮るか。
// ★0 と 1 では演出が見えないので **途中だけ** を撮る。
constexpr std::array<double, 10> kDemoShotProgress{0.05, 0.15, 0.25, 0.35, 0.45,
                                                   0.55, 0.65, 0.75, 0.85, 0.95};

struct DemoStats {
  std::string name;
  std::string params_text;
  long frames = 0;
  double elapsed_seconds = 0.0;
  double achieved_fps = 0.0;
  double max_progress_step = 0.0;
  std::string step_text;
};

// デモ用に「画面 1 枚ぶん」を合成する。
// ★遷移させるのは写真ではなく **完成したフレーム** である。そうしないと
//   遷移中だけキャプションが消えたり、前の写真の文字が残ったりする。
void compose_demo_frame(SDL_Renderer* renderer, ambient::render::Canvas& canvas,
                        const ambient::render::ScreenFonts& fonts,
                        const ambient::render::Texture* photo,
                        ambient::render::ScreenData data, const char* label,
                        SDL_Color wash) {
  const ambient::render::Canvas::Scope scope(renderer, canvas);
  if (!scope.ok()) {
    return;
  }
  SDL_SetRenderDrawBlendMode(renderer, SDL_BLENDMODE_NONE);
  SDL_SetRenderDrawColor(renderer, 0, 0, 0, 255);
  SDL_RenderClear(renderer);

  data.photo.title = std::string("写真 ") + label;
  data.photo.place = std::string("トランジション見本 ") + label;
  const ambient::render::ScreenLayout layout{kLogicalWidth, kLogicalHeight};
  ambient::render::draw_screen(renderer, DisplayStateKind::Photo, fonts, photo, data, layout);

  // 2 枚を一目で見分けられるようにする（境界の形を判定するため）。
  SDL_SetRenderDrawBlendMode(renderer, SDL_BLENDMODE_BLEND);
  SDL_SetRenderDrawColor(renderer, wash.r, wash.g, wash.b, wash.a);
  const SDL_Rect full{0, 0, kLogicalWidth, kLogicalHeight};
  SDL_RenderFillRect(renderer, &full);

  ambient::render::TextStyle big;
  big.color = SDL_Color{255, 255, 255, 255};
  big.drop_shadow = true;
  big.shadow_offset = 6;
  ambient::render::draw_text(renderer, *fonts.hero, label, 590, 250, big);
}

// 演出を 1 つ、実時間で最後まで再生する。
// 戻り値 false = 途中で中断された（ESC / ウィンドウを閉じた）。
bool run_demo_effect(const DemoChoice& choice, ambient::render::Display& display,
                     ambient::render::Canvas* canvas_a, ambient::render::Canvas* canvas_b,
                     const ambient::render::Texture& photo, ambient::core::TransitionRng& rng,
                     const Options& options, double counter_frequency, DemoStats& stats) {
  SDL_Renderer* renderer = display.renderer();

  // ★パラメータは core に決めさせる（同じ seed なら fps を変えても同じ絵になる）。
  const ambient::core::TransitionParams params =
      ambient::core::deriveTransitionParams(choice.kind, rng);
  ambient::core::TransitionRng dwell_rng{options.transition_seed};
  const ambient::core::KenBurnsParams ken_burns = ambient::core::deriveKenBurns(dwell_rng);

  const ambient::core::Millis duration =
      choice.dwell ? ambient::core::kDwellDurationDefault
                   : ambient::core::durationOf(choice.kind);

  stats.name = choice.name;
  stats.params_text =
      choice.dwell
          ? std::string("start/end 千分率 ") + std::to_string(ken_burns.start.wMilli) + " -> " +
                std::to_string(ken_burns.end.wMilli)
          : describe_params(choice.kind, params);

  std::fprintf(stderr, "[demo] %s: duration=%lld ms / %s\n", choice.name,
               static_cast<long long>(duration.value), stats.params_text.c_str());

  ambient::core::TransitionPlan plan;
  plan.kind = choice.kind;
  plan.duration = duration;

  FrameLimiter limiter(options.transition_fps_cap, counter_frequency);
  const Uint64 start_counter = SDL_GetPerformanceCounter();
  std::size_t shot_index = 0;
  float previous_progress = 0.0F;
  bool aborted = false;
  std::string error;

  for (;;) {
    SDL_Event event;
    while (SDL_PollEvent(&event) != 0) {
      if (event.type == SDL_QUIT ||
          (event.type == SDL_KEYDOWN && event.key.keysym.sym == SDLK_ESCAPE)) {
        aborted = true;
      }
    }
    if (aborted) {
      break;
    }

    const Uint64 now = SDL_GetPerformanceCounter();
    const double elapsed_ms =
        static_cast<double>(now - start_counter) / counter_frequency * 1000.0;
    // ★経過時間から進捗を作る。フレーム番号は登場しない。
    const ambient::core::Progress progress = ambient::core::Progress::fromElapsed(
        ambient::core::Millis{static_cast<std::int64_t>(elapsed_ms)}, duration);

    if (choice.dwell) {
      ambient::render::draw_ken_burns(renderer, ken_burns, progress, photo.raw(),
                                      kLogicalWidth, kLogicalHeight);
    } else {
      ambient::render::draw_transition(
          renderer, plan, params, progress, canvas_a != nullptr ? canvas_a->raw() : nullptr,
          canvas_b != nullptr ? canvas_b->raw() : nullptr, kLogicalWidth, kLogicalHeight);
    }

    ++stats.frames;
    if (stats.frames > 1) {
      stats.max_progress_step = std::max(
          stats.max_progress_step,
          static_cast<double>(progress.value()) - static_cast<double>(previous_progress));
    }
    previous_progress = progress.value();

    // 途中経過の保存。★present の前に読むこと（present 後は未定義になる）。
    if (!options.screenshot_path.empty() && shot_index < kDemoShotProgress.size() &&
        static_cast<double>(progress.value()) >= kDemoShotProgress[shot_index]) {
      char name[256];
      std::snprintf(name, sizeof(name), "%s/%s-%02zu-p%03d.png",
                    options.screenshot_path.c_str(), choice.file_stem, shot_index,
                    static_cast<int>(static_cast<double>(progress.value()) * 100.0 + 0.5));
      if (!display.save_screenshot(name, error)) {
        std::fprintf(stderr, "[demo] スクリーンショットに失敗: %s\n", error.c_str());
      }
      // 1 フレームで複数のしきい値をまたいだら、同じ絵を重複保存しない。
      while (shot_index < kDemoShotProgress.size() &&
             static_cast<double>(progress.value()) >= kDemoShotProgress[shot_index]) {
        ++shot_index;
      }
    }

    SDL_RenderPresent(renderer);
    limiter.wait();

    if (progress.done()) {
      break;
    }
  }

  const Uint64 end_counter = SDL_GetPerformanceCounter();
  stats.elapsed_seconds =
      static_cast<double>(end_counter - start_counter) / counter_frequency;
  stats.achieved_fps = (stats.elapsed_seconds > 0.0)
                           ? static_cast<double>(stats.frames) / stats.elapsed_seconds
                           : 0.0;
  stats.step_text = choice.dwell
                        ? std::string("切り出しが 1 フレームで ") +
                              std::to_string(static_cast<int>(
                                  stats.max_progress_step *
                                  std::abs(static_cast<double>(ken_burns.end.wMilli) -
                                           static_cast<double>(ken_burns.start.wMilli)))) +
                              " 千分率ぶん変わる"
                        : describe_step(choice.kind, params, stats.max_progress_step);
  return !aborted;
}

// SDL のリソース（Display / Font / Texture）を持つ処理は、すべてこの関数に閉じる。
//
// ★★★ この構造には理由がある。早期 return を足す前に必ず読むこと ★★★
//
// Display / Font / Texture のデストラクタは SDL_DestroyRenderer /
// SDL_DestroyWindow / TTF_CloseFont を呼ぶ。これらは shutdown_runtime()
// （TTF_Quit → IMG_Quit → SDL_Quit）より **前** に走らなければならない。
// SDL_Quit() は全ウィンドウとレンダラを既に破棄しているので、その後に
// デストラクタが走ると解放済みメモリを触る。
//
// 以前はこの処理が main() に直書きされ、各エラー経路が「リソースを持ったまま
// shutdown_runtime() を呼んで return」していた。正常終了パスだけが明示的な
// reset() で正しい順序を守っていて、エラー経路はその規律から外れていた
// （実測: --photo に画像でないファイルを渡すと SIGSEGV で落ちていた）。
//
// 別関数に切り出すと、**この関数を抜けた時点で全デストラクタが走り終わっている**。
// したがって新しいエラー経路をこの関数のどこに足しても順序は自動的に正しくなり、
// 経路ごとに reset() を並べる必要がない。
//
// 守ること:
//   * この関数の中で shutdown_runtime() を呼ばない（呼ぶのは main() だけ）
//   * SDL リソースの寿命を main() 側へ引き上げない
int run(const Options& options) {
  std::string error;

  ambient::render::DisplayConfig display_config;
  display_config.logical_width = kLogicalWidth;
  display_config.logical_height = kLogicalHeight;
  display_config.window_width = options.window_width;
  display_config.window_height = options.window_height;
  display_config.title = "GeoFrame Ambient Display";

  // ★宣言順がそのまま破棄順（の逆順）になる。並べ替えないこと。
  //   display を **最初に** 宣言する = **最後に** 破棄される。
  //   テクスチャとフォントはレンダラより先に解放しなければならない
  //   （SDL_DestroyRenderer 後の SDL_DestroyTexture は解放済みメモリを触る）。
  std::unique_ptr<ambient::render::Display> display =
      ambient::render::Display::create(display_config, error);
  if (display == nullptr) {
    std::fprintf(stderr, "[app] ★エラー★ %s\n", error.c_str());
    return EXIT_FAILURE;
  }

  log_input_mapping_selfcheck(*display);

  // SDL_ttf は「開いたときの point size」でしか描けないので、必要な字の
  // 大きさぶんだけ Font を開く。数メートル離れて読めることが要件なので、
  // 本文以上は思い切って大きくしてある（HUD だけが小さい）。
  ambient::render::FontSpec base_spec;
  base_spec.path = options.font_path.string();
  base_spec.face_index = options.font_face_index;
  // fetch-fonts.sh は静的 Medium インスタンス（fvar 無し、可変軸の問題が
  // そもそも存在しない実体）を取得するので、合成ボールドは不要。
  // 合成ボールドは字形が崩れる副作用があるため使わない。
  base_spec.synthetic_bold = false;

  // ★宣言順の注意（display より後に置くこと）は下の photo と同じ。
  //   複数フォントを1つずつ開くので、失敗経路も1つずつ早期 return する。
  //   run() を抜けた時点で全部のデストラクタが走るので順序は自動的に正しい。
  struct FontSlot {
    int point_size;
    std::unique_ptr<ambient::render::Font> font;
  };
  std::array<FontSlot, 7> font_slots{{
      {150, nullptr},  // hero   : 震度
      {108, nullptr},  // value  : 室温・CO2 など
      {56, nullptr},   // title  : 見出し・写真タイトル
      {40, nullptr},   // label  : カードのラベル
      {30, nullptr},   // body   : 本文
      {26, nullptr},   // caption: 補足
      {20, nullptr},   // hud    : 開発用 HUD（画面要素ではない）
  }};
  for (FontSlot& slot : font_slots) {
    ambient::render::FontSpec spec = base_spec;
    spec.point_size = slot.point_size;
    slot.font = ambient::render::Font::open(spec, error);
    if (slot.font == nullptr) {
      std::fprintf(stderr, "[app] ★エラー★ %s\n", error.c_str());
      return EXIT_FAILURE;
    }
  }

  ambient::render::ScreenFonts screen_fonts;
  screen_fonts.hero = font_slots[0].font.get();
  screen_fonts.value = font_slots[1].font.get();
  screen_fonts.title = font_slots[2].font.get();
  screen_fonts.label = font_slots[3].font.get();
  screen_fonts.body = font_slots[4].font.get();
  screen_fonts.caption = font_slots[5].font.get();
  const ambient::render::Font& hud_font = *font_slots[6].font;

  auto photo = ambient::render::Texture::load(display->renderer(),
                                              options.photo_path.string(), error);
  if (photo == nullptr) {
    std::fprintf(stderr, "[app] ★エラー★ %s\n", error.c_str());
    return EXIT_FAILURE;
  }

  {
    const SDL_Rect crop = ambient::render::cover_source_rect(
        photo->width(), photo->height(), kLogicalWidth, kLogicalHeight);
    std::fprintf(stderr,
                 "[app] cover クロップ: src=(%d,%d,%d,%d) of %dx%d -> %dx%d\n",
                 crop.x, crop.y, crop.w, crop.h, photo->width(), photo->height(),
                 kLogicalWidth, kLogicalHeight);
  }

  ambient::render::ScreenData screen_data =
      make_sample_screen_data(options.care_item_count);
  const ambient::render::ScreenLayout layout{kLogicalWidth, kLogicalHeight};
  DisplayStateKind current_screen = options.screen;

  const bool screenshot_mode = !options.screenshot_path.empty();
  const bool run_seconds_mode = options.run_seconds > 0.0;
  int exit_code = EXIT_SUCCESS;

  // -------------------------------------------------------------------------
  // --screen all: 5画面を連続してスクリーンショットへ落として終了する。
  // Phase 0 の完了条件（ネットワーク完全遮断で全画面を表示し保存できること）
  // がこの経路。通常のループとは要求が違うので混ぜず、独立した経路にしてある。
  // -------------------------------------------------------------------------
  if (options.screen_all) {
    for (const ScreenChoice& choice : kScreenChoices) {
      const std::string path =
          options.screenshot_path + "/" + choice.file_stem + ".png";
      for (int frame = 0; frame < kBatchWarmupFrames; ++frame) {
        // ★イベントを汲むこと。macOS ではウィンドウがマップされるまで
        //   数フレームかかり、汲まないと読み出しが黒画像になることがある。
        SDL_Event event;
        while (SDL_PollEvent(&event) != 0) {
          // 取得中はユーザ操作を受け付けない（撮り漏らしを防ぐ）。
        }
        render_frame(*display, screen_fonts, hud_font, photo.get(), choice.kind,
                     screen_data, 0.0, ambient::render::LogicalPoint{}, false,
                     /*show_hud=*/false);
        if (frame + 1 == kBatchWarmupFrames) {
          // ★ SDL_RenderPresent の前に読むこと（present 後は黒になる）。
          if (!display->save_screenshot(path, error)) {
            std::fprintf(stderr, "[app] ★エラー★ %s\n", error.c_str());
            exit_code = EXIT_FAILURE;
          }
        }
        SDL_RenderPresent(display->renderer());
      }
    }
    std::fprintf(stderr, "[app] --screen all: %zu 画面を %s へ保存しました。\n",
                 kScreenChoices.size(), options.screenshot_path.c_str());
    return exit_code;
  }

  // SDL の単調カウンタの周波数。FPS 計測にもフレーム上限にも使う。
  const double counter_frequency = static_cast<double>(SDL_GetPerformanceFrequency());

  // -------------------------------------------------------------------------
  // 描画側の能力申告（Wave 2B）
  //
  // ★これは「どれくらいの頻度で出したいか」（TransitionWeights）ではなく
  //   「そもそも描けるか」（プラットフォームの事実）。由来が違うので分けてある
  //   （ambient/core/transition.hpp の RenderCaps の節）。
  // -------------------------------------------------------------------------
  ambient::render::RenderCapsReport caps_report =
      ambient::render::probe_render_caps(display->renderer());
  std::fprintf(stderr, "[app] RenderCaps: blend=%s 判定=%s（%s）\n",
               caps_report.caps.blend ? "true" : "false",
               ambient::render::toString(caps_report.method), caps_report.note.c_str());
  if (caps_report.method == ambient::render::CapsProbeMethod::Measured) {
    std::fprintf(stderr,
                 "[app]   実測値: 描画色 R=%d B=%d / テクスチャ alpha R=%d G=%d"
                 "（どちらも中間色なら blend が効いている）\n",
                 caps_report.drawMixR, caps_report.drawMixB, caps_report.texMixR,
                 caps_report.texMixG);
  }
  if (options.force_no_blend) {
    caps_report.caps.blend = false;
    std::fprintf(stderr,
                 "[app] --no-blend: blend 群を抽選から外します"
                 "（Tab5 で blend が使えなかった場合の予行演習）。\n");
  }
  {
    const ambient::core::TransitionWeights allowed = ambient::core::allowedWeights(
        ambient::core::kDefaultTransitionWeights, caps_report.caps);
    std::fprintf(stderr, "[app] 抽選に残る演出: ");
    for (std::size_t i = 0; i < ambient::core::kTransitionKindCount; ++i) {
      if (allowed.weight[i] > 0) {
        std::fprintf(stderr, "%s ", ambient::core::toString(ambient::core::kAllTransitionKinds[i]));
      }
    }
    std::fprintf(stderr, "/ フォールバック=%s / 滞在効果=%s\n",
                 ambient::core::toString(ambient::core::fallbackTransition(caps_report.caps)),
                 caps_report.caps.allows(ambient::core::DwellEffect::KenBurns) ? "KenBurns 可"
                                                                              : "None のみ");
  }

  // -------------------------------------------------------------------------
  // --transition-demo: 演出の見本を順に再生して終了する。
  //
  // ★完了条件の「6 演出のスクリーンショット」と「15fps での比較評価」は
  //   この経路で採る。実写真の巡回に混ぜると、どの演出が当たるかが乱数任せで
  //   比較にならない。
  // -------------------------------------------------------------------------
  if (!options.transition_demo.empty()) {
    std::vector<DemoChoice> demo_list;
    if (options.transition_demo == "all") {
      demo_list.assign(kDemoChoices.begin(), kDemoChoices.end());
    } else {
      for (const DemoChoice& choice : kDemoChoices) {
        if (options.transition_demo == choice.name) {
          demo_list.push_back(choice);
        }
      }
      if (demo_list.empty()) {
        std::fprintf(stderr,
                     "[app] --transition-demo の値が不正です: %s\n"
                     "      crossfade / fade_through_black / slide / wipe / blinds /"
                     " kenburns / all のいずれかを指定してください。\n",
                     options.transition_demo.c_str());
        return EXIT_FAILURE;
      }
    }

    std::string canvas_error;
    std::unique_ptr<ambient::render::Canvas> canvas_a = ambient::render::Canvas::create(
        display->renderer(), kLogicalWidth, kLogicalHeight, canvas_error);
    std::unique_ptr<ambient::render::Canvas> canvas_b = ambient::render::Canvas::create(
        display->renderer(), kLogicalWidth, kLogicalHeight, canvas_error);
    if (canvas_a == nullptr || canvas_b == nullptr) {
      std::fprintf(stderr, "[app] ★エラー★ デモ用キャンバスを用意できません: %s\n",
                   canvas_error.c_str());
      return EXIT_FAILURE;
    }
    compose_demo_frame(display->renderer(), *canvas_a, screen_fonts, photo.get(), screen_data,
                       "A", SDL_Color{20, 60, 200, 110});
    compose_demo_frame(display->renderer(), *canvas_b, screen_fonts, photo.get(), screen_data,
                       "B", SDL_Color{220, 110, 20, 110});

    std::fprintf(stderr,
                 "\n[demo] ===== トランジション見本 =====\n"
                 "[demo] fps 上限: %s / seed: %llu\n",
                 options.transition_fps_cap > 0.0
                     ? std::to_string(static_cast<int>(options.transition_fps_cap)).c_str()
                     : "無制限(vsync)",
                 static_cast<unsigned long long>(options.transition_seed));

    std::vector<DemoStats> all_stats;
    for (const DemoChoice& choice : demo_list) {
      // ★演出ごとに seed を初期化する。--transition-demo slide だけを流しても
      //   all を流したときと同じパラメータになる（比較のため）。
      ambient::core::TransitionRng rng{options.transition_seed};
      DemoStats stats;
      const bool completed =
          run_demo_effect(choice, *display, canvas_a.get(), canvas_b.get(), *photo, rng,
                          options, counter_frequency, stats);
      all_stats.push_back(stats);
      if (!completed) {
        std::fprintf(stderr, "[demo] 中断されました。\n");
        break;
      }
    }

    std::fprintf(stderr, "\n[demo] %-20s %6s %8s %10s  %s\n", "演出", "frames", "fps",
                 "最大Δ進捗", "1 フレームあたりの動き");
    for (const DemoStats& stats : all_stats) {
      std::fprintf(stderr, "[demo] %-20s %6ld %8.1f %10.3f  %s\n", stats.name.c_str(),
                   stats.frames, stats.achieved_fps, stats.max_progress_step,
                   stats.step_text.c_str());
    }
    std::fprintf(stderr, "[demo] ==============================\n\n");
    return exit_code;
  }

  // -------------------------------------------------------------------------
  // 写真の巡回表示（--photo-source が none 以外のとき）
  //
  // ★宣言位置に理由がある。display / font / photo より **後** に置くこと。
  //   run() を抜けるとき逆順に破棄されるので、ここで作ったスレッド
  //   （ManifestPoller / ImageWorker）は SDL のリソースより先に join される。
  // -------------------------------------------------------------------------
  std::unique_ptr<ambient::core::IHttpClient> image_http;
  std::unique_ptr<ambient::render::ImageWorker> image_worker;
  std::unique_ptr<ManifestPoller> manifest_poller;
  std::unique_ptr<LivePhotoTextures> live_photo;
  std::unique_ptr<PhotoSlideshow> slideshow;
  bool photos_degraded = false;

  if (options.photo_source != PhotoSource::None) {
    ambient::transport::GeoFrameConfig geo_config;
    geo_config.baseUrl = options.server_url;

    // ★HTTP クライアントは **スレッドごとに別インスタンス**。
    //   libcurl の easy handle はスレッド間で共有できない。
    std::unique_ptr<ambient::core::IHttpClient> api_http;
    if (options.photo_source == PhotoSource::Fixture) {
      const std::string fixture_dir = (options.asset_dir / "fixtures").string();
      api_http = std::make_unique<ambient::transport::FixtureHttpClient>(fixture_dir);
      image_http = std::make_unique<ambient::transport::FixtureHttpClient>(fixture_dir);
      std::fprintf(stderr,
                   "[app] --photo-source fixture: %s を IHttpClient 実装として使います"
                   "（実通信と同一のクライアントコードを通ります）。\n",
                   fixture_dir.c_str());
      std::fprintf(stderr,
                   "[app]   ★derivatives/*.jpg に対応する fixture は存在しないため、"
                   "画像取得は 404（HTML）になります。これは意図した経路です。\n");
    } else {
      ambient::transport::CurlHttpConfig curl_config;
      std::string curl_error;
      api_http = ambient::transport::CurlHttpClient::create(curl_config, curl_error);
      if (api_http != nullptr) {
        image_http = ambient::transport::CurlHttpClient::create(curl_config, curl_error);
      }
      if (api_http == nullptr || image_http == nullptr) {
        std::fprintf(stderr, "[app] ★エラー★ %s\n", curl_error.c_str());
        return EXIT_FAILURE;
      }
      std::fprintf(stderr, "[app] --photo-source network: %s へ接続します（GET のみ）。\n",
                   geo_config.baseUrl.c_str());
    }

    ambient::render::ImageWorkerConfig worker_config;
    worker_config.baseUrl = geo_config.baseUrl;
    worker_config.targetWidth = kLogicalWidth;
    worker_config.targetHeight = kLogicalHeight;
    if (options.decode_budget_mb > 0.0) {
      worker_config.decodeBudget.maxDecodedBytes =
          static_cast<std::uint64_t>(options.decode_budget_mb * 1024.0 * 1024.0);
    }
    std::fprintf(stderr,
                 "[app] デコード予算: %.1f MB / キャッシュ上限 %zu 枚"
                 "（1280x720 RGBA = 3.5MB/枚）\n",
                 static_cast<double>(worker_config.decodeBudget.maxDecodedBytes) /
                     (1024.0 * 1024.0),
                 worker_config.cacheCapacity);
    image_worker =
        std::make_unique<ambient::render::ImageWorker>(*image_http, worker_config);

    std::string live_error;
    live_photo = LivePhotoTextures::create(display->renderer(), kLogicalWidth,
                                           kLogicalHeight, live_error);
    if (live_photo == nullptr) {
      // ★表示上の便宜であって致命ではない。fixture の静止画へ縮退する。
      std::fprintf(stderr,
                   "[app] 写真テクスチャを用意できませんでした（静止画へ縮退）: %s\n",
                   live_error.c_str());
    }

    slideshow = std::make_unique<PhotoSlideshow>(*image_worker, options.photo_interval_ms,
                                                 counter_frequency);
    manifest_poller = std::make_unique<ManifestPoller>(
        std::move(api_http), geo_config, options.manifest_interval_ms);
  }

  // -------------------------------------------------------------------------
  // 環境情報と地震アラート（Wave 3B）
  //
  // ★ここから先、表示状態は **core の decide() が決める**。
  //   このファイルには「地震が来たら ALERT」も「環境カットの周期」も書かない。
  //   前者は decide()、後者は EnvironmentModel が持っている。
  // -------------------------------------------------------------------------
  QuakeInjector injector;
  const bool inject_enabled = !options.inject_quake_path.empty();
  if (inject_enabled) {
    std::string inject_error;
    if (!injector.load(options.inject_quake_path, inject_error)) {
      std::fprintf(stderr, "[app] ★エラー★ %s\n", inject_error.c_str());
      return EXIT_FAILURE;
    }
    std::fprintf(stderr,
                 "[app] --inject-quake: %s から %zu 件の合成地震を読み込みました。\n",
                 options.inject_quake_path.c_str(), injector.size());
    if (!options.inject_selection.all &&
        injector.at(options.inject_selection.index) == nullptr) {
      std::fprintf(stderr,
                   "[app] ★エラー★ --inject-index %zu は範囲外です（0〜%zu）。\n",
                   options.inject_selection.index, injector.size() - 1);
      return EXIT_FAILURE;
    }
  }

  // ★表示状態をアービターに任せるかどうか。
  //   既存の --screen / 数字キーによる手動選択を壊さないため、
  //   環境か地震のどちらかを配線したときだけアービター駆動にする。
  const bool arbiter_driven = (options.env_source != EnvSource::None) || inject_enabled;

  ambient::core::EnvironmentModel env_model;
  ambient::core::AlertEngine alert_engine;
  PhotoClaimSource photo_claim;
  {
    ambient::core::EnvCutSchedule schedule;
    schedule.interval =
        ambient::core::Millis{static_cast<std::int64_t>(options.env_card_interval_ms)};
    schedule.duration =
        ambient::core::Millis{static_cast<std::int64_t>(options.env_card_duration_ms)};
    env_model.setCutSchedule(schedule);
    env_model.setUtcOffsetMinutes(local_utc_offset_minutes());
  }

  // ★登録順は固定。同一優先度は「span 内で先に現れたもの」が勝つ契約なので、
  //   順序が揺れると結果が揺れる（arbiter.hpp §3）。弱い順に並べてある。
  const std::array<ambient::core::IClaimSource*, 3> claim_sources{
      {&photo_claim, &env_model, &alert_engine}};
  ambient::core::StateMachine state_machine;
  std::vector<ambient::core::Claim> claims;
  ambient::core::TimeJumpDetector jump_detector;

  std::unique_ptr<ambient::core::IHttpClient> env_http;
  std::unique_ptr<EnvPoller> env_poller;
  if (options.env_source != EnvSource::None) {
    ambient::transport::GeoFrameConfig env_config;
    env_config.baseUrl = options.server_url;
    if (options.env_source == EnvSource::Fixture) {
      const std::string fixture_dir = (options.asset_dir / "fixtures").string();
      env_http = std::make_unique<ambient::transport::FixtureHttpClient>(fixture_dir);
      std::fprintf(stderr,
                   "[app] --env-source fixture: %s を IHttpClient 実装として使います。\n",
                   fixture_dir.c_str());
    } else {
      ambient::transport::CurlHttpConfig curl_config;
      std::string curl_error;
      env_http = ambient::transport::CurlHttpClient::create(curl_config, curl_error);
      if (env_http == nullptr) {
        std::fprintf(stderr, "[app] ★エラー★ %s\n", curl_error.c_str());
        return EXIT_FAILURE;
      }
      std::fprintf(stderr,
                   "[app] --env-source network: %s へ接続します（GET のみ）。\n",
                   env_config.baseUrl.c_str());
    }
    std::fprintf(stderr,
                 "[app] 環境ポーリング間隔 %.0f ms（1 周期あたり GET 3 本 = "
                 "env / roomenv / presence）\n"
                 "[app]   ★/api/env はサーバ側で 300 秒キャッシュされる。"
                 "地震の速報性の上限も約 5 分（企画書 §5.4）。\n",
                 options.env_interval_ms);
    env_poller = std::make_unique<EnvPoller>(std::move(env_http), env_config,
                                             options.env_interval_ms);
  }
  if (arbiter_driven) {
    std::fprintf(stderr,
                 "[app] 表示状態は core の decide() が決めます"
                 "（数字キーの手動切り替えは無効。環境カット %.0f ms ごとに %.0f ms）。\n",
                 options.env_card_interval_ms, options.env_card_duration_ms);
  }

  // 環境の View Model を作り直す必要があるか。
  // ★毎フレーム作り直すと文字列を大量に確保する。取得したときと、
  //   ENV を表示している間だけ 1 秒に 1 回に絞る（STALE 表示の更新のため）。
  bool env_view_dirty = false;
  Uint64 env_view_refreshed_at = 0;
  std::string alert_eid_shown;
  // ★AlertEngine の firstLoad。**最初に成功した ingest** で倒す
  //   （失敗した周期では倒さない。EnvPoller::Update に持たせない理由も同じ）。
  bool env_first_ingest = true;
  bool injected = false;
  std::size_t injected_count = 0;
  std::size_t env_ingest_count = 0;
  bool env_degraded = false;

  // -------------------------------------------------------------------------
  // トランジションの配線（Wave 2B）
  //
  // ★遷移させるのは「写真」ではなく「画面 1 枚ぶん」である。
  //   キャプションや時計を含んだ完成フレームどうしを混ぜないと、遷移中だけ
  //   文字が消えたり、前の写真のキャプションが新しい写真の上に残ったりする。
  //   そこで切り替えの瞬間に前後のフレームをキャンバスへ焼いて、以後の
  //   700〜1000ms はそれを合成するだけにする（毎フレーム描き直さない）。
  //
  // ★★ 申し送り: KenBurns だけキャプションを載せられない ★★
  //
  //   draw_photo_screen() は必ず先に背景を塗る（screens.cpp の
  //   draw_background_photo が無条件で fill する）ので、「自前で描いた背景の
  //   上にキャプションだけ重ねる」経路が存在しない。遷移はキャンバスに
  //   完成フレームを焼いてから混ぜるので問題ないが、KenBurns は
  //   **写真そのものを毎フレーム切り出し直す**ため、完成フレームを焼く手が
  //   使えない（フレームごと拡大するとキャプションまで一緒に拡大する）。
  //
  //   一行で解ける。どちらか一方を screens.cpp / image.hpp の担当が入れれば済む:
  //     (a) image.hpp に Texture::from_raw(SDL_Texture*, int w, int h) を足す
  //         → KenBurns をキャンバスへ描いてそれを背景として渡せる。
  //           1280x720 のキャンバスなら draw_cover は等倍なので他は無改造。
  //     (b) draw_photo_screen に「背景は呼び出し側が描く」版を足す
  //   ここで scrim とキャプションを書き写す手は採らない（意匠が 2 か所に増え、
  //   ずれてもどのテストにも引っかからない）。
  //
  //   それまでの間、--dwell kenburns は **写真レイヤだけ** を描く。
  //   見た目を混在させないため、このモードでは遷移も写真レイヤだけで行う。
  const bool use_composed_frames = !options.dwell_ken_burns;

  ambient::core::TransitionRng transition_rng{options.transition_seed};
  std::unique_ptr<ambient::render::Canvas> canvas_from;
  std::unique_ptr<ambient::render::Canvas> canvas_to;
  std::optional<ambient::core::PlannedTransition> active_transition;
  std::optional<ambient::core::PlannedDwell> active_dwell;
  long transition_frames = 0;
  std::size_t transitions_played = 0;
  std::size_t transitions_cut_short = 0;

  if (slideshow != nullptr && use_composed_frames) {
    std::string canvas_error;
    canvas_from = ambient::render::Canvas::create(display->renderer(), kLogicalWidth,
                                                  kLogicalHeight, canvas_error);
    canvas_to = ambient::render::Canvas::create(display->renderer(), kLogicalWidth,
                                               kLogicalHeight, canvas_error);
    if (canvas_from == nullptr || canvas_to == nullptr) {
      // ★致命ではない。写真テクスチャを直接混ぜる形へ縮退する
      //   （遷移中はキャプションが出ない）。どちらの経路で動いているかは残す。
      canvas_from.reset();
      canvas_to.reset();
      std::fprintf(stderr,
                   "[app] 合成キャンバスを用意できませんでした（写真レイヤのみで遷移"
                   "します）: %s\n",
                   canvas_error.c_str());
    }
  }
  if (slideshow != nullptr) {
    std::fprintf(stderr, "[app] トランジション: %s / 滞在効果: %s / fps 上限: %s\n",
                 (canvas_from != nullptr) ? "完成フレームを合成" : "写真レイヤのみ",
                 options.dwell_ken_burns ? "KenBurns（キャプション無し）" : "None",
                 options.transition_fps_cap > 0.0
                     ? std::to_string(static_cast<int>(options.transition_fps_cap)).c_str()
                     : "無制限(vsync)");
  }

  // 「今この瞬間に画面へ出ている写真のフレーム」をキャンバスへ焼く。
  const auto bake_frame = [&](ambient::render::Canvas* canvas,
                              const ambient::render::Texture* texture) {
    if (canvas == nullptr) {
      return;
    }
    const ambient::render::Canvas::Scope scope(display->renderer(), *canvas);
    if (!scope.ok()) {
      return;
    }
    SDL_SetRenderDrawBlendMode(display->renderer(), SDL_BLENDMODE_NONE);
    SDL_SetRenderDrawColor(display->renderer(), 0, 0, 0, 255);
    SDL_RenderClear(display->renderer());
    ambient::render::draw_screen(display->renderer(), DisplayStateKind::Photo, screen_fonts,
                                 texture, screen_data, layout);
  };

  FrameLimiter frame_limiter(options.transition_fps_cap, counter_frequency);

  bool running = true;
  int frames_rendered = 0;
  double fps = 0.0;
  Uint64 fps_window_start = SDL_GetPerformanceCounter();
  int fps_window_frames = 0;
  ambient::render::LogicalPoint last_click;
  bool has_click = false;

  RunTimingStats run_stats;
  run_stats.start_counter = SDL_GetPerformanceCounter();
  run_stats.prev_frame_end_counter = run_stats.start_counter;
  if (run_seconds_mode) {
    std::fprintf(stderr, "[app] --run-seconds %.3f 秒間描画してから自動終了します。\n",
                 options.run_seconds);
  }

  while (running) {
    SDL_Event event;
    while (SDL_PollEvent(&event) != 0) {
      switch (event.type) {
        case SDL_QUIT:
          running = false;
          break;
        case SDL_KEYDOWN: {
          const SDL_Keycode key = event.key.keysym.sym;
          if (key == SDLK_ESCAPE) {
            running = false;
            break;
          }
          // 数字キー 1〜5 で表示状態を切り替える（目視確認用）。
          // ★アービター駆動のときは効かない。ここで上書きすると
          //   「decide() が決めた状態」を app が横取りすることになる。
          if (key >= SDLK_1 && key <= SDLK_5) {
            const std::size_t index = static_cast<std::size_t>(key - SDLK_1);
            if (arbiter_driven) {
              std::fprintf(stderr,
                           "[app] 表示状態は core の decide() が決めています"
                           "（手動切り替えは無効）。\n");
            } else {
              current_screen = kScreenChoices[index].kind;
              std::fprintf(stderr, "[app] 画面切り替え: %s\n",
                           kScreenChoices[index].name);
            }
          }
          if (key == SDLK_n && slideshow != nullptr) {
            slideshow->skip_now();
          }
          // Q キー: 合成地震を 1 件ずつ注入する。
          // ★AlertEngine を通す。ここで「ALERT を出す」判断はしない。
          if (key == SDLK_q && inject_enabled) {
            const std::size_t index = injector.nextIndex();
            if (const ambient::core::QuakeItem* quake = injector.at(index)) {
              const std::array<ambient::core::QuakeItem, 1> one{*quake};
              const ambient::core::SteadyTime steady = steady_now();
              alert_engine.ingest(one, steady, /*firstLoad=*/false,
                                  /*otherEventActive=*/false);
              ++injected_count;
              std::fprintf(stderr,
                           "[app] 地震を注入: [%zu] eid=%s 震度=%s M%s %s → %s\n",
                           index, quake->eid.c_str(), quake->maxi.c_str(),
                           quake->mag.c_str(), quake->place.c_str(),
                           alert_engine.alerting(steady) ? "発火中"
                                                         : "発火せず（震度未満/既知/"
                                                           "クールダウン中）");
            }
          }
          break;
        }
        case SDL_MOUSEBUTTONDOWN: {
          // 【重要】ウィンドウ座標を直接使わないこと。必ず window_to_logical を通す。
          last_click = display->window_to_logical(event.button.x, event.button.y);
          has_click = true;
          const bool inside = last_click.x >= 0.0F && last_click.y >= 0.0F &&
                              last_click.x <= static_cast<float>(kLogicalWidth) &&
                              last_click.y <= static_cast<float>(kLogicalHeight);
          std::fprintf(stderr,
                       "[app] クリック window=(%d,%d) -> logical=(%.1f,%.1f) %s\n",
                       event.button.x, event.button.y,
                       static_cast<double>(last_click.x),
                       static_cast<double>(last_click.y),
                       inside ? "" : "← レターボックスの黒帯（範囲外）");

          // CARE 画面ではカードの当たり判定を取る。
          // ★実際の記録（送信キューへの投入）は後続フェーズの担当なので、
          //   ここではヒットしたスロット番号をログに出すだけに留める。
          //   当たり判定そのものは ambient/render/screens.hpp の
          //   hit_test_care() に切り出してあり、後続フェーズはそれを使う。
          if (current_screen == DisplayStateKind::Care) {
            const auto hit =
                ambient::render::hit_test_care(screen_data.care, last_click, layout);
            if (hit) {
              std::fprintf(stderr,
                           "[app]   → CARE カードにヒット: index=%zu slot=%zu "
                           "label=%s（記録処理は後続フェーズ）\n",
                           hit->index, hit->slot,
                           screen_data.care.cards[hit->index].label.c_str());
            } else {
              std::fprintf(stderr, "[app]   → CARE カードの外側\n");
            }
          }
          break;
        }
        default:
          break;
      }
    }

    // 一定時間ぶんのフレーム数から FPS を出す。瞬間値（1/delta）は
    // 起動直後の未スロットルなフレームで数千まで跳ねて意味を成さない。
    ++fps_window_frames;
    const Uint64 now = SDL_GetPerformanceCounter();
    const double window_elapsed =
        static_cast<double>(now - fps_window_start) / counter_frequency;
    if (window_elapsed >= kFpsWindowSeconds) {
      fps = static_cast<double>(fps_window_frames) / window_elapsed;
      fps_window_frames = 0;
      fps_window_start = now;
    }

    // -----------------------------------------------------------------------
    // 写真の巡回（--photo-source が none 以外のとき）
    //
    // ★ここでは **一切ブロックしない**。manifest の取得はポーリングスレッド、
    //   画像の取得とデコードは ImageWorker のスレッドで進んでいる。
    //   サーバが落ちていても描画ループは 60fps のまま回り続ける。
    // -----------------------------------------------------------------------
    const ambient::render::Texture* background = photo.get();
    if (slideshow != nullptr) {
      if (manifest_poller != nullptr) {
        ManifestPoller::Update update;
        if (manifest_poller->take(update)) {
          const bool ok = update.status == ambient::transport::GeoFrameStatus::Ok;
          if (ok) {
            // ★★ clearFailures() は「復帰したとき **だけ**」呼ぶ ★★
            //   毎回呼ぶと、恒久的に取れない写真（404 のまま消えていない
            //   derivative など）を manifest 間隔ごとに永久に取りに行くことになる。
            //   認証の無いサーバへ 30 秒ごとに無駄な GET を撃ち続ける形であり、
            //   LRU スラッシングと同じ「静かに負荷を増やす」種類の不具合になる。
            //   代償は「通信が正常なまま 1 回だけ失敗した写真は、次に通信が
            //   落ちて復帰するまで飛ばされ続ける」こと。表示は止まらないので
            //   こちらの方が安全側。
            if (photos_degraded) {
              std::fprintf(stderr,
                           "[app] /api/photos が復帰しました（%zu 件）。"
                           "失敗した写真の再試行を許可します。\n",
                           update.manifest.items.size());
              // Excluded も忘れるのは「一時」除外だからで、恒久排除ではない。
              image_worker->clearFailures();
            }
            slideshow->set_manifest(std::move(update.manifest));
          } else if (!photos_degraded) {
            std::fprintf(stderr, "[app] /api/photos の取得に失敗: %s (%s)\n",
                         update.message.c_str(),
                         ambient::transport::toString(update.status));
          }
          photos_degraded = !ok;
        }
      }

      if (const std::optional<PhotoSwitchEvent> switched = slideshow->update(now)) {
        // ★★ ここが「今この写真から次の写真へ移る」点である（Wave 2B）★★
        //   switched->from_key が空 = 遷移元が無い（初回）ので演出は出さない。
        if (live_photo != nullptr && slideshow->current_image() != nullptr) {
          std::string present_error;
          if (!live_photo->present(*slideshow->current_image(), present_error)) {
            std::fprintf(stderr, "[app] 写真の転送に失敗: %s\n", present_error.c_str());
          }
        }

        // ★遷移元のフレームは **キャプションを差し替える前** に焼くこと。
        //   焼いた後に screen_data.photo を新しい写真のものへ更新する。
        const bool has_previous =
            !switched->from_key.empty() && live_photo != nullptr &&
            live_photo->previous() != nullptr;
        if (has_previous) {
          bake_frame(canvas_from.get(), live_photo->previous());
        }

        const ambient::core::PhotoItem* item = slideshow->current_item();
        const ambient::render::RgbaImage* shown = slideshow->current_image();
        if (item != nullptr) {
          screen_data.photo = make_caption(*item);
          std::fprintf(stderr, "[app] 写真: #%zu id=%s %s (%ux%u)\n", switched->to_index,
                       switched->to_key.c_str(), item->file.value.c_str(),
                       (shown != nullptr) ? shown->sourceWidth : 0U,
                       (shown != nullptr) ? shown->sourceHeight : 0U);
        }

        if (has_previous) {
          bake_frame(canvas_to.get(), live_photo->current());

          // ★遷移中に次の切り替えが来た場合。テクスチャは 2 枚しか無いので
          //   遷移元は既に上書きされている。ここで打ち切って新しい遷移を
          //   「今の 1 つ前の写真 → 今の写真」として張り直す。
          //   **切り替えそのものは絶対に抑止しない**（巡回が止まるため）。
          if (active_transition.has_value()) {
            ++transitions_cut_short;
          }
          const ambient::core::SteadyTime steady = steady_now();
          active_transition = ambient::core::planTransition(
              ambient::core::kDefaultTransitionWeights, caps_report.caps, steady,
              transition_rng);
          transition_frames = 0;
          std::fprintf(stderr, "[app] 遷移: %s %lld ms / %s\n",
                       ambient::core::toString(active_transition->plan.kind),
                       static_cast<long long>(active_transition->plan.duration.value),
                       describe_params(active_transition->plan.kind, active_transition->params)
                           .c_str());
        }

        // ★滞在効果は写真 1 枚ごとに引き直す。caps によらず同じ回数だけ
        //   乱数を引くので、blend を切っても以降の演出列はずれない。
        active_dwell = ambient::core::planDwell(
            options.dwell_ken_burns ? caps_report.caps : ambient::core::RenderCaps{false},
            steady_now(), transition_rng);
      }

      if (live_photo != nullptr && live_photo->current() != nullptr) {
        background = live_photo->current();
      }
    }

    // -----------------------------------------------------------------------
    // 環境・地震 → claim → decide()（Wave 3B）
    //
    // ★ここでも一切ブロックしない。取得は EnvPoller のスレッドで進んでいる。
    //   サーバが落ちていても描画ループは回り続ける。
    //
    // ★「地震が来たら ALERT」はここに書かない。AlertEngine が claim を出し、
    //   decide() が優先度で選ぶ。app は選ばれた kind を描くだけ。
    // -----------------------------------------------------------------------
    if (arbiter_driven) {
      const ambient::core::SteadyTime steady = steady_now();

      // スリープ復帰 / NTP 同期の跳躍。★検知も対処のポリシーも core にある。
      if (const std::optional<ambient::core::TimeJump> jump =
              jump_detector.update(steady, wall_now())) {
        alert_engine.onTimeJump(*jump);
        env_model.onTimeJump(*jump);
        std::fprintf(stderr,
                     "[app] 時刻の跳躍を検知: skew=%lld ms"
                     "（クールダウンと STALE の基準を引き直しました）\n",
                     static_cast<long long>(jump->skew.value));
      }

      if (env_poller != nullptr) {
        EnvPoller::Update update;
        if (env_poller->take(update)) {
          const ambient::core::WallTime wall = wall_now();
          const bool ok = update.env_ok && update.room_ok && update.presence_ok;
          if (update.env_ok) {
            env_model.ingestEnv(update.env, steady, wall);
            // ★地震は AlertEngine（エッジ→レベル変換器）へ。
            //   firstLoad は **最初に成功した応答だけ** true。
            //   ★フラグを「最初の周期」で消してはいけない。起動直後に
            //     サーバがまだ立ち上がっていない（= 1 周期目が失敗する）と、
            //     2 周期目が firstLoad=false で入り、JMA の 24 時間窓に
            //     入っている過去の地震が「新着」として発火する。
            //     成功した ingest の直後にだけ倒すこと。
            alert_engine.ingest(update.env.quakes, steady, env_first_ingest,
                                /*otherEventActive=*/false);
            env_first_ingest = false;
          }
          if (update.room_ok) {
            env_model.ingestRoomEnv(update.room, steady, wall);
          }
          if (update.presence_ok) {
            env_model.ingestPresence(update.presence, steady);
          }
          ++env_ingest_count;
          env_view_dirty = true;

          if (!ok && !env_degraded) {
            // ★写真も環境も止めない。企画書 §10:「Home Assistant不達 →
            //   最終値に STALE 表示。写真表示は継続」。Degraded にはしない。
            std::fprintf(stderr, "[app] 環境情報の取得に失敗: %s\n",
                         update.message.c_str());
          } else if (ok && env_degraded) {
            std::fprintf(stderr, "[app] 環境情報が復帰しました。\n");
          }
          env_degraded = !ok;
        }
      }

      // 自動注入（--inject-at）。★Q キーと同じく AlertEngine を通す。
      if (inject_enabled && !injected) {
        const double elapsed_seconds =
            static_cast<double>(now - run_stats.start_counter) / counter_frequency;
        if (elapsed_seconds >= options.inject_at_seconds) {
          injected = true;
          if (options.inject_selection.all) {
            alert_engine.ingest(injector.all(), steady, /*firstLoad=*/false,
                                /*otherEventActive=*/false);
            std::fprintf(stderr, "[app] 地震を注入: %zu 件まとめて（実応答と同じ形）\n",
                         injector.size());
          } else if (const ambient::core::QuakeItem* quake =
                         injector.at(options.inject_selection.index)) {
            const std::array<ambient::core::QuakeItem, 1> one{*quake};
            alert_engine.ingest(one, steady, /*firstLoad=*/false,
                                /*otherEventActive=*/false);
            std::fprintf(stderr, "[app] 地震を注入: [%zu] eid=%s 震度=%s M%s %s\n",
                         options.inject_selection.index, quake->eid.c_str(),
                         quake->maxi.c_str(), quake->mag.c_str(), quake->place.c_str());
          }
          ++injected_count;
        }
      }

      // ★claim を集めて decide() に決めさせる。登録順は固定。
      ambient::core::collectClaims(claim_sources, steady, claims);
      if (const std::optional<ambient::core::Transition> transition =
              state_machine.update(claims, steady)) {
        std::fprintf(stderr, "[app] 表示状態: %s#%u -> %s#%u\n",
                     ambient::core::toString(transition->from), transition->fromPayloadId,
                     ambient::core::toString(transition->to), transition->toPayloadId);
      }
      current_screen = state_machine.current();

      // ALERT の中身（生の maxi 文字列をそのまま渡す）。
      if (alert_engine.alerting(steady)) {
        const ambient::core::QuakeItem& quake = alert_engine.activeQuake();
        if (quake.eid != alert_eid_shown) {
          screen_data.alert = make_alert_screen(quake);
          alert_eid_shown = quake.eid;
          std::fprintf(stderr, "[app] アラート発火: eid=%s 震度=%s %s\n", quake.eid.c_str(),
                       quake.maxi.c_str(), quake.place.c_str());
        }
      }

      // ENV の中身。★毎フレーム作り直さない（文字列を大量に確保するため）。
      const bool showing_env = current_screen == DisplayStateKind::Env;
      const double since_refresh =
          static_cast<double>(now - env_view_refreshed_at) / counter_frequency;
      if (env_view_dirty || (showing_env && since_refresh >= 1.0)) {
        const ambient::core::EnvView view = env_model.view(steady);
        if (!view.empty()) {
          screen_data.env = make_env_screen(view);
        }
        env_view_dirty = false;
        env_view_refreshed_at = now;
      }
    }

    // -----------------------------------------------------------------------
    // 遷移 / 滞在効果（PHOTO 画面のときだけ）
    //
    // ★進捗は SteadyTime から出す。フレーム番号は一切登場しない。
    //   したがって --transition-fps-cap を掛けても演出の長さは変わらず、
    //   同じカーブを疎にサンプルするだけになる。
    // -----------------------------------------------------------------------
    bool drew_effect = false;
    if (current_screen == DisplayStateKind::Photo && slideshow != nullptr) {
      const ambient::core::SteadyTime steady = steady_now();
      const auto texture_of = [](const ambient::render::Texture* texture) -> SDL_Texture* {
        return (texture != nullptr) ? texture->raw() : nullptr;
      };

      if (active_transition.has_value()) {
        const ambient::core::Progress progress =
            active_transition->plan.progressAt(steady);
        SDL_Texture* from_texture =
            (canvas_from != nullptr)
                ? canvas_from->raw()
                : texture_of(live_photo != nullptr ? live_photo->previous() : nullptr);
        SDL_Texture* to_texture =
            (canvas_to != nullptr)
                ? canvas_to->raw()
                : texture_of(live_photo != nullptr ? live_photo->current() : nullptr);
        if (to_texture != nullptr) {
          ambient::render::draw_transition(display->renderer(), active_transition->plan,
                                           active_transition->params, progress, from_texture,
                                           to_texture, kLogicalWidth, kLogicalHeight);
          ++transition_frames;
          drew_effect = true;
        }
        if (progress.done()) {
          ++transitions_played;
          // ★低 fps でどれだけ疎になったかの記録。実機の見え方の予測に使う。
          std::fprintf(stderr, "[app]   %s 完了: %ld フレーム（%lld ms）\n",
                       ambient::core::toString(active_transition->plan.kind),
                       transition_frames,
                       static_cast<long long>(active_transition->plan.duration.value));
          active_transition.reset();
        }
      } else if (active_dwell.has_value() &&
                 active_dwell->plan.effect == ambient::core::DwellEffect::KenBurns &&
                 live_photo != nullptr && live_photo->current() != nullptr) {
        ambient::render::draw_ken_burns(display->renderer(), active_dwell->kenBurns,
                                        active_dwell->plan.progressAt(steady),
                                        live_photo->current()->raw(), kLogicalWidth,
                                        kLogicalHeight);
        drew_effect = true;
      }
    }

    if (drew_effect) {
      // ★演出が画面全面を占めるので draw_screen は通さない。HUD だけ載せる。
      draw_dev_hud(display->renderer(), hud_font, fps, last_click, has_click);
    } else {
      render_frame(*display, screen_fonts, hud_font, background, current_screen, screen_data,
                   fps, last_click, has_click, /*show_hud=*/true);
    }
    ++frames_rendered;
    record_frame_timing(run_stats, counter_frequency);

    // --run-seconds 未指定時の従来動作: FPS が実測値になり次第すぐスクリーンショットを
    // 撮って終了する（"-- FPS" を残さない）。--run-seconds 指定時はこちらではなく
    // 下の run_seconds 到達判定の中でまとめて保存する（N秒回してから最後に1枚）。
    if (screenshot_mode && !run_seconds_mode && frames_rendered >= kScreenshotWarmupFrames &&
        fps > 0.0) {
      // ★ SDL_RenderPresent の前に読むこと。present 後はバックバッファが
      //    未定義になり真っ黒な画像が保存される。
      if (!display->save_screenshot(options.screenshot_path, error)) {
        std::fprintf(stderr, "[app] ★エラー★ %s\n", error.c_str());
        exit_code = EXIT_FAILURE;
      }
      SDL_RenderPresent(display->renderer());
      running = false;
      break;
    }

    if (run_seconds_mode && run_stats.total_elapsed_seconds >= options.run_seconds) {
      // 指定秒数に到達。--screenshot も指定されていれば、ここで最後の1枚を保存する
      // （耐久試験で「N秒回した後の画面」を撮るための経路）。
      if (screenshot_mode) {
        // ★ SDL_RenderPresent の前に読むこと（理由は上のコメントと同じ）。
        if (!display->save_screenshot(options.screenshot_path, error)) {
          std::fprintf(stderr, "[app] ★エラー★ %s\n", error.c_str());
          exit_code = EXIT_FAILURE;
        }
      }
      SDL_RenderPresent(display->renderer());
      running = false;
      break;
    }

    SDL_RenderPresent(display->renderer());
    // ★fps 上限。present の後に待つ（present 前に待つと vsync と二重に待つ）。
    frame_limiter.wait();
  }

  if (run_seconds_mode) {
    print_run_seconds_summary(run_stats);
  }

  if (slideshow != nullptr) {
    std::fprintf(stderr, "[app] 遷移: 再生 %zu 回 / 途中で打ち切り %zu 回\n",
                 transitions_played, transitions_cut_short);
    if (transitions_cut_short > 0) {
      std::fprintf(stderr,
                   "[app]   ★打ち切りは「写真の間隔が演出より短い」印です。"
                   "テクスチャは 2 枚しか無いので遷移元が上書きされます。\n");
    }
  }

  if (arbiter_driven) {
    std::fprintf(stderr, "\n[app] ===== 環境・地震のサマリ =====\n");
    if (env_poller != nullptr) {
      // ★実サーバへ何本投げたかは必ず数えて出す。ポーリングの設定ミスは
      //   「表示は正常に見えるがログを読むまで気づけない」種類の事故になる。
      std::fprintf(stderr,
                   "[app] 環境ポーリング: %zu 周期 / GET %zu 本"
                   "（間隔 %.0f ms、1 周期 3 本）\n",
                   env_poller->poll_count(), env_poller->request_count(),
                   options.env_interval_ms);
      std::fprintf(stderr, "[app] メインスレッドで取り込んだ回数: %zu\n", env_ingest_count);
    }
    std::fprintf(stderr, "[app] 地震の注入: %zu 回 / 既知の eid: %zu 件\n", injected_count,
                 alert_engine.seenCount());
    std::fprintf(stderr, "[app] 表示状態の遷移: %zu 件（直近 %zu 件を保持）\n",
                 state_machine.history().size(), ambient::core::StateMachine::kHistoryMax);
    // ★「未設定」と「そもそも取りに行っていない」を混ぜない。
    //   前者は capability-gate（HA なし）で、後者は --env-source none である。
    const char* room_state = "未取得（--env-source none）";
    if (env_poller != nullptr) {
      room_state = env_model.roomEnvNotConfigured() ? "未設定（Home Assistant なし）" : "あり";
    }
    std::fprintf(stderr, "[app] 最終状態: %s / 室内センサー: %s\n",
                 ambient::core::toString(state_machine.current()), room_state);
    std::fprintf(stderr, "[app] ================================\n\n");
  }

  if (image_worker != nullptr) {
    const ambient::render::ImageWorkerStats stats = image_worker->stats();
    std::fprintf(stderr, "\n[app] ===== 写真パイプラインのサマリ =====\n");
    std::fprintf(stderr, "[app] 要求 %zu / 完了 %zu / 失敗 %zu / 一時除外 %zu\n",
                 stats.requested, stats.completed, stats.failed, stats.excluded);
    std::fprintf(stderr, "[app] LRU 追い出し %zu / キャッシュヒット %zu\n", stats.evicted,
                 stats.cacheHits);
    std::fprintf(stderr, "[app] キャッシュ現在: %zu 枚 / %.1f MB（上限 %zu 枚）\n",
                 stats.cachedImages,
                 static_cast<double>(stats.cachedBytes) / (1024.0 * 1024.0),
                 stats.cacheCapacity);
    std::fprintf(stderr, "[app] 1 枚のデコードの最大 RGBA バイト数: %.1f MB\n",
                 static_cast<double>(stats.peakDecodedBytes) / (1024.0 * 1024.0));
    if (slideshow != nullptr) {
      std::fprintf(stderr, "[app] 表示した写真 %zu 枚 / 飛ばした写真 %zu 枚 / manifest %zu 件\n",
                   slideshow->shown(), slideshow->skipped(), slideshow->manifest_size());
    }
    if (manifest_poller != nullptr) {
      std::fprintf(stderr, "[app] /api/photos のポーリング回数: %zu\n",
                   manifest_poller->poll_count());
    }
    std::fprintf(stderr, "[app] ====================================\n\n");
  }

  // ★ここに photo / *_font / display の reset() を並べる必要はない。
  //   この関数を抜けるときに宣言と逆順で自動的に破棄される（上の宣言順コメント）。
  //   明示 reset() を正常終了パスだけに置くと、エラー経路との非対称が復活する。
  return exit_code;
}

}  // namespace

int main(int argc, char** argv) {
  Options options;
  if (!parse_options(argc, argv, options)) {
    return EXIT_FAILURE;
  }
  if (options.show_help) {
    print_usage(argv[0]);
    return EXIT_SUCCESS;
  }

  if (options.font_path.empty()) {
    options.font_path = options.asset_dir / "fonts" / "NotoSansJP.ttf";
  }
  if (options.photo_path.empty()) {
    options.photo_path = options.asset_dir / "fixtures" / "sample-photo.jpg";
  }

  if (options.screen_all) {
    if (options.screenshot_path.empty()) {
      std::fprintf(stderr,
                   "[app] --screen all は --screenshot <dir> と一緒に指定して"
                   "ください（5画面ぶんの保存先ディレクトリ）。\n");
      return EXIT_FAILURE;
    }
    // save_screenshot は親ディレクトリを作らない（IMG_SavePNG が黙って失敗する）。
    std::error_code ec;
    fs::create_directories(options.screenshot_path, ec);
    if (!fs::is_directory(options.screenshot_path, ec)) {
      std::fprintf(stderr,
                   "[app] ★エラー★ スクリーンショットの保存先ディレクトリを"
                   "用意できません: %s\n",
                   options.screenshot_path.c_str());
      return EXIT_FAILURE;
    }
    if (options.run_seconds > 0.0) {
      std::fprintf(stderr, "[app] --screen all では --run-seconds は無視されます。\n");
    }
    if (options.photo_source != PhotoSource::None) {
      // ★--screen all は「ネットワーク完全遮断で 5 画面を出せること」の検証経路。
      //   ここで写真を取りに行くと取得したものが決定的でなくなる。
      std::fprintf(stderr, "[app] --screen all では --photo-source は無視されます。\n");
      options.photo_source = PhotoSource::None;
    }
  }

  std::fprintf(stderr, "[app] ambient_core   %s\n", ambient::core::version_string());
  std::fprintf(stderr, "[app] core target    %s\n", ambient::core::target_note());
  std::fprintf(stderr, "[app] transport      %s (libcurl %s)\n",
               ambient::transport::version_string(),
               ambient::transport::curl_runtime_version());

  if (!require_file(options.font_path, "フォント",
                    "native-display/tools/fetch-fonts.sh を実行して取得してください")) {
    return EXIT_FAILURE;
  }
  if (!require_file(options.photo_path, "fixture 画像",
                    "native-display/tools/make-sample-fixture.py を実行して生成してください")) {
    return EXIT_FAILURE;
  }

  std::string error;
  if (!ambient::render::init_runtime(error)) {
    std::fprintf(stderr, "[app] ★エラー★ %s\n", error.c_str());
    // ★ここで shutdown_runtime() を呼んではいけない（対にするのは成功時だけ）。
    //   init_runtime は自分の失敗経路で必ず自分が確保したものを巻き戻している:
    //     SDL_Init 失敗  -> 何も確保していない
    //     IMG_Init 失敗  -> SDL_Quit() 済み
    //     TTF_Init 失敗  -> IMG_Quit(); SDL_Quit() 済み
    //   したがって失敗して戻ってきた時点で「初期化前」と同じ状態であり、
    //   ここで TTF_Quit / IMG_Quit / SDL_Quit を重ねて呼ぶ意味は無い。
    return EXIT_FAILURE;
  }
  ambient::render::log_runtime_versions();

  // ★curl_global_init は **スレッドセーフではない**。他のスレッドを起こす前、
  //   すなわち run() に入る前にメインスレッドから 1 回だけ呼ぶ。
  //   run() を抜けた時点で全ての CurlHttpClient と全スレッドは片付いている。
  std::string curl_error;
  const bool curl_ready = ambient::transport::CurlHttpClient::globalInit(curl_error);
  if (!curl_ready) {
    std::fprintf(stderr, "[app] ★エラー★ %s\n", curl_error.c_str());
    ambient::render::shutdown_runtime();
    return EXIT_FAILURE;
  }

  // ★この 3 行の順序が C-1 の修正そのもの。入れ替えないこと。
  //   run() を抜けた時点で SDL リソースのデストラクタは全て走り終わっているので、
  //   shutdown_runtime() を安全に呼べる。逆にすると解放済みメモリを触る。
  const int code = run(options);
  ambient::transport::CurlHttpClient::globalShutdown();
  ambient::render::shutdown_runtime();
  return code;
}
