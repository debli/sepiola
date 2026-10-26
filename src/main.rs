fn main() {
    if let Err(error) = sepiola::cli::run() {
        eprintln!("sepiola: {error}");
        std::process::exit(1);
    }
}
