fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() == 1 && args[0] == "--version" {
        println!("{}", harness::version_line());
        return;
    }
    eprintln!("usage: harness --version");
    std::process::exit(2);
}
