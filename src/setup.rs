//! The one command a person runs on their own server: it asks the little it can't work out, then
//! does the rest and ends on the QR for pstd and the line for Claude. Linux servers with systemd;
//! elsewhere it says so and stops.

use crate::store::{Config, Store};
use crate::DEFAULT_BIND;
use qrcode::render::unicode;
use qrcode::QrCode;
use std::io::{self, BufRead, ErrorKind, Write};
use std::net::TcpListener;
use std::process::Command;
use std::time::Duration;

const SERVICE: &str = "pstd-bridge";
const UNIT: &str = "/etc/systemd/system/pstd-bridge.service";
const INSTALLED: &str = "/usr/local/bin/pstd-bridge";
/// a first certificate can take this long
const WAIT_SECONDS: u64 = 90;
/// behind a website: the address under the site's own domain, so the site itself stays as it is
const SITE_PATH: &str = "/pstd-bridge";
const LOCAL_PORT: u16 = 8787;

pub fn is_root() -> bool {
    cfg!(target_os = "linux")
        && Command::new("id").arg("-u").output().is_ok_and(|o| String::from_utf8_lossy(&o.stdout).trim() == "0")
}

fn ask(question: &str) -> io::Result<String> {
    print!("{question} ");
    io::stdout().flush()?;
    let mut line = String::new();
    io::stdin().lock().read_line(&mut line)?;
    Ok(line.trim().to_string())
}

fn step(text: &str) {
    println!("\n▸ {text}");
}

fn run_quiet(program: &str, args: &[&str]) -> bool {
    Command::new(program).args(args).output().is_ok_and(|o| o.status.success())
}

fn must(program: &str, args: &[&str]) -> io::Result<()> {
    let out = Command::new(program).args(args).output()?;
    if out.status.success() {
        return Ok(());
    }
    Err(io::Error::other(format!("{program} {}: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim())))
}

fn encoded(text: &str) -> String {
    text.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

pub fn pair_link(config: &Config) -> String {
    format!("pstd://connect?url={}&token={}&claude={}", encoded(&config.url), config.pstd_token, config.claude_token)
}

pub fn claude_line(config: &Config) -> String {
    format!(
        "claude mcp add --transport http --scope user pstd {}/mcp --header \"Authorization: Bearer {}\"",
        config.url, config.claude_token
    )
}

/// the hostname without a domain of one's own: sslip.io answers with the IP written in the name
fn free_hostname(ip: &str) -> String {
    format!("{}.sslip.io", ip.replace('.', "-"))
}

async fn public_ip() -> Option<String> {
    let reply = reqwest::Client::new().get("https://api.ipify.org").timeout(Duration::from_secs(10)).send().await.ok()?;
    let ip = reply.text().await.ok()?;
    ip.parse::<std::net::Ipv4Addr>().is_ok().then_some(ip)
}

fn port_taken(port: u16) -> bool {
    matches!(TcpListener::bind(("0.0.0.0", port)), Err(e) if e.kind() == ErrorKind::AddrInUse)
}

fn unit(user: &str, home: &str) -> String {
    format!(
        "[Unit]
Description=pstd bridge
After=network-online.target
Wants=network-online.target

[Service]
ExecStart={INSTALLED} serve
Environment=PSTD_BRIDGE_HOME={home}
User={user}
AmbientCapabilities=CAP_NET_BIND_SERVICE
Restart=always
RestartSec=3
NoNewPrivileges=true
ProtectSystem=strict
ReadWritePaths={home}

[Install]
WantedBy=multi-user.target
"
    )
}

fn install_service(store: &Store) -> io::Result<()> {
    let binary = std::env::current_exe()?;
    if binary.to_string_lossy() != INSTALLED {
        std::fs::copy(&binary, INSTALLED)?;
    }
    if !run_quiet("id", &[SERVICE]) {
        must("useradd", &["--system", "--no-create-home", "--shell", "/usr/sbin/nologin", SERVICE])?;
    }
    let home = store.root().to_string_lossy().to_string();
    must("chown", &["-R", &format!("{SERVICE}:{SERVICE}"), &home])?;
    std::fs::write(UNIT, unit(SERVICE, &home))?;
    must("systemctl", &["daemon-reload"])?;
    must("systemctl", &["enable", SERVICE])?;
    must("systemctl", &["restart", SERVICE])
}

fn open_firewall() {
    let active = Command::new("ufw").arg("status").output().is_ok_and(|o| String::from_utf8_lossy(&o.stdout).contains("Status: active"));
    if active && run_quiet("ufw", &["allow", "443/tcp"]) {
        println!("  port 443 opened in ufw");
    }
}

async fn wait_until_up(url: &str, token: &str) -> bool {
    let client = reqwest::Client::new();
    for _ in 0..(WAIT_SECONDS / 3) {
        let reply = client.get(format!("{url}/api/health")).bearer_auth(token).timeout(Duration::from_secs(3)).send().await;
        if reply.is_ok_and(|r| r.status().is_success()) {
            return true;
        }
        tokio::time::sleep(Duration::from_secs(3)).await;
    }
    false
}

pub fn site_url(host: &str) -> String {
    format!("https://{host}{SITE_PATH}")
}

fn behind_proxy_hint(host: &str) {
    println!(
        "
  Add this to your website's setup, for {host}. Only this path changes,
  the rest of the site stays as it is.

  nginx, inside the server block that listens on 443:
    location {SITE_PATH}/ {{
      proxy_pass http://{DEFAULT_BIND}/;
      proxy_read_timeout 200s;
      client_max_body_size 8m;
    }}
  then: nginx -t && systemctl reload nginx

  Caddy, inside the site's block:
    handle_path {SITE_PATH}/* {{
      reverse_proxy {DEFAULT_BIND}
    }}
  then: systemctl reload caddy

  Apache, inside the VirtualHost on 443, with mod_proxy_http on:
    ProxyPass {SITE_PATH}/ http://{DEFAULT_BIND}/ timeout=200
  then: systemctl reload apache2
"
    );
}

/// `auto`: no questions, for the line pstd builds. `domain`: the answer to the one that matters
pub async fn run(store: &Store, domain: Option<String>, auto: bool) -> io::Result<()> {
    if !cfg!(target_os = "linux") || Command::new("systemctl").arg("--version").output().is_err() {
        return Err(io::Error::other("setup is for a Linux server with systemd"));
    }
    if !is_root() {
        return Err(io::Error::other("run it as root: sudo pstd-bridge setup"));
    }
    println!("pstd bridge setup\n");
    let mut config = store.config()?;
    // running it again: our own service must not read as somebody else holding the ports
    run_quiet("systemctl", &["stop", SERVICE]);
    if port_taken(LOCAL_PORT) {
        return Err(io::Error::other(format!("port {LOCAL_PORT} is taken by something else on this server")));
    }

    step("Address");
    let proxied = port_taken(443);
    let question = if proxied {
        println!("  A website already runs here. It stays as it is: pstd gets a path under its domain.");
        "Your website's domain:"
    } else {
        "Your domain for this server, or Enter if you don't have one:"
    };
    let mut host = match domain {
        Some(domain) => domain,
        None if auto => String::new(),
        None => ask(question).or_skip(),
    };
    if host.is_empty() {
        if proxied {
            return Err(io::Error::other("a website's domain is needed here"));
        }
        let ip = public_ip().await.ok_or_else(|| io::Error::other("can't find this server's public IP"))?;
        host = free_hostname(&ip);
        println!("  using {host}");
    }
    config.url = if proxied { site_url(&host) } else { format!("https://{host}") };
    config.tls_host = if proxied { String::new() } else { host.clone() };
    config.bind = DEFAULT_BIND.into();

    if !auto {
        step("Pictures");
        println!("  The key of your image generator (OpenAI). You can also add it in pstd later.");
        let image_key = ask("Paste it, or press Enter to skip:").or_skip();
        if !image_key.is_empty() {
            config.image_key = image_key;
        }
    }
    store.save_config(&config)?;

    step("Installing the service");
    install_service(store)?;
    if proxied {
        behind_proxy_hint(&host);
        ask("Press Enter when that's done:").or_skip();
    } else {
        open_firewall();
    }

    step("Checking it from outside");
    if !wait_until_up(&config.url, &config.pstd_token).await {
        println!("  {} doesn't answer yet. Check that port 443 is open at your provider,", config.url);
        println!("  then look at: journalctl -u {SERVICE} -n 30");
        return Err(io::Error::other("the server is not reachable"));
    }
    println!("  it answers at {}", config.url);
    connect(store)
}

/// what `ask` gives up on stdin closed: empty, as pressing Enter
trait Answer {
    fn or_skip(self) -> String;
}

impl Answer for io::Result<String> {
    fn or_skip(self) -> String {
        self.unwrap_or_default()
    }
}

pub fn connect(store: &Store) -> io::Result<()> {
    let config = store.config()?;
    if config.url.is_empty() {
        return Err(io::Error::other("run setup first"));
    }
    let code = QrCode::new(pair_link(&config)).map_err(io::Error::other)?;
    let picture = code.render::<unicode::Dense1x2>().quiet_zone(true).build();
    println!("\n{picture}\nScan this with your iPhone's camera. pstd shows the next steps.\n");
    println!("Without pstd at hand, for Claude Code:\n\n{}\n", claude_line(&config));
    Ok(())
}

pub fn key(store: &Store) -> io::Result<()> {
    let mut config = store.config()?;
    config.image_key = ask("Paste the image generator's key:")?;
    store.save_config(&config)?;
    if is_root() {
        run_quiet("chown", &[&format!("{SERVICE}:{SERVICE}"), &store.root().join("config.json").to_string_lossy()]);
    }
    println!("saved");
    Ok(())
}
