//! Stand-alone out-of-process plugin scanner: `rlive-plugin-scanner --scan <clap|vst3|au> <path>`.

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = rlive_plugin_host::scanner::run_scanner_main(&args).unwrap_or_else(|| {
        eprintln!("usage: rlive-plugin-scanner --scan <clap|vst3|au> <path>");
        2
    });
    std::process::exit(code);
}
