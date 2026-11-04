# Headless Ubuntu Server

The headless build needs no display server, window toolkit, browser on the host,
or desktop session. It uses the same `Engine`, sysfs scanner, usbmon counters,
rate history, issue watcher, SDR stall detection and journal parser as the desktop.
Topology refreshes every two seconds; rates and issues update once a second even
with no connected clients. The responsive, read-only web UI polls a snapshot once
per second and includes nested topology, controller/bus/device rates, 60-second
traffic graphs, device details, error-code counts, current issues, and searchable
in-memory event history (the latest 2,000 events). Persistent events are appended
to the existing server-side log; older log entries are not loaded into the UI.

## Build and run

On Ubuntu Server 24.04/26.04 amd64, install `build-essential`, `curl`, `git`,
`pkg-config`, `hwdata`, and `usbutils`. Install current stable Rust through
[rustup](https://rustup.rs/) (the desktop library requires Rust 1.95 or newer).
Then:

```sh
git clone https://github.com/n71fs/usb-bus-monitor.git
cd usb-bus-monitor
git switch feat/headless-web
cargo build --locked --release --no-default-features --features web
./target/release/usb-monitor --headless
```

Open `http://127.0.0.1:8080` on the host, or use an SSH tunnel:

```sh
ssh -N -L 8080:127.0.0.1:8080 user@server
```

Then open `http://127.0.0.1:8080` on your client. The default listen address is
**127.0.0.1**, never all interfaces. The process prints the actual bound URL;
`--port 0` selects an ephemeral port. IPv6 is supported, e.g. `--bind ::1`.
Malformed IP addresses/ports and bind failures cause a nonzero exit.

To build both interfaces, use `cargo build --locked --release --features web`.
Running that binary without `--headless` retains the existing desktop behavior.
The default build still includes only the desktop; `--dump`, desktop installation,
and `--sys-root PATH` remain available. `--demo-traffic` simulates rates only for
devices present in sysfs and is clearly marked in the web UI; do not use in service.

## USB traffic and journal permissions

Topology needs no root privileges. Enable traffic once on the host:

```sh
sh setup-usbmon.sh
```

This installs the existing persistent usbmon module/udev configuration. The
`plugdev` group can read `/dev/usbmon*`; it can observe all USB traffic, including
keyboards. Grant it only to trusted accounts. No graphical authentication prompt
is invoked in headless mode. The web server cannot run setup commands or change
host configuration.

For an interactive account, add it to `plugdev` and `systemd-journal`, then log
out and back in. The journal group grants broad system-log read access. Verify
permissions **as the service account**, not as root:

```sh
sudo -u usb-monitor test -r /dev/usbmon0
sudo -u usb-monitor journalctl -n 1 --no-pager
```

Missing usbmon/module permissions leave topology available and show a warning;
the traffic monitor retries automatically. Without readable journal entries,
USB resets, enumeration errors and SDR software errors may be absent. Journal
availability reflects the journal follower process and is not a guarantee that
all system logs are readable. Test the command above and inspect service logs.

## LAN or Tailscale

Bind to a specific interface address explicitly:

```sh
# Private LAN address assigned to this server
usb-monitor --headless --bind 192.168.1.25 --port 8080
# Tailscale address assigned to this server
usb-monitor --headless --bind 100.101.102.103 --port 8080
```

Replace example addresses with the server's real addresses (`ip addr`,
`tailscale ip -4`). The bind fails if that address is unavailable. A Tailscale
service bind may require restarting after Tailscale connects. A LAN bind is plain
HTTP and has **no built-in authentication**: any reachable client can read USB
identities/serials, traffic statistics and relevant journal messages. Restrict
access with firewall rules and Tailscale grants/ACLs. Do not add router port
forwarding, publish a Docker port on all interfaces, or use Tailscale Funnel.
`--bind 0.0.0.0` and `--bind ::` deliberately listen on all matching interfaces;
they are supported only as explicit opt-ins and are discouraged.

A preferred Tailscale option keeps the process on localhost and uses an encrypted
[tailnet-only Serve proxy](https://tailscale.com/kb/1242/tailscale-serve):

```sh
sudo tailscale serve --bg http://127.0.0.1:8080
```

Follow the Tailscale HTTPS setup prompts and restrict the resulting HTTPS service
using your tailnet policy. `tailscale serve` and public `tailscale funnel` have
different exposure scopes. Do not enable Funnel for this monitor.

## systemd

After building, install the binary and create a dedicated account. The following
commands assume the existing setup script has created the usbmon rule and your
Ubuntu system has `plugdev` and `systemd-journal` groups:

```sh
sudo useradd --system --user-group --home-dir /var/lib/usb-monitor --shell /usr/sbin/nologin usb-monitor
sudo usermod -aG plugdev,systemd-journal usb-monitor
sudo install -m 0755 target/release/usb-monitor /usr/local/bin/usb-monitor
sudo install -m 0644 deploy/usb-monitor.service /etc/systemd/system/usb-monitor.service
sudo systemctl daemon-reload
sudo systemctl enable --now usb-monitor
sudo systemctl status usb-monitor
journalctl -u usb-monitor -f
```

If the account already exists, skip `useradd`. The service runs without root,
uses localhost, and stores events in `/var/lib/usb-monitor/usb-monitor/events.log`.
Rotate that file with logrotate's `copytruncate` option if required (the watcher
keeps its file descriptor open). systemd stops the server with SIGTERM; the web
server drains requests, stops sampling, terminates its journal follower and exits. systemd also cleans up
any processes left in the service control group.

To change the interface/port, use `sudo systemctl edit usb-monitor`:

```ini
[Service]
ExecStart=
ExecStart=/usr/local/bin/usb-monitor --headless --bind 100.101.102.103 --port 8080
```

Then run `sudo systemctl daemon-reload && sudo systemctl restart usb-monitor`.
For a direct Tailscale bind you may add `After=tailscaled.service` in `[Unit]`,
but ordering alone does not guarantee that the address is already assigned.
Using Serve with the localhost default avoids that startup dependency.

## Authentication and reverse proxies

There is no application password, session, or API token. `/api/state` has the
same exposure as the UI. Protect the **entire origin**, including static assets
and `/api/state`, with an authenticated HTTPS reverse proxy when users outside a
trusted tailnet need access. For example, Caddy with `basic_auth` (use a password
hash generated with `caddy hash-password`) or an OIDC-aware proxy can forward to
`127.0.0.1:8080`. Keep the backend on localhost so clients cannot bypass proxy
authentication. Set up certificates and access rules appropriate to your network;
never send HTTP Basic credentials over plain HTTP. No CORS access is enabled.
The UI uses same-origin assets, text-only rendering of device/log strings,
no-store responses, and a restrictive content security policy.

## Validation

```sh
cargo test --locked --no-default-features --features web
cargo test --locked                         # original desktop + fake sysfs tests
cargo check --locked --all-features
cargo build --locked --release --no-default-features --features web
```

The integration tests exercise a fake sysfs host and HTTP lifecycle without a
display. Unit tests retain the existing SDR stop/recovery, bandwidth and error
checks. Actual usbmon IOCTLs, hardware hotplug, Tailscale routing and journal
permissions still require verification on the deployment host.

References: [systemd execution settings](https://www.freedesktop.org/software/systemd/man/latest/systemd.exec.html), [Tailscale Serve command](https://tailscale.com/docs/reference/tailscale-cli/serve).
