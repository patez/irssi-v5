use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use dashmap::DashMap;
use rand::Rng;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tracing::info;

pub struct Manager {
    socket_path: PathBuf,
    sessions_dir: PathBuf,
    soju_addr: String,
    irc_server: String,
    irc_port: u16,
    /// Tracks users provisioned in this process run (avoids redundant calls)
    provisioned: Arc<DashMap<String, ()>>,
}

impl Manager {
    pub fn new(
        socket_path: PathBuf,
        sessions_dir: PathBuf,
        soju_addr: String,
        irc_server: String,
        irc_port: u16,
    ) -> Arc<Self> {
        Arc::new(Self {
            socket_path,
            sessions_dir,
            soju_addr,
            irc_server,
            irc_port,
            provisioned: Arc::new(DashMap::new()),
        })
    }

    /// Ensure a soju account and irssi config exist for this user.
    /// Idempotent — safe to call on every login.
    pub async fn ensure_user(&self, username: &str) -> Result<()> {
        if self.provisioned.contains_key(username) {
            return Ok(());
        }

        let user_dir = self.sessions_dir.join(username);
        let config_path = user_dir.join("irssi.conf");

        // Already provisioned from a previous run
        if config_path.exists() {
            self.provisioned.insert(username.to_string(), ());
            return Ok(());
        }

        let password = random_password();

        // Create soju user via admin unix socket
        let result = self
            .bouncer_serv(&format!(
                "user create -username {} -password {}",
                username, password
            ))
            .await;

        if let Err(e) = result {
            if !e.to_string().contains("already exists") {
                return Err(e).context("soju user create failed");
            }
        }

        // Add upstream IRC network
        let network_name = self.irc_server.replace('.', "-");
        let irc_addr = format!("ircs://{}:{}", self.irc_server, self.irc_port);

        let result = self
            .bouncer_serv(&format!(
                "network create -user {} -name {} -addr {} -nick {}",
                username, network_name, irc_addr, username
            ))
            .await;

        if let Err(e) = result {
            if !e.to_string().contains("already exists") {
                return Err(e).context("soju network create failed");
            }
        }

        // Write irssi config
        tokio::fs::create_dir_all(&user_dir)
            .await
            .context("failed to create user dir")?;

        let (soju_host, soju_port) = split_addr(&self.soju_addr);
        let irssi_conf = format!(
            r#"servers = ({{
  address = "{soju_host}";
  port = {soju_port};
  use_ssl = no;
  password = "{username}/{network_name}:{password}";
  autoconnect = yes;
}});

settings = {{
  core = {{
    real_name = "{username}";
    user_name = "{username}";
    nick = "{username}";
  }};
  "fe-text" = {{ term_charset = "UTF-8"; }};
  "fe-common/core" = {{ term_charset = "UTF-8"; }};
}};
"#,
        );

        tokio::fs::write(&config_path, irssi_conf)
            .await
            .context("failed to write irssi config")?;

        info!("Provisioned soju user: {}", username);
        self.provisioned.insert(username.to_string(), ());
        Ok(())
    }

    pub fn user_dir(&self, username: &str) -> PathBuf {
        self.sessions_dir.join(username)
    }

    pub async fn delete_user(&self, username: &str) -> Result<()> {
        self.provisioned.remove(username);

        let _ = self
            .bouncer_serv(&format!("user delete {}", username))
            .await;

        let user_dir = self.sessions_dir.join(username);
        if user_dir.exists() {
            tokio::fs::remove_dir_all(&user_dir)
                .await
                .context("failed to remove user dir")?;
        }
        Ok(())
    }

    /// Send a BouncerServ command via the soju admin unix socket.
    /// Connects as an anonymous admin client, sends the command, reads the response.
    async fn bouncer_serv(&self, cmd: &str) -> Result<()> {
        let stream = UnixStream::connect(&self.socket_path)
            .await
            .with_context(|| format!("failed to connect to soju socket {:?}", self.socket_path))?;

        let (reader, mut writer) = stream.into_split();
        let mut reader = BufReader::new(reader);

        // IRC handshake — admin socket accepts any nick/user
        writer
            .write_all(b"NICK soju-admin\r\nUSER soju-admin 0 * :soju-admin\r\n")
            .await
            .context("failed to send IRC handshake")?;

        // Wait for 001 (welcome) before sending commands
        let mut line = String::new();
        loop {
            line.clear();
            let n = reader.read_line(&mut line).await.context("read error")?;
            if n == 0 {
                return Err(anyhow!("soju socket closed before welcome"));
            }
            if line.contains("001") {
                break;
            }
            if line.starts_with("ERROR") {
                return Err(anyhow!("soju socket error: {}", line.trim()));
            }
        }

        // Send the BouncerServ command
        let msg = format!("PRIVMSG BouncerServ :{}\r\n", cmd);
        writer
            .write_all(msg.as_bytes())
            .await
            .context("failed to send BouncerServ command")?;

        // Read response — look for a NOTICE from BouncerServ
        let mut response = String::new();
        loop {
            response.clear();
            let n = reader
                .read_line(&mut response)
                .await
                .context("read error waiting for response")?;
            if n == 0 {
                break;
            }
            let r = response.trim();
            if r.contains("NOTICE") && r.contains("BouncerServ") {
                if r.to_lowercase().contains("error")
                    || r.to_lowercase().contains("unknown")
                    || r.to_lowercase().contains("failed")
                {
                    return Err(anyhow!("BouncerServ error: {}", r));
                }
                break;
            }
            if r.starts_with("PING") {
                let pong = format!("PONG {}\r\n", &r[5..]);
                writer.write_all(pong.as_bytes()).await.ok();
            }
        }

        writer.write_all(b"QUIT\r\n").await.ok();
        Ok(())
    }
}

fn random_password() -> String {
    let bytes: Vec<u8> = (0..16).map(|_| rand::thread_rng().gen()).collect();
    hex::encode(bytes)
}

fn split_addr(addr: &str) -> (&str, &str) {
    match addr.rsplit_once(':') {
        Some((host, port)) => (host, port),
        None => (addr, "6667"),
    }
}