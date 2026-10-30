//! Opt-in actual loopback OpenSSH fixture. Never changes system SSH configuration.
use std::{
    os::unix::{fs::PermissionsExt, process::CommandExt},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
};
pub struct Server {
    child: Child,
    pub port: u16,
    pub command: PathBuf,
    pub known_hosts: PathBuf,
    trusted: String,
}
pub fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}
fn quote(path: &Path) -> String {
    format!("'{}'", path.to_str().unwrap().replace('\'', "'\\''"))
}
impl Server {
    pub fn new(root: &Path) -> Self {
        let root = root.join("real-ssh");
        std::fs::create_dir(&root).unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        for name in ["host", "client"] {
            let result = Command::new("/usr/bin/ssh-keygen")
                .args(["-q", "-t", "ed25519", "-N", "", "-f"])
                .arg(root.join(name))
                .output()
                .unwrap();
            assert!(result.status.success());
        }
        let authorized = root.join("authorized_keys");
        std::fs::copy(root.join("client.pub"), &authorized).unwrap();
        std::fs::set_permissions(&authorized, std::fs::Permissions::from_mode(0o600)).unwrap();
        let user = Command::new("id").arg("-un").output().unwrap();
        assert!(user.status.success());
        let user = String::from_utf8(user.stdout).unwrap();
        let port = free_port();
        let config = root.join("sshd_config");
        std::fs::write(&config,format!("Port {port}\nListenAddress 127.0.0.1\nHostKey {}\nPidFile {}\nAuthorizedKeysFile {}\nPasswordAuthentication no\nKbdInteractiveAuthentication no\nUsePAM no\nAllowUsers {}\nAllowTcpForwarding remote\nGatewayPorts no\nLogLevel VERBOSE\n",root.join("host").display(),root.join("pid").display(),authorized.display(),user.trim())).unwrap();
        let known_hosts = root.join("known_hosts");
        let public = std::fs::read_to_string(root.join("host.pub")).unwrap();
        let parts: Vec<_> = public.split_whitespace().collect();
        let trusted = format!("[127.0.0.1]:{port} {} {}\n", parts[0], parts[1]);
        std::fs::write(&known_hosts, "").unwrap();
        let client_config = root.join("ssh_config");
        std::fs::write(&client_config,format!("Host *\n IdentityFile {}\n IdentitiesOnly yes\n UserKnownHostsFile {}\n GlobalKnownHostsFile /dev/null\n",root.join("client").display(),known_hosts.display())).unwrap();
        let command = root.join("ssh");
        std::fs::write(
            &command,
            format!(
                "#!/bin/sh\nexec /usr/bin/ssh -F {} \"$@\"\n",
                quote(&client_config)
            ),
        )
        .unwrap();
        std::fs::set_permissions(&command, std::fs::Permissions::from_mode(0o700)).unwrap();
        let log = std::fs::File::create(root.join("sshd.log")).unwrap();
        let child = Command::new("/usr/sbin/sshd")
            .args(["-D", "-e", "-f"])
            .arg(config)
            .stdin(Stdio::null())
            .stdout(log.try_clone().unwrap())
            .stderr(log)
            .process_group(0)
            .spawn()
            .unwrap();
        let mut server = Self {
            child,
            port,
            command,
            known_hosts,
            trusted,
        };
        for _ in 0..100 {
            assert!(
                server.child.try_wait().unwrap().is_none(),
                "sshd exited: {}",
                std::fs::read_to_string(root.join("sshd.log")).unwrap_or_default()
            );
            if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
                return server;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        panic!("sshd did not listen");
    }
    pub async fn relay(&mut self) -> Relay {
        let relay = Relay::start(self.port).await;
        self.trusted = self.trusted.replace(
            &format!("[127.0.0.1]:{} ", self.port),
            &format!("[127.0.0.1]:{} ", relay.port),
        );
        self.port = relay.port;
        relay
    }
    pub fn trust(&self) {
        std::fs::write(&self.known_hosts, &self.trusted).unwrap();
    }
    pub fn disconnect_transport(&self, launcher: i32) {
        // Only a live SSH transport in this test launcher's private process group.
        let output = Command::new("ps")
            .args(["-ax", "-o", "pid=,pgid=,comm="])
            .output()
            .unwrap();
        assert!(output.status.success());
        let text = String::from_utf8(output.stdout).unwrap();
        let transports: Vec<i32> = text
            .lines()
            .filter_map(|line| {
                let mut fields = line.split_whitespace();
                let pid: i32 = fields.next()?.parse().ok()?;
                let group: i32 = fields.next()?.parse().ok()?;
                (group == launcher && Path::new(fields.next()?).file_name()? == "ssh")
                    .then_some(pid)
            })
            .collect();
        assert_eq!(
            transports.len(),
            1,
            "expected one SSH transport in the owned launcher group"
        );
        let pid = transports[0];
        unsafe {
            assert_eq!(libc::getpgid(pid), launcher);
            assert_eq!(libc::kill(pid, libc::SIGKILL), 0);
            assert_eq!(
                libc::kill(self.child.id() as i32, 0),
                0,
                "the test SSH daemon must stay alive"
            );
        }
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        unsafe {
            libc::kill(-(self.child.id() as i32), libc::SIGKILL);
        }
        let _ = self.child.wait();
    }
}

/// Holds both directions without EOF, TCP resets, process signals or dropped bytes.
pub struct Relay {
    pub port: u16,
    paused: tokio::sync::watch::Sender<bool>,
    task: tokio::task::JoinHandle<()>,
}
impl Relay {
    async fn start(upstream: u16) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let (paused, gate) = tokio::sync::watch::channel(false);
        let task = tokio::spawn(async move {
            let mut connections = tokio::task::JoinSet::new();
            loop {
                tokio::select! {
                    accepted = listener.accept() => {
                        let (client, _) = accepted.unwrap();
                        let gate = gate.clone();
                        connections.spawn(async move {
                            if let Ok(server) = tokio::net::TcpStream::connect(("127.0.0.1", upstream)).await {
                                let (cr, cw) = client.into_split();
                                let (sr, sw) = server.into_split();
                                tokio::select! {
                                    _ = pump(cr, sw, gate.clone()) => {},
                                    _ = pump(sr, cw, gate) => {},
                                }
                            }
                        });
                    },
                    _ = connections.join_next(), if !connections.is_empty() => {},
                }
            }
        });
        Self { port, paused, task }
    }
    pub fn pause(&self) {
        self.paused.send_replace(true);
    }
    pub fn resume(&self) {
        self.paused.send_replace(false);
    }
}
impl Drop for Relay {
    fn drop(&mut self) {
        self.task.abort();
    }
}
async fn pump(
    mut reader: tokio::net::tcp::OwnedReadHalf,
    mut writer: tokio::net::tcp::OwnedWriteHalf,
    mut gate: tokio::sync::watch::Receiver<bool>,
) -> std::io::Result<()> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut buffer = [0u8; 8192];
    loop {
        while *gate.borrow() {
            if gate.changed().await.is_err() {
                return Ok(());
            }
        }
        let n = tokio::select! {
            changed = gate.changed() => { if changed.is_err() {return Ok(());} continue; },
            n = reader.read(&mut buffer) => n?,
        };
        if n == 0 {
            return Ok(());
        }
        while *gate.borrow() {
            if gate.changed().await.is_err() {
                return Ok(());
            }
        }
        writer.write_all(&buffer[..n]).await?;
    }
}

#[tokio::test]
async fn relay_holds_open_stream_and_resumes_without_losing_bytes() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let upstream = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let relay = Relay::start(upstream.local_addr().unwrap().port()).await;
    let echo = tokio::spawn(async move {
        let (mut stream, _) = upstream.accept().await.unwrap();
        let mut buffer = [0; 512];
        loop {
            let n = stream.read(&mut buffer).await.unwrap();
            if n == 0 {
                break;
            }
            stream.write_all(&buffer[..n]).await.unwrap();
        }
    });
    let mut client = tokio::net::TcpStream::connect(("127.0.0.1", relay.port))
        .await
        .unwrap();
    client.write_all(b"warm").await.unwrap();
    let mut warm = [0; 4];
    client.read_exact(&mut warm).await.unwrap();
    assert_eq!(&warm, b"warm");
    relay.pause();
    let payload = [42; 512];
    client.write_all(&payload).await.unwrap();
    let mut received = [0; 512];
    assert!(
        tokio::time::timeout(
            std::time::Duration::from_millis(100),
            client.read_exact(&mut received)
        )
        .await
        .is_err(),
        "paused relay must neither return bytes nor close the stream"
    );
    relay.resume();
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        client.read_exact(&mut received),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(received, payload);
    drop(client);
    tokio::time::timeout(std::time::Duration::from_secs(2), echo)
        .await
        .unwrap()
        .unwrap();
}
