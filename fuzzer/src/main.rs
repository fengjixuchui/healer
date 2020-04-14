use fuzzer::{init_env, show_info, start_up, Config};
use std::fs::read_to_string;
use std::path::PathBuf;
use std::process::exit;
use structopt::StructOpt;

#[derive(Debug, StructOpt)]
#[structopt(name = "fuzzer", about = "Kernel fuzzer of healer.")]
struct Settings {
    #[structopt(short = "c", long, default_value = "healer-fuzzer.toml")]
    config: PathBuf,
}

fn main() {
    let settings = Settings::from_args();
    let cfg_data = read_to_string(&settings.config).unwrap_or_else(|e| {
        eprintln!("Config error: {}: {}", settings.config.display(), e);
        exit(exitcode::IOERR)
    });

    let cfg: Config = toml::from_str(&cfg_data).unwrap_or_else(|e| {
        eprintln!("Config Error: {}: {}", settings.config.display(), e);
        exit(exitcode::CONFIG)
    });
    cfg.check();
    show_info();
    init_env();
    start_up(cfg)
}
