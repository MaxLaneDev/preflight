// Example adapter target: compile `print TEXT` into a native executable via rustc.
use std::path::Path;
fn main() {
    let args: Vec<String> = std::env::args().collect();
    let source = std::fs::read_to_string(&args[1]).unwrap();
    let Some(payload) = source.strip_prefix("print ") else {
        eprintln!("error: expected print"); std::process::exit(1);
    };
    let rust = Path::new(&args[2]).with_extension("rs");
    let program = format!(r#"use std::io::Read;
fn main() {{
    let payload = {payload:?};
    if payload == "SLEEP" {{ std::thread::sleep(std::time::Duration::from_secs(5)); }}
    let mut input = String::new(); std::io::stdin().read_to_string(&mut input).unwrap();
    print!("{{payload}}{{input}}"); std::fs::write("result.txt",payload).unwrap();
}}"#);
    std::fs::write(&rust,program).unwrap();
    let status = std::process::Command::new("rustc").arg("--crate-name").arg("fixture_program")
        .arg(&rust).arg("-o").arg(&args[2]).status().unwrap();
    std::process::exit(status.code().unwrap_or(1));
}
