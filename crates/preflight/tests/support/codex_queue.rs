use std::{io::Write,fs::OpenOptions};
fn main() {
 let root=std::env::current_exe().unwrap().parent().unwrap().to_path_buf();
 let args=std::env::args().skip(1).collect::<Vec<_>>();
 assert!(args.len()==5 || (args.len()==6 && args[5]=="--resume"));assert_eq!(args[0],"queue");assert_eq!(args[1],"--thread");assert_eq!(args[3],"--message");
 let mut log=OpenOptions::new().create(true).append(true).open(root.join("calls")).unwrap();
 writeln!(log,"{:?}",args).unwrap();log.flush().unwrap();
 match std::fs::read_to_string(root.join("mode")).unwrap_or_default().as_str() {
  "fail"=>std::process::exit(7),
  "sleep"=>{std::thread::sleep(std::time::Duration::from_secs(30));std::fs::write(root.join("unexpected_finish"),"bad").unwrap();},
  _=>{}
 }
}
