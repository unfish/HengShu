fn main() {
    hengshu::privacy::install_secret_scrubbing_panic_hook();
    if let Err(error) = hengshu::cli::run() {
        eprintln!("{}", hengshu::privacy::scrub_secrets(&error));
        std::process::exit(hengshu::api::FAILURE_EXIT_CODE);
    }
}
