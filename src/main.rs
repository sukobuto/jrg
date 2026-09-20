use clap::Parser;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

fn main() {
    let args = jrg::cli::Args::parse();
    let cancelled = Arc::new(AtomicBool::new(false));
    let signal_flag = Arc::clone(&cancelled);
    if ctrlc::set_handler(move || signal_flag.store(true, Ordering::Relaxed)).is_err() {
        eprintln!("jrg: could not install interrupt handler");
        std::process::exit(2);
    }
    let result = jrg::cli::run(args, &cancelled);
    let code = if cancelled.load(Ordering::Relaxed) {
        130
    } else {
        match result {
            Ok(code) => code,
            Err(error) => {
                eprintln!("jrg: {error:#}");
                2
            }
        }
    };
    std::process::exit(code);
}
