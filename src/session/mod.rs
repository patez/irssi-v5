use std::collections::HashMap;
use std::path::Path;
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
    // Keep child alive — dropping it would kill the process
    _child: Child,
}

pub struct Manager {
    sessions: Arc<DashMap<String, Arc<Mutex<Session>>>>,
    port_pool: Arc<Mutex<PortPool>>,
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
    pub fn new(base_port: u16) -> Arc<Self> {
        Arc::new(Self {
            sessions: Arc::new(DashMap::new()),
            port_pool: Arc::new(Mutex::new(PortPool::new(base_port))),
        })
    }

    /// Return an existing session or spawn a new ttyd for this user.
    pub async fn get_or_create(
        self: &Arc<Self>,
        username: &str,
        user_dir: &Path,
    ) -> Result<u16> {
        // Return existing port if session is still alive
        if let Some(entry) = self.sessions.get(username) {
            let sess = entry.lock().await;
            return Ok(sess.port);
        }

        let port = self.port_pool.lock().await.alloc()?;

        // Canonicalize to absolute path — relative paths break when the working
        // directory doesn't match the project root
        let abs_user_dir = std::fs::canonicalize(user_dir)
            .unwrap_or_else(|_| user_dir.to_path_buf());
        let home_str = abs_user_dir.to_str().unwrap_or("/tmp").to_owned();

        info!("spawning ttyd for {} on port {} --home {}", username, port, home_str);

        let child = Command::new("ttyd")
            .args([
                "--port", &port.to_string(),
                "--interface", "127.0.0.1",
                "--once",
                "--writable",
                "irssi", "--home", &home_str,
            ])
            .kill_on_drop(true)
            .spawn()
            .with_context(|| format!("failed to spawn ttyd for {}", username))?;

        // Wait for ttyd to start accepting connections
        wait_for_port(port, Duration::from_secs(5))
            .await
            .with_context(|| format!("ttyd did not start in time for {}", username))?;

        info!("ttyd started for {} on port {}", username, port);

        let session = Arc::new(Mutex::new(Session { port, _child: child }));
        self.sessions.insert(username.to_string(), session);

        // Reap when ttyd exits
        let sessions = Arc::clone(&self.sessions);
        let pool = Arc::clone(&self.port_pool);
        let username_owned = username.to_string();

        tokio::spawn(async move {
            // Poll until session is gone from the map or child exits
            loop {
                sleep(Duration::from_secs(5)).await;
                let entry = sessions.get(&username_owned);
                match entry {
                    None => break,
                    Some(e) => {
                        // try_lock: if locked, session is actively in use
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
        // Removing from the map drops the Arc<Mutex<Session>>,
        // and since _child has kill_on_drop(true), the process is killed.
        if self.sessions.remove(username).is_some() {
            info!("killed session for {}", username);
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
    let deadline = timeout(max_wait, async {
        loop {
            if TcpStream::connect(&addr).await.is_ok() {
                return;
            }
            sleep(Duration::from_millis(100)).await;
        }
    });

    deadline.await.map_err(|_| anyhow!("port {} not ready after {:?}", port, max_wait))
}