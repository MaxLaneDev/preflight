#[cfg(feature = "intentional-failure")]
compile_error!("intentional Preflight build failure");

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    println!("Preflight helper: stdout");
    eprintln!("Preflight helper: stderr");
    match args.first().map(String::as_str) {
        Some("sleep") => std::thread::sleep(std::time::Duration::from_secs(60)),
        Some("fail") => std::process::exit(7),
        Some("echo") => println!("{}", args.get(1).expect("echo argument")),
        Some("write") => std::fs::write("result.txt", "workspace only").expect("write output"),
        Some("flood") => {
            use std::io::Write;
            let mut stdout = std::io::stdout().lock();
            for _ in 0..400 { stdout.write_all(&[b'x';4096]).unwrap(); }
        }
        _ => println!("helper finished"),
    }
}
