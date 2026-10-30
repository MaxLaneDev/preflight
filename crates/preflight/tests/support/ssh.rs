// Test transport only: executes the SSH remote command locally. This is not an SSH server.
fn main() {
    let args: Vec<_> = std::env::args().skip(1).collect();
    for flag in ["-T","-a","-x","-oStrictHostKeyChecking=yes","-oBatchMode=yes","-oExitOnForwardFailure=yes"] {
        assert!(args.iter().any(|a|a==flag),"missing SSH safety flag {flag}");
    }
    let script=args.last().unwrap();
    if script=="uname -s; uname -m" {
        println!("{}\n{}", if cfg!(target_os="macos") {"Darwin"} else {"Linux"},std::env::consts::ARCH);
        return;
    }
    if script.starts_with("exec ") {
        let index=args.iter().position(|a|a=="-R").expect("reverse tunnel argument");
        assert!(args[index+1].starts_with("127.0.0.1:"));
    }
    let status=std::process::Command::new("/bin/sh").args(["-c",script]).status().unwrap();
    std::process::exit(status.code().unwrap_or(1));
}
