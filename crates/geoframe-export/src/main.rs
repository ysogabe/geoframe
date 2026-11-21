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
    // `run`・`serve` では、SIGINT・SIGTERM で終わらず、キャンセルの印を立てる。実行中の rclone を止めて `wait` し終えてから、
    // 実行枠を手放して終わる（子が残ったまま、枠だけが外れないように）。handler は `run_command_cancellable` が入れる
    let cancel = geoframe_export::cancel::Cancel::new();
    let code = geoframe_export::app::run_command_cancellable(
        Some(geoframe_export::sys::euid()),
        &args,
        &mut std::io::stdout().lock(),
        &mut std::io::stderr().lock(),
        &cancel,
        true,
    );
    ExitCode::from(code)
}
