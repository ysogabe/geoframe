use std::process::ExitCode;

fn main() -> ExitCode {
    geoframe::cli::run(std::env::args().skip(1))
}
