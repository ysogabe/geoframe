use std::process::ExitCode;

// musl 標準のアロケータは画像処理で 15〜25% 遅い（docs/rust-migration/image-poc.md §3.5）ので、
// musl のときだけ mimalloc を使う。glibc のビルドは標準のアロケータのまま
#[cfg(target_env = "musl")]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

fn main() -> ExitCode {
    geoframe::cli::run(std::env::args().skip(1))
}
