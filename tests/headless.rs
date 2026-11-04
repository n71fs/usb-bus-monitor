#![cfg(feature = "web")]
//! End-to-end headless lifecycle: no DISPLAY, real HTTP, fake sysfs, SIGTERM.
use serde_json::Value;
use std::{
    fs,
    io::{BufRead, BufReader, Read, Write},
    net::TcpStream,
    os::unix::fs::symlink,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

struct Server(Child);
impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn request(port: u16, path: &str) -> String {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    write!(
        stream,
        "GET {path} HTTP/1.0\r\nHost: localhost:{port}\r\nConnection: close\r\n\r\n"
    )
    .unwrap();
    let mut out = String::new();
    stream.read_to_string(&mut out).unwrap();
    out
}
fn state(port: u16) -> Value {
    let out = request(port, "/api/state");
    assert!(
        out.starts_with("HTTP/1.0 200") || out.starts_with("HTTP/1.1 200"),
        "{out}"
    );
    serde_json::from_str(out.split_once("\r\n\r\n").unwrap().1).unwrap()
}
fn wait_for(port: u16, check: impl Fn(&Value) -> bool) -> Value {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let v = state(port);
        if check(&v) {
            return v;
        }
        assert!(Instant::now() < deadline, "condition timed out: {v}");
        std::thread::sleep(Duration::from_millis(100));
    }
}
#[test]
fn headless_http_samples_without_clients_and_exits_on_sigterm() {
    let root = std::env::temp_dir().join(format!("usbmon-web-{}", std::process::id()));
    fs::create_dir_all(root.join("bus/usb/devices")).unwrap();
    let pci = root.join("devices/pci0000:00/0000:00:14.0");
    let attrs = |p: &std::path::Path, values: &[(&str, &str)]| {
        fs::create_dir_all(p).unwrap();
        for (k, v) in values {
            fs::write(p.join(k), v).unwrap();
        }
    };
    attrs(&pci, &[("vendor", "0x8086"), ("device", "0x1234")]);
    attrs(
        &pci.join("usb1"),
        &[
            ("busnum", "1"),
            ("devnum", "1"),
            ("speed", "480"),
            ("bDeviceClass", "09"),
        ],
    );
    attrs(
        &pci.join("usb1/1-2"),
        &[
            ("busnum", "1"),
            ("devnum", "2"),
            ("speed", "480"),
            ("idVendor", "0bda"),
            ("idProduct", "2838"),
            ("product", "<img src=x onerror=alert(1)>"),
            ("version", "2.00"),
        ],
    );
    symlink(pci.join("usb1"), root.join("bus/usb/devices/usb1")).unwrap();
    symlink(pci.join("usb1/1-2"), root.join("bus/usb/devices/1-2")).unwrap();
    let mut server = Server(
        Command::new(env!("CARGO_BIN_EXE_usb-monitor"))
            .args(["--headless", "--demo-traffic", "--port", "0", "--sys-root"])
            .arg(&root)
            .env_remove("DISPLAY")
            .env_remove("WAYLAND_DISPLAY")
            .env("XDG_STATE_HOME", root.join("state"))
            .stderr(Stdio::piped())
            .stdout(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let mut line = String::new();
    BufReader::new(server.0.stderr.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    assert!(
        line.contains("http://127.0.0.1:"),
        "default must be loopback: {line}"
    );
    let port = line
        .split("127.0.0.1:")
        .nth(1)
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap()
        .parse()
        .unwrap();
    // Don't connect while it starts sampling: monitoring is independent of HTTP.
    std::thread::sleep(Duration::from_millis(3300));
    let v = wait_for(port, |v| {
        v["controllers"][0]["buses"][0]["devices"][0]["rate"]["now"]
            .as_f64()
            .unwrap_or(0.0)
            > 0.0
    });
    assert!(v["generation"].as_u64().unwrap() >= 3);
    assert_eq!(v["traffic_state"], "Running");
    let d = &v["controllers"][0]["buses"][0]["devices"][0];
    assert_eq!(d["sdr"], "RTL-SDR");
    assert_eq!(d["name"], "<img src=x onerror=alert(1)>");
    assert!(d["rate"]["history"].as_array().unwrap().len() >= 2);
    assert!(request(port, "/").contains("USB topology"));
    fs::remove_file(root.join("bus/usb/devices/1-2")).unwrap();
    let v = wait_for(port, |v| {
        v["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["text"].as_str().unwrap().starts_with("Disconnected:"))
    });
    assert!(
        v["controllers"][0]["buses"][0]["devices"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let log = fs::read_to_string(root.join("state/usb-monitor/events.log")).unwrap();
    assert!(log.contains("Disconnected:"));
    unsafe {
        libc::kill(server.0.id() as i32, libc::SIGTERM);
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(status) = server.0.try_wait().unwrap() {
            assert!(status.success(), "{status}");
            break;
        }
        assert!(Instant::now() < deadline, "SIGTERM did not stop server");
        std::thread::sleep(Duration::from_millis(50));
    }
    fs::remove_dir_all(&root).unwrap();
}
#[test]
fn invalid_options_and_bind_conflicts_fail() {
    for args in [
        vec!["--bind", "127.0.0.1"],
        vec!["--headless", "--port", "65536"],
        vec!["--headless", "--bind", "invalid"],
        vec!["--headless", "--dump"],
        vec!["--allow-host", "monitor.example.test"],
        vec!["--headless", "--allow-host", "*"],
        vec!["--headless", "--allow-host", "localhost:bad"],
    ] {
        assert!(
            !Command::new(env!("CARGO_BIN_EXE_usb-monitor"))
                .args(args)
                .output()
                .unwrap()
                .status
                .success()
        );
    }
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_usb-monitor"))
        .args([
            "--headless",
            "--port",
            &listener.local_addr().unwrap().port().to_string(),
        ])
        .output()
        .unwrap();
    assert!(!out.status.success());
}

#[test]
fn degraded_mode_and_journal_child_shutdown() {
    use std::os::unix::fs::PermissionsExt;
    let root = std::env::temp_dir().join(format!("usbmon-journal-{}", std::process::id()));
    fs::create_dir_all(root.join("bin")).unwrap();
    // A controlled journal follower verifies that SIGTERM reaps the subprocess.
    let script = root.join("bin/journalctl");
    fs::write(
        &script,
        format!(
            "#!/bin/sh\necho $$ > {}/journal.pid\nexec /bin/sleep 120\n",
            root.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    let mut server = Server(
        Command::new(env!("CARGO_BIN_EXE_usb-monitor"))
            .args(["--headless", "--port", "0", "--sys-root"])
            .arg(root.join("missing-sysfs"))
            .env("PATH", root.join("bin"))
            .env("XDG_STATE_HOME", root.join("state"))
            .stderr(Stdio::piped())
            .stdout(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let mut line = String::new();
    BufReader::new(server.0.stderr.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    let port = line
        .split("127.0.0.1:")
        .nth(1)
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap()
        .parse()
        .unwrap();
    let v = wait_for(port, |v| {
        v["journal_running"] == true && v["traffic_state"] != "Starting"
    });
    assert!(v["scan_error"].as_str().unwrap().contains("Could not read"));
    assert_eq!(v["traffic_state"], "NotLoaded");
    let deadline = Instant::now() + Duration::from_secs(3);
    while !root.join("journal.pid").exists() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(50));
    }
    let pid: i32 = fs::read_to_string(root.join("journal.pid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    unsafe {
        libc::kill(server.0.id() as i32, libc::SIGTERM);
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(status) = server.0.try_wait().unwrap() {
            assert!(status.success());
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(
        unsafe { libc::kill(pid, 0) },
        -1,
        "journal follower survived server shutdown"
    );
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ESRCH)
    );
    fs::remove_dir_all(root).unwrap();
}
