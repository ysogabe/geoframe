use std::process::ExitCode;

fn main() -> ExitCode {
    let mut args = Vec::new();
    for arg in std::env::args_os().skip(1) {
        match arg.into_string() {
            Ok(arg) => args.push(arg),
            Err(_) => {
                eprintln!("geoframe-export: 引数が UTF-8 ではありません");
                return ExitCode::from(geoframe_export::app::EXIT_USAGE);
            }
        }
    }
    // ログは標準エラーへ（標準出力はコマンドの JSON 専用）。完了ログと rclone の警告が出る
    geoframe_export::logging::init();
    // `run`・`serve` では、SIGINT・SIGTERM で終わらず、キャンセルの印を立てる。実行中の rclone を止めて `wait` し終えてから、
    // 実行枠を手放して終わる（子が残ったまま、枠だけが外れないように）。`run` の受け口は libc の handler、`serve` の受け口は
    // tokio のシグナル（どちらも `run_command_cancellable` の `handle_signals` が真のとき、その中で入れる）
    let cancel = geoframe_export::cancel::Cancel::new();
    // 標準出力・標準エラーは、コマンドの実行中ずっとロックしない（`lock()` のガードを持ち続けると、ワーカーのパニックの表示や
    // 標準エラーへ出すログが、別のスレッドでロック待ちになり、実行枠を外せなくなる）。書くたびにロックを取って放す
    let code = geoframe_export::app::run_command_cancellable(
        Some(geoframe_export::sys::euid()),
        &args,
        &mut std::io::stdout(),
        &mut std::io::stderr(),
        &cancel,
        true,
    );
    ExitCode::from(code)
}
