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
    let code = geoframe_export::app::run_command(
        Some(geoframe_export::sys::euid()),
        &args,
        &mut std::io::stdout().lock(),
        &mut std::io::stderr().lock(),
    );
    ExitCode::from(code)
}
