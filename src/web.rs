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
    response::{Html, Response},
    routing::get,
};
use serde_json::{Value, json};
use std::{
    net::SocketAddr,
    path::PathBuf,
    sync::{Arc, RwLock},
};

type Snapshot = Arc<RwLock<Value>>;

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

fn router(state: Snapshot) -> Router {
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
        .layer(middleware::from_fn(headers))
        .with_state(state)
}

pub fn run(
    root: PathBuf,
    ids: IdDb,
    demo: bool,
    address: SocketAddr,
) -> Result<(), Box<dyn std::error::Error>> {
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
        let result = axum::serve(listener, router(state)).with_graceful_shutdown(shutdown()).await;
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
        let app = router(Arc::new(RwLock::new(json!({"generation": 7}))));
        for (path, content_type) in [
            ("/", "text/html"),
            ("/app.js", "text/javascript"),
            ("/style.css", "text/css"),
            ("/api/state", "application/json"),
        ] {
            let r = app
                .clone()
                .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
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
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::NOT_FOUND);
    }
}
