use std::{env,fs,process::Command,time::Duration};
fn main() {
    let args=env::args().skip(1).collect::<Vec<_>>();
    println!("EXECUTABLE={}",env::current_exe().unwrap().display());
    match args[0].as_str() {
        "produce-tree"=>{
            fs::create_dir_all("module-dir/lib").unwrap();fs::create_dir("module-dir/empty").unwrap();
            fs::write("module-dir/lib/module.dat","module tree payload").unwrap();
            for i in 0..105 {fs::write(format!("module-dir/lib/part-{i:03}"),"part").unwrap();}
        }
        "consume-tree"=>{
            let root=std::path::Path::new(&args[1]);
            assert_eq!(fs::read_to_string(root.join("lib/module.dat")).unwrap(),"module tree payload");
            assert!(root.join("empty").is_dir());println!("consumed directory module");
        }
        "produce"=>{fs::copy(env::current_exe().unwrap(),"worker.exe").unwrap();fs::write("payload.txt","artifact payload").unwrap();fs::write("private.txt","not declared").unwrap();}
        "consume"=>{assert_eq!(fs::read_to_string(&args[1]).unwrap(),"artifact payload");assert!(!std::path::Path::new("private.txt").exists());println!("consumed artifact");}
        "junit"=>{fs::write(&args[1],&args[2]).unwrap();}
        "sleep"=>{println!("ready");std::thread::sleep(Duration::from_millis(args[1].parse().unwrap()));}
        "fail"=>std::process::exit(7),
        "write"=>{fs::write(&args[1],args.get(2).map(String::as_str).unwrap_or("written")).unwrap();}
        "env"=>{assert_eq!(fs::read_to_string("input.txt").unwrap(),"artifact payload");fs::write("env.txt",env::var("STAGE_VALUE").unwrap()).unwrap();}
        "child"=>{
            let mut child=Command::new(env::current_exe().unwrap()).args(["delayed-write",&args[1]]).spawn().unwrap();
            println!("child ready");child.wait().unwrap();
        }
        "delayed-write"=>{std::thread::sleep(Duration::from_millis(1500));fs::write(&args[1],"survived").unwrap();}
        _=>panic!("unknown test worker mode"),
    }
}
