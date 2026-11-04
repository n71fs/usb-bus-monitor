"use strict";
const $ = (id) => document.getElementById(id);
let state,
  selected = null,
  entities = new Map();
// USB descriptors, journal lines and controller names are untrusted text.
function el(tag, text, cls) {
  const n = document.createElement(tag);
  if (text !== undefined) n.textContent = text;
  if (cls) n.className = cls;
  return n;
}
function rate(n) {
  return n >= 1e6
    ? `${(n / 1e6).toFixed(1)} MB/s`
    : n >= 1e3
      ? `${(n / 1e3).toFixed(0)} kB/s`
      : `${Math.round(n || 0)} B/s`;
}
function node(item, subtitle) {
  entities.set(item.id, item);
  const b = el(
    "button",
    undefined,
    `node${selected === item.id ? " selected" : ""}`,
  );
  b.type = "button";
  b.dataset.id = item.id;
  const name = el("span");
  if (item.sdr) name.append(el("span", "SDR", "badge"));
  const issue = state.issues.find((i) => i.target === item.id);
  if (issue) name.className = issue.severity;
  name.append(document.createTextNode(item.name), el("small", subtitle));
  b.append(
    name,
    el(
      "span",
      state.traffic_state === "Running" ? rate(item.rate.now) : "unavailable",
      "rate",
    ),
  );
  b.onclick = () => {
    selected = item.id;
    renderTree();
    renderDetails();
  };
  return b;
}
function device(d, parent) {
  const row = el("div");
  row.append(
    node(
      d,
      `${d.id} · ${d.speed_mbps} Mb/s${d.below_speed ? " · slower than capable" : ""}${d.errors ? " · transfer errors" : ""}`,
    ),
  );
  if (d.children.length) {
    const children = el("div", undefined, "devices");
    d.children.forEach((c) => device(c, children));
    row.append(children);
  }
  parent.append(row);
}
function renderTree() {
  const container = $("tree"),
    scroll = container.scrollTop;
  const focus = document.activeElement?.dataset.id;
  entities = new Map();
  const tree = el("div");
  for (const c of state.controllers) {
    const section = el("div", undefined, "controller");
    section.append(node(c, `${c.kind} · ${c.pci} · ${c.sdr_count} SDRs`));
    for (const b of c.buses) {
      const bus = el("div", undefined, "bus");
      bus.append(
        el(
          "div",
          `${b.label} · ${state.traffic_state === "Running" ? rate(b.rate.now) : "traffic unavailable"}`,
          "bus-title",
        ),
      );
      const bar = el("progress");
      bar.max = b.capacity;
      bar.value = b.rate.now;
      bar.setAttribute("aria-label", `${b.label} bandwidth utilization`);
      bus.append(bar);
      if (!b.devices.length)
        bus.append(el("small", "Nothing plugged in", "empty"));
      b.devices.forEach((d) => device(d, bus));
      section.append(bus);
    }
    tree.append(section);
  }
  if (!state.controllers.length)
    tree.append(el("p", "No USB controllers found.", "empty"));
  container.replaceChildren(tree);
  container.scrollTop = scroll;
  if (focus)
    Array.from(container.querySelectorAll("button"))
      .find((b) => b.dataset.id === focus)
      ?.focus({ preventScroll: true });
}
function graph(s, label) {
  const box = el("div");
  box.append(
    el(
      "p",
      `${label} · ${state.traffic_state === "Running" ? rate(s.now) : "traffic unavailable"}`,
    ),
  );
  const canvas = el("canvas", undefined, "chart");
  canvas.width = 900;
  canvas.height = 220;
  canvas.setAttribute("role", "img");
  canvas.setAttribute(
    "aria-label",
    `${label}: last 60 seconds, current ${rate(s.now)}`,
  );
  box.append(canvas);
  const ctx = canvas.getContext("2d"),
    values = s.history || [],
    max = Math.max(1, ...values);
  ctx.strokeStyle = "#253241";
  ctx.beginPath();
  ctx.moveTo(0, 195);
  ctx.lineTo(900, 195);
  ctx.stroke();
  if (values.length) {
    ctx.strokeStyle = "#62dbc3";
    ctx.lineWidth = 3;
    ctx.beginPath();
    values.forEach((v, i) => {
      const x = ((60 - values.length + i) * 900) / 59,
        y = 190 - (v / max) * 150;
      if (i === 0) ctx.moveTo(x, y);
      else ctx.lineTo(x, y);
    });
    ctx.stroke();
  }
  ctx.fillStyle = "#93a6b9";
  ctx.font = "18px system-ui";
  ctx.fillText(rate(max), 12, 24);
  ctx.fillText("−60s", 12, 215);
  ctx.fillText("now", 850, 215);
  return box;
}
function renderDetails() {
  if (!selected) return;
  const d = entities.get(selected),
    target = $("details");
  if (!d) {
    target.replaceChildren(
      el("p", "This device is disconnected. Select another item.", "empty"),
    );
    return;
  }
  const content = el("div");
  content.append(el("h3", d.name));
  if (d.buses) {
    content.append(el("p", `${d.kind} · ${d.pci}`, "empty"));
    const facts = el("dl", undefined, "facts");
    for (const [k, v] of [
      ["Driver", d.driver],
      ["PCIe link", d.pcie_link],
    ])
      facts.append(el("dt", k), el("dd", v || "Unknown"));
    content.append(facts);
    d.buses.forEach((b) => content.append(graph(b.rate, b.label)));
  } else {
    const facts = el("dl", undefined, "facts");
    for (const [k, v] of [
      ["Location", `Bus ${d.bus}, port ${d.port}, address ${d.address}`],
      ["USB IDs", `${d.vendor_id}:${d.product_id}`],
      ["Manufacturer", d.manufacturer],
      ["Serial", d.serial],
      ["Link", `${d.speed_mbps} Mb/s · USB ${d.usb_version || "?"}`],
      ["Max power", d.max_power],
      ["Drivers", d.drivers.join(", ")],
      ["SDR", d.sdr],
      ["Transfer errors", d.errors],
    ])
      facts.append(el("dt", k), el("dd", String(v ?? "—") || "—"));
    content.append(facts, graph(d.rate, "Device traffic"));
    d.error_codes.forEach((e) =>
      content.append(el("p", `${e.name} (${e.code}): ${e.count}`, "Warning")),
    );
  }
  state.issues
    .filter((i) => i.target === selected)
    .forEach((i) => content.append(el("p", i.text, i.severity)));
  target.replaceChildren(content);
}
function renderEvents() {
  const filter = $("filter").value.toLowerCase(),
    list = $("events"),
    scroll = list.scrollTop;
  const events = state.events.filter((e) =>
    `${e.time} ${e.severity} ${e.text}`.toLowerCase().includes(filter),
  );
  const content = el("div");
  for (const e of events) {
    const row = el("div", undefined, `event ${e.severity}`);
    row.append(el("time", `${e.time} · ${e.severity}`), el("span", e.text));
    content.append(row);
  }
  if (!events.length) content.append(el("p", "No matching events.", "empty"));
  list.replaceChildren(content);
  list.scrollTop = scroll;
  $("event-count").textContent = `${events.length} / ${state.events.length}`;
}
function render() {
  $("board").textContent = state.board || "Linux USB host";
  $("demo").textContent = state.demo ? "DEMO TRAFFIC" : "";
  let devices = 0;
  const count = (d) => {
    devices++;
    d.children.forEach(count);
  };
  state.controllers.forEach((c) =>
    c.buses.forEach((b) => b.devices.forEach(count)),
  );
  $("summary").replaceChildren(
    ...[
      [state.controllers.length, "Controllers"],
      [devices, "Devices"],
      [state.controllers.reduce((n, c) => n + c.sdr_count, 0), "SDRs"],
      [
        state.traffic_state === "Running"
          ? rate(state.controllers.reduce((n, c) => n + c.rate.now, 0))
          : "Unavailable",
        "Live traffic",
      ],
    ].map(([v, k]) => {
      const m = el("div", undefined, "metric");
      m.append(el("strong", String(v)), el("span", k));
      return m;
    }),
  );
  const notices = [];
  if (state.demo) notices.push("Demo traffic is simulated.");
  if (state.scan_error) notices.push(state.scan_error);
  if (state.traffic_state !== "Running")
    notices.push(
      `Live traffic: ${state.traffic_state}. Run setup-usbmon.sh on the server and grant the service account usbmon access; topology remains available.`,
    );
  if (!state.journal_running)
    notices.push(
      "System journal unavailable: USB resets and SDR software errors may be missing. Check journalctl permissions for the service account.",
    );
  if (state.lost_events)
    notices.push(
      `${state.lost_events} traffic events dropped by the kernel; rates may be low.`,
    );
  $("notice").textContent = notices.join("\n");
  renderTree();
  renderDetails();
  const issues = el("div");
  state.issues.forEach((i) => {
    const row = el("div", undefined, `issue ${i.severity}`);
    const b = el("button", `${i.severity} · ${i.text}`);
    b.onclick = () => {
      if (i.target) {
        selected = i.target;
        renderTree();
        renderDetails();
        $("details").scrollIntoView({ behavior: "smooth", block: "nearest" });
      }
    };
    row.append(b);
    issues.append(row);
  });
  if (!state.issues.length)
    issues.append(el("p", "No issues right now.", "empty"));
  $("issues").replaceChildren(issues);
  $("issue-count").textContent = state.issues.length;
  renderEvents();
  $("log-path").textContent =
    `Persistent server log: ${state.log_path || "disabled"}`;
  $("updated").textContent = `Sample: ${state.time} (server time)`;
}
$("filter").addEventListener("input", () => {
  if (state) renderEvents();
});
async function poll() {
  try {
    const response = await fetch("/api/state", { cache: "no-store" });
    if (!response.ok) throw new Error(`HTTP ${response.status}`);
    const next = await response.json();
    const changed = !state || state.generation !== next.generation;
    state = next;
    if (changed) render();
    $("connection").textContent = "● Connected";
    $("connection").className = "";
  } catch (e) {
    $("connection").textContent = "Disconnected · retrying";
    $("connection").className = "offline";
  } finally {
    setTimeout(poll, 1000);
  }
}
poll();
