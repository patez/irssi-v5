use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use dashmap::DashMap;
use tokio::net::TcpStream;
use tokio::process::{Child, Command};
use tokio::sync::Mutex;
use tokio::time::{sleep, timeout};
use tracing::{info, warn};

pub struct Session {
    pub port: u16,
    // Keep alive — dropping kills ttyd.
    // With dtach, irssi lives on in its socket after ttyd exits.
    _child: Child,
}

pub struct Manager {
    sessions: Arc<DashMap<String, Arc<Mutex<Session>>>>,
    port_pool: Arc<Mutex<PortPool>>,
    ttyd_index: String,
    dtach_session: bool,
}

struct PortPool {
    base: u16,
    used: HashMap<u16, bool>,
}

impl PortPool {
    fn new(base: u16) -> Self {
        Self { base, used: HashMap::new() }
    }

    fn alloc(&mut self) -> Result<u16> {
        for p in self.base..self.base + 1000 {
            if !self.used.get(&p).copied().unwrap_or(false) {
                self.used.insert(p, true);
                return Ok(p);
            }
        }
        Err(anyhow!("no free ports in range {}–{}", self.base, self.base + 999))
    }

    fn free(&mut self, port: u16) {
        self.used.insert(port, false);
    }
}

impl Manager {
    pub fn new(base_port: u16, dtach_session: bool, ttyd_index: PathBuf) -> Arc<Self> {
        Arc::new(Self {
            sessions: Arc::new(DashMap::new()),
            port_pool: Arc::new(Mutex::new(PortPool::new(base_port))),
            ttyd_index: ttyd_index.to_str().unwrap_or("./public/ttyd.html").to_owned(),
            dtach_session,
        })
    }

    pub async fn get_or_create(
        self: &Arc<Self>,
        username: &str,
        user_dir: &Path,
    ) -> Result<u16> {
        if let Some(entry) = self.sessions.get(username) {
            let sess = entry.lock().await;
            return Ok(sess.port);
        }

        let port = self.port_pool.lock().await.alloc()?;

        let abs_user_dir = std::fs::canonicalize(user_dir)
            .unwrap_or_else(|_| user_dir.to_path_buf());
        let home_str = abs_user_dir.to_str().unwrap_or("/tmp").to_owned();
        let config_path = format!("{}/config", home_str);

        let child = if self.dtach_session {
            let sock = format!("/tmp/irc-{}.sock", username);
            info!("spawning ttyd+dtach for {} on port {} sock {}", username, port, sock);

            Command::new("ttyd")
                .args([
                    "--port", &port.to_string(),
                    "--interface", "127.0.0.1",
                    "--index", &self.ttyd_index,
                    "--writable",
                    "dtach", "-A", &sock,
                    "irssi", "--config", &config_path,
                ])
                .kill_on_drop(true)
                .spawn()
                .with_context(|| format!("failed to spawn ttyd+dtach for {}", username))?
        } else {
            info!("spawning ttyd for {} on port {}", username, port);

            Command::new("ttyd")
                .args([
                    "--port", &port.to_string(),
                    "--interface", "127.0.0.1",
                    "--index", &self.ttyd_index,
                    "--writable",
                    "irssi", "--config", &config_path,
                ])
                .kill_on_drop(true)
                .spawn()
                .with_context(|| format!("failed to spawn ttyd for {}", username))?
        };

        wait_for_port(port, Duration::from_secs(5))
            .await
            .with_context(|| format!("ttyd did not start in time for {}", username))?;

        info!("ttyd started for {} on port {}", username, port);

        let session = Arc::new(Mutex::new(Session { port, _child: child }));
        self.sessions.insert(username.to_string(), session);

        let sessions = Arc::clone(&self.sessions);
        let pool = Arc::clone(&self.port_pool);
        let username_owned = username.to_string();

        tokio::spawn(async move {
            loop {
                sleep(Duration::from_secs(5)).await;
                match sessions.get(&username_owned) {
                    None => break,
                    Some(e) => {
                        if let Ok(mut sess) = e.try_lock() {
                            if let Ok(Some(_)) = sess._child.try_wait() {
                                drop(sess);
                                sessions.remove(&username_owned);
                                pool.lock().await.free(port);
                                info!("ttyd exited for {} (port {})", username_owned, port);
                                break;
                            }
                        }
                    }
                }
            }
        });

        Ok(port)
    }

    pub fn kill(&self, username: &str) {
        if self.sessions.remove(username).is_some() {
            info!("killed ttyd session for {}", username);
        }

        if self.dtach_session {
            let sock = format!("/tmp/irc-{}.sock", username);
            match std::fs::remove_file(&sock) {
                Ok(_) => info!("removed dtach socket {} for {}", sock, username),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => warn!("failed to remove dtach socket {} for {}: {}", sock, username, e),
            }
        }
    }

    pub fn is_active(&self, username: &str) -> bool {
        self.sessions.contains_key(username)
    }

    pub fn active_count(&self) -> usize {
        self.sessions.len()
    }

    pub fn active_usernames(&self) -> Vec<String> {
        self.sessions.iter().map(|e| e.key().clone()).collect()
    }
}

async fn wait_for_port(port: u16, max_wait: Duration) -> Result<()> {
    let addr = format!("127.0.0.1:{}", port);
    timeout(max_wait, async {
        loop {
            if TcpStream::connect(&addr).await.is_ok() { return; }
            sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .map_err(|_| anyhow!("port {} not ready after {:?}", port, max_wait))
}