//! Read-only HTTP interface. Monitoring never depends on connected browsers.
use crate::{
    engine::{Engine, TICK},
    ids::IdDb,
    names::Names,
    rates::{Rates, Series, usable_bytes_per_sec},
    sysfs::Device,
};
use axum::{
    Json, Router,
    extract::State,
    http::{HeaderValue, StatusCode},
    middleware::{self, Next},
    response::{Html, IntoResponse, Response},
    routing::get,
};
use serde_json::{Value, json};
use std::{
    net::SocketAddr,
    path::PathBuf,
    sync::{Arc, RwLock},
};

type Snapshot = Arc<RwLock<Value>>;

/// Exact HTTP authorities, never DNS-resolved: DNS rebinding must not turn an
/// arbitrary hostname into a trusted loopback origin. Ports are significant.
#[derive(Clone)]
struct HostPolicy(Vec<String>);

fn authority(value: &str) -> Result<String, String> {
    let parsed = value
        .parse::<axum::http::uri::Authority>()
        .map_err(|_| format!("invalid allowed host: {value}"))?;
    let host = parsed.host();
    let ip = host
        .trim_start_matches('[')
        .trim_end_matches(']')
        .parse::<std::net::IpAddr>();
    let normalized = if let Ok(ip) = ip {
        match ip {
            std::net::IpAddr::V4(ip) => ip.to_string(),
            std::net::IpAddr::V6(ip) => format!("[{ip}]"),
        }
    } else {
        if host.len() > 253
            || !host.split('.').all(|label| {
                !label.is_empty()
                    && label.len() <= 63
                    && label
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'-')
                    && !label.starts_with('-')
                    && !label.ends_with('-')
            })
        {
            return Err(format!("invalid allowed host: {value}"));
        }
        host.to_ascii_lowercase()
    };
    // Authority's port accessors treat a nonnumeric suffix as absent. Parse
    // the complete suffix ourselves so malformed Host values cannot alias a
    // trusted authority without a port.
    let invalid = || format!("invalid allowed host: {value}");
    let suffix = value.strip_prefix(host).ok_or_else(invalid)?;
    let port = if suffix.is_empty() {
        None
    } else {
        let text = suffix.strip_prefix(':').ok_or_else(invalid)?;
        if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
            return Err(invalid());
        }
        Some(text.parse::<u16>().map_err(|_| invalid())?)
    };
    Ok(match port {
        Some(port) => format!("{normalized}:{port}"),
        None => normalized,
    })
}

impl HostPolicy {
    fn new(address: SocketAddr, extra: &[String]) -> Self {
        let mut hosts = extra.to_vec();
        if !address.ip().is_unspecified() {
            hosts.push(address.to_string());
        }
        if address.ip().is_loopback() {
            hosts.push(format!("localhost:{}", address.port()));
        }
        Self(hosts)
    }

    fn accepts(&self, request: &axum::extract::Request) -> bool {
        let mut values = request.headers().get_all("host").iter();
        let Some(host) = values
            .next()
            .and_then(|v| v.to_str().ok())
            .and_then(|v| authority(v).ok())
        else {
            return false;
        };
        if values.next().is_some() || !self.0.contains(&host) {
            return false;
        }
        // Do not accept conflicting absolute-form request targets either.
        request
            .uri()
            .authority()
            .is_none_or(|a| authority(a.as_str()).ok().as_ref() == Some(&host))
    }
}

async fn require_host(
    State(hosts): State<HostPolicy>,
    request: axum::extract::Request,
    next: Next,
) -> Response {
    if !hosts.accepts(&request) {
        return (StatusCode::FORBIDDEN, "Untrusted Host header").into_response();
    }
    // Forwarded and X-Forwarded-Host never grant trust.
    next.run(request).await
}

fn series(s: Option<&Series>) -> Value {
    json!({"now": s.map_or(0.0, Series::now), "history": s.map(|s| &s.history)})
}

fn device(d: &Device, rates: &Rates) -> Value {
    let errors = rates.errors(d);
    json!({
        "id": d.sysname, "name": d.display_name(), "bus": d.busnum, "address": d.devnum,
        "port": d.port_path(), "vendor_id": format!("{:04x}", d.vendor_id),
        "product_id": format!("{:04x}", d.product_id), "manufacturer": d.manufacturer,
        "serial": d.serial, "speed_mbps": d.speed_mbps, "usb_version": d.usb_version,
        "max_power": d.max_power, "drivers": d.drivers, "sdr": d.sdr, "hub": d.is_hub,
        "below_speed": d.below_capable_speed(), "rate": series(rates.device(d)),
        "errors": errors.map_or(0, |e| e.errors),
        "error_codes": errors.map(|e| e.error_codes.iter().map(|(code, count)|
            json!({"code": code, "name": crate::traffic::error_name(*code), "count": count})).collect::<Vec<_>>()).unwrap_or_default(),
        "children": d.children.iter().map(|c| device(c, rates)).collect::<Vec<_>>()
    })
}

fn snapshot(e: &Engine, names: &Names, demo: bool) -> Value {
    json!({
        "version": env!("CARGO_PKG_VERSION"), "generation": e.generation, "demo": demo,
        "time": crate::issues::now_string(), "board": e.topo.board, "scan_error": e.error,
        "traffic_state": format!("{:?}", e.monitor.state()),
        "journal_running": *e.klog.running.lock().unwrap(),
        "log_path": e.watcher.log_path, "usbfs_memory_mb": e.topo.usbfs_memory_mb,
        "lost_events": e.rates.latest.lost_events,
        "controllers": e.topo.controllers.iter().map(|c| json!({
            "id": format!("pci:{}", c.pci_addr), "pci": c.pci_addr,
            "name": names.get(&c.pci_addr).unwrap_or(&c.name), "kind": c.kind.label(),
            "driver": c.driver, "pcie_link": c.pcie_link, "sdr_count": c.sdrs().len(),
            "rate": series(e.rates.controllers.get(&c.pci_addr)),
            "buses": c.buses.iter().map(|b| json!({
                "id": b.busnum, "label": b.label(), "speed_mbps": b.speed_mbps(),
                "capacity": usable_bytes_per_sec(b.speed_mbps()), "rate": series(e.rates.buses.get(&b.busnum)),
                "devices": b.root.children.iter().map(|d| device(d, &e.rates)).collect::<Vec<_>>()
            })).collect::<Vec<_>>()
        })).collect::<Vec<_>>(),
        "issues": e.watcher.active.iter().map(|i| json!({"severity": format!("{:?}", i.severity), "text": i.text, "target": i.target})).collect::<Vec<_>>(),
        "events": e.watcher.events.iter().map(|i| json!({"time": i.time, "severity": format!("{:?}", i.severity), "text": i.text})).collect::<Vec<_>>()
    })
}

async fn headers(request: axum::extract::Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    for (key, value) in [
        ("cache-control", "no-store"),
        ("x-content-type-options", "nosniff"),
        ("referrer-policy", "no-referrer"),
        (
            "content-security-policy",
            "default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'none'",
        ),
    ] {
        response
            .headers_mut()
            .insert(key, HeaderValue::from_static(value));
    }
    response
}

fn router(state: Snapshot, hosts: HostPolicy) -> Router {
    Router::new()
        .route(
            "/",
            get(|| async { Html(include_str!("../web/index.html")) }),
        )
        .route(
            "/app.js",
            get(|| async {
                (
                    [("content-type", "text/javascript; charset=utf-8")],
                    include_str!("../web/app.js"),
                )
            }),
        )
        .route(
            "/style.css",
            get(|| async {
                (
                    [("content-type", "text/css; charset=utf-8")],
                    include_str!("../web/style.css"),
                )
            }),
        )
        .route(
            "/api/state",
            get(|State(state): State<Snapshot>| async move {
                state
                    .read()
                    .map(|v| Json(v.clone()))
                    .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)
            }),
        )
        .layer(middleware::from_fn_with_state(hosts, require_host))
        .layer(middleware::from_fn(headers))
        .with_state(state)
}

pub fn run(
    root: PathBuf,
    ids: IdDb,
    demo: bool,
    address: SocketAddr,
    allowed_hosts: Vec<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    let allowed_hosts = allowed_hosts
        .iter()
        .map(|h| authority(h))
        .collect::<Result<Vec<_>, _>>()?;
    tokio::runtime::Runtime::new()?.block_on(async move {
        // Bind first: failures must not start monitor threads or create event logs.
        let listener = tokio::net::TcpListener::bind(address).await?;
        let actual = listener.local_addr()?;
        eprintln!("USB Bus Monitor: http://{actual} (read-only, no built-in authentication)");
        if !address.ip().is_loopback() {
            eprintln!("Explicit network bind: restrict access with firewall/Tailscale ACLs or an authenticated HTTPS proxy. Do not forward this port publicly.");
        }
        let mut engine = Engine::new(root, ids, demo);
        let names = Names::load();
        let state = Arc::new(RwLock::new(snapshot(&engine, &names, demo)));
        let published = state.clone();
        let (stop, stopped) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            while matches!(stopped.recv_timeout(TICK), Err(std::sync::mpsc::RecvTimeoutError::Timeout)) {
                let before = engine.generation;
                engine.tick();
                if before != engine.generation {
                    *published.write().unwrap() = snapshot(&engine, &names, demo);
                }
            }
            engine.klog.stop();
        });
        let result = axum::serve(listener, router(state, HostPolicy::new(actual, &allowed_hosts))).with_graceful_shutdown(shutdown()).await;
        let _ = stop.send(());
        let _ = worker.join();
        result
    })?;
    Ok(())
}

async fn shutdown() {
    #[cfg(unix)]
    {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("SIGTERM handler");
        tokio::select! { _ = tokio::signal::ctrl_c() => {}, _ = term.recv() => {} }
    }
    #[cfg(not(unix))]
    let _ = tokio::signal::ctrl_c().await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::{Body, to_bytes},
        http::Request,
    };
    use tower::ServiceExt;

    #[tokio::test]
    async fn read_only_routes_and_security_headers() {
        let app = router(
            Arc::new(RwLock::new(json!({"generation": 7}))),
            HostPolicy::new("127.0.0.1:8080".parse().unwrap(), &[]),
        );
        for (path, content_type) in [
            ("/", "text/html"),
            ("/app.js", "text/javascript"),
            ("/style.css", "text/css"),
            ("/api/state", "application/json"),
        ] {
            let r = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(path)
                        .header("host", "localhost:8080")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(r.status(), StatusCode::OK);
            assert!(
                r.headers()["content-type"]
                    .to_str()
                    .unwrap()
                    .starts_with(content_type)
            );
            assert_eq!(r.headers()["cache-control"], "no-store");
            assert!(
                r.headers()["content-security-policy"]
                    .to_str()
                    .unwrap()
                    .contains("frame-ancestors 'none'")
            );
            if path == "/api/state" {
                let body = to_bytes(r.into_body(), 4096).await.unwrap();
                assert_eq!(
                    serde_json::from_slice::<Value>(&body).unwrap()["generation"],
                    7
                );
            }
        }
        let r = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/state")
                    .header("host", "localhost:8080")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::METHOD_NOT_ALLOWED);
        let r = app
            .oneshot(
                Request::builder()
                    .uri("/../../etc/passwd")
                    .header("host", "localhost:8080")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::NOT_FOUND);
    }
    #[tokio::test]
    async fn rejects_rebinding_and_untrusted_forwarded_hosts_on_every_route() {
        let app = router(
            Arc::new(RwLock::new(json!({"serial": "secret"}))),
            HostPolicy::new(
                "127.0.0.1:8080".parse().unwrap(),
                &["monitor.example.test".into()],
            ),
        );
        for path in ["/", "/api/state", "/app.js", "/style.css", "/missing"] {
            for host in [
                None,
                Some("attacker.test:8080"),
                Some("localhost:9999"),
                Some("localhost:8080.attacker.test"),
                Some("localhost:8080@attacker.test"),
                Some("localhost:8080,attacker.test"),
            ] {
                let mut request = Request::builder()
                    .uri(path)
                    .header("x-forwarded-host", "localhost:8080")
                    .header("forwarded", "host=localhost:8080");
                if let Some(host) = host {
                    request = request.header("host", host);
                }
                let r = app
                    .clone()
                    .oneshot(request.body(Body::empty()).unwrap())
                    .await
                    .unwrap();
                assert_eq!(r.status(), StatusCode::FORBIDDEN, "{path} {host:?}");
                let body = to_bytes(r.into_body(), 4096).await.unwrap();
                assert!(!String::from_utf8_lossy(&body).contains("secret"));
            }
        }
        for host in [
            "localhost:8080",
            "LOCALHOST:8080",
            "127.0.0.1:8080",
            "monitor.example.test",
        ] {
            let r = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri("/api/state")
                        .header("host", host)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(r.status(), StatusCode::OK, "{host}");
        }
        for request in [
            Request::builder()
                .uri("/api/state")
                .header("host", "localhost:8080")
                .header("host", "attacker.test:8080")
                .body(Body::empty())
                .unwrap(),
            Request::builder()
                .uri("http://attacker.test:8080/api/state")
                .header("host", "localhost:8080")
                .body(Body::empty())
                .unwrap(),
        ] {
            assert_eq!(
                app.clone().oneshot(request).await.unwrap().status(),
                StatusCode::FORBIDDEN
            );
        }
    }

    #[test]
    fn exact_host_configuration_handles_ipv6_and_specific_interface_binds() {
        for bad in [
            "*",
            "*.example.test",
            "https://monitor.example.test",
            "user@localhost",
            "localhost/path",
            "localhost:",
            "localhost:bad",
            "localhost:65536",
            "",
            "a..b",
            "a b",
        ] {
            assert!(authority(bad).is_err(), "accepted {bad}");
        }
        assert_eq!(authority("[::1]:8080").unwrap(), "[::1]:8080");
        assert_eq!(
            authority("MONITOR.example.test:443").unwrap(),
            "monitor.example.test:443"
        );
        for (bound, allowed, rejected) in [
            ("[::1]:8080", "[::1]:8080", "attacker.test:8080"),
            (
                "192.168.1.25:8080",
                "192.168.1.25:8080",
                "attacker.test:8080",
            ),
            (
                "100.101.102.103:8080",
                "100.101.102.103:8080",
                "attacker.test:8080",
            ),
            (
                "0.0.0.0:8080",
                "monitor.example.test:8080",
                "192.168.1.25:8080",
            ),
        ] {
            let policy = HostPolicy::new(
                bound.parse().unwrap(),
                &["monitor.example.test:8080".into()],
            );
            let request = |host| {
                Request::builder()
                    .uri("/api/state")
                    .header("host", host)
                    .body(Body::empty())
                    .unwrap()
            };
            assert!(policy.accepts(&request(allowed)));
            assert!(!policy.accepts(&request(rejected)));
        }
    }
}
