mod engine;
#[cfg(feature = "desktop")]
mod gui;
#[cfg(feature = "web")]
mod web;
mod ids;
mod install;
mod issues;
mod klog;
mod names;
mod rates;
mod sdr;
mod sysfs;
mod traffic;

use std::path::PathBuf;

use ids::IdDb;
use sysfs::{Device, Topology};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut sys_root = PathBuf::from("/sys");
    let mut dump = false;
    let mut demo = false;
    let mut headless = false;
    let mut bind: std::net::IpAddr = "127.0.0.1".parse()?;
    let mut port: u16 = 8080;
    let mut bind_set = false;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--dump" => dump = true,
            "--headless" => headless = true,
            "--bind" => { bind = args.next().ok_or("--bind needs an IP address")?.parse()?; bind_set = true; },
            "--port" => { port = args.next().ok_or("--port needs a number")?.parse()?; bind_set = true; },
            "--install" => return Ok(install::install()?),
            "--uninstall" => return Ok(install::uninstall()?),
            // Made-up traffic, for screenshots on machines without SDRs.
            "--demo-traffic" => demo = true,
            "--sys-root" => sys_root = args.next().ok_or("--sys-root needs a path")?.into(),
            "-V" | "--version" => {
                println!("USB Bus Monitor {}", env!("CARGO_PKG_VERSION"));
                return Ok(());
            }
            "-h" | "--help" => {
                println!(
                    "usb-monitor [--headless [--bind IP] [--port PORT] | --install | --uninstall | --dump | --version] [--sys-root PATH]\n\n  \
                     --install        add USB Bus Monitor to the app menu (no sudo needed)\n  \
                     --uninstall      remove it from the app menu\n  \
                     --dump           print the USB layout as text instead of opening the window\n  \
                     --headless       serve the web UI (requires web feature)\n  \
                     --bind IP        explicit listen address (default 127.0.0.1)\n  \
                     --port PORT      listen port (default 8080)\n  \
                     --version        print the version number\n  \
                     --sys-root PATH  read a copy of /sys from PATH (for testing)"
                );
                return Ok(());
            }
            other => return Err(format!("unknown option {other}").into()),
        }
    }

    if bind_set && !headless { return Err("--bind and --port require --headless".into()); }
    if headless && dump { return Err("--headless and --dump are mutually exclusive".into()); }
    if headless {
        #[cfg(feature = "web")]
        return web::run(sys_root, IdDb::load_system(), demo, std::net::SocketAddr::new(bind, port));
        #[cfg(not(feature = "web"))]
        return Err("web mode was not compiled; rebuild with --features web".into());
    }
    let _ = (bind, port);
    let ids = IdDb::load_system();
    if dump {
        let topo = sysfs::scan(&sys_root, &ids)?;
        print!("{}", render_text(&topo));
        return Ok(());
    }
    #[cfg(feature = "desktop")]
    { gui::run(sys_root, ids, demo)?; Ok(()) }
    #[cfg(not(feature = "desktop"))]
    { Err("desktop mode was not compiled; use --headless or --dump".into()) }
}

/// Plain-text version of the tree, for `--dump`.
pub fn render_text(topo: &Topology) -> String {
    let mut s = String::new();
    if !topo.board.is_empty() {
        s += &format!("Board: {}\n", topo.board);
    }
    for c in &topo.controllers {
        s += &format!("{}  [{}]  PCI {}", c.name, c.kind.label(), c.pci_addr);
        if !c.pcie_link.is_empty() {
            s += &format!("  link {}", c.pcie_link);
        }
        let sdrs = c.sdrs().len();
        if sdrs > 0 {
            s += &format!("  SDRs: {sdrs}");
        }
        s += "\n";
        for b in &c.buses {
            s += &format!("  {}\n", b.label());
            for d in &b.root.children {
                dump_device(&mut s, d, 2);
            }
        }
    }
    s
}

fn dump_device(s: &mut String, d: &Device, depth: usize) {
    let indent = "  ".repeat(depth);
    let port = d.port_path().unwrap_or("?");
    s.push_str(&format!("{indent}port {port}: {} [{:04x}:{:04x}] {}", d.display_name(), d.vendor_id, d.product_id, sysfs::speed_short(d.speed_mbps)));
    if let Some(kind) = d.sdr {
        s.push_str(&format!("  SDR: {kind}"));
    }
    if d.below_capable_speed() {
        s.push_str(&format!("  (supports USB {}, running slower)", d.usb_version));
    }
    s.push('\n');
    for c in &d.children {
        dump_device(s, c, depth + 1);
    }
}
