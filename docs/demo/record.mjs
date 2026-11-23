// Drives the running brew-app over WebView2's CDP port while ffmpeg records the window.
// usage: node record.mjs <out.mp4> <scenes.json>
import { spawn } from "node:child_process";
import { readFileSync } from "node:fs";

const [out, scenesPath] = process.argv.slice(2);
const scenes = JSON.parse(readFileSync(scenesPath, "utf8"));
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

const targets = await (await fetch("http://127.0.0.1:9222/json")).json();
const page = targets.find((t) => t.type === "page");
const ws = new WebSocket(page.webSocketDebuggerUrl);
await new Promise((r) => ws.addEventListener("open", r, { once: true }));
let seq = 0;
const pending = new Map();
ws.addEventListener("message", (e) => {
  const m = JSON.parse(e.data);
  if (m.id && pending.has(m.id)) { pending.get(m.id)(m); pending.delete(m.id); }
});
const send = (method, params = {}) => new Promise((r) => {
  const id = ++seq; pending.set(id, r); ws.send(JSON.stringify({ id, method, params }));
});
const evaluate = async (expression) => {
  const m = await send("Runtime.evaluate", { expression, awaitPromise: true, returnByValue: true });
  return m.result?.result?.value;
};

const helpers = `
window.__scroller = () => {
  let best = document.scrollingElement, room = best.scrollHeight - best.clientHeight;
  for (const el of document.querySelectorAll("*")) {
    const r = el.scrollHeight - el.clientHeight, s = getComputedStyle(el).overflowY;
    if (r > room && (s === "auto" || s === "scroll")) { best = el; room = r; }
  }
  return best;
};
window.__scroll = (frac, ms) => new Promise((done) => {
  const el = window.__scroller(), from = el.scrollTop;
  const to = (el.scrollHeight - el.clientHeight) * frac, t0 = performance.now();
  const step = (t) => {
    const k = Math.min(1, (t - t0) / ms), e = k < .5 ? 2*k*k : 1 - (-2*k + 2)**2 / 2;
    el.scrollTop = from + (to - from) * e;
    k < 1 ? requestAnimationFrame(step) : done();
  };
  requestAnimationFrame(step);
});
`;

// CDP input doesn't move the OS cursor, so draw one for the viewer.
const cursor = (x, y) => evaluate(`(() => {
  let c = document.getElementById("__cursor");
  if (!c) { c = document.createElement("div"); c.id = "__cursor";
    c.style.cssText = "position:fixed;width:18px;height:18px;margin:-9px 0 0 -9px;border-radius:50%;background:rgba(30,30,30,.35);border:2px solid #fff;box-shadow:0 1px 4px rgba(0,0,0,.4);pointer-events:none;z-index:99999";
    document.body.append(c); }
  c.style.opacity = 1; c.style.left = "${x}px"; c.style.top = "${y}px";
})()`);
const plot = async () => {
  await evaluate(`document.querySelector(".u-over")?.scrollIntoView({block: "center"})`);
  await sleep(400);
  return evaluate(`(() => { const r = document.querySelector(".u-over").getBoundingClientRect(); return {x: r.x, y: r.y, w: r.width, h: r.height}; })()`);
};
const mouse = (type, x, y, extra = {}) => send("Input.dispatchMouseEvent", { type, x, y, button: "left", ...extra });
async function drag(from, to) {
  const r = await plot(), y = r.y + r.h * 0.5;
  const x0 = r.x + r.w * from, x1 = r.x + r.w * to;
  await cursor(x0, y); await sleep(500);
  await mouse("mousePressed", x0, y, { clickCount: 1, buttons: 1 });
  for (let i = 1; i <= 40; i++) {
    const x = x0 + (x1 - x0) * i / 40;
    await mouse("mouseMoved", x, y, { buttons: 1 }); await cursor(x, y); await sleep(25);
  }
  await mouse("mouseReleased", x1, y, { clickCount: 1 });
}
async function dblclick() {
  const r = await plot(), x = r.x + r.w / 2, y = r.y + r.h / 2;
  await cursor(x, y); await sleep(400);
  for (const n of [1, 2]) {
    await mouse("mousePressed", x, y, { clickCount: n, buttons: 1 });
    await mouse("mouseReleased", x, y, { clickCount: n });
  }
}

// Screencast rather than screen grab: frames come from the WebView itself, so other
// windows on top don't end up in the video. Frames arrive only on change; a 30 fps
// clock repeats the latest one.
const ffmpeg = spawn("ffmpeg", [
  "-y", "-f", "image2pipe", "-framerate", "30", "-c:v", "mjpeg", "-i", "-",
  "-vf", "scale=trunc(iw/2)*2:trunc(ih/2)*2",
  "-c:v", "libx264", "-preset", "slow", "-crf", "18", "-pix_fmt", "yuv420p", "-movflags", "+faststart", out,
], { stdio: ["pipe", "ignore", "inherit"] });
let latest = null;
ws.addEventListener("message", (e) => {
  const m = JSON.parse(e.data);
  if (m.method === "Page.screencastFrame") {
    latest = Buffer.from(m.params.data, "base64");
    ws.send(JSON.stringify({ id: ++seq, method: "Page.screencastFrameAck", params: { sessionId: m.params.sessionId } }));
  }
});
// A clean page each take: overlays from a previous run would otherwise still be there.
await send("Page.enable");
await evaluate(`location.hash = "#/"`);
await send("Page.reload");
await sleep(2500);

// Overlays live in the page so the screencast picks them up.
await evaluate(`(() => {
  const st = document.createElement("style");
  st.textContent = \`
    footer .path { visibility: hidden }
    #__title { position: fixed; inset: 0; z-index: 100000; display: grid; place-content: center; text-align: center;
      background: radial-gradient(ellipse at center, #1d2230 0%, #0d0f14 75%); color: #f2f4f8;
      font-family: "Inter Variable", system-ui, sans-serif; transition: opacity .8s ease }
    #__title h1 { font-size: 64px; font-weight: 700; letter-spacing: -0.03em; margin: 0 }
    #__title p { font-size: 22px; color: #9aa3b5; margin: 12px 0 0 }
    #__cap { position: fixed; left: 50%; bottom: 44px; z-index: 99998; transform: translate(-50%, 12px); opacity: 0;
      padding: 10px 20px; border-radius: 999px; background: rgba(12, 14, 20, .88); color: #f2f4f8;
      font: 600 18px "Inter Variable", system-ui, sans-serif; box-shadow: 0 6px 24px rgba(0,0,0,.45);
      border: 1px solid rgba(255,255,255,.08); transition: opacity .35s ease, transform .35s ease; white-space: nowrap }
    #__cap.on { opacity: 1; transform: translate(-50%, 0) }
    #__cursor { transition: opacity .3s }
  \`;
  document.head.append(st);
  const cap = document.createElement("div"); cap.id = "__cap"; document.body.append(cap);
  window.__caption = (text) => {
    if (!text) { cap.classList.remove("on"); return; }
    cap.classList.remove("on");
    setTimeout(() => { cap.textContent = text; cap.classList.add("on"); }, cap.textContent ? 350 : 0);
  };
  window.__card = (title, sub) => {
    let c = document.getElementById("__title");
    if (!title) { if (c) { c.style.opacity = 0; setTimeout(() => c.remove(), 900); } return; }
    if (!c) { c = document.createElement("div"); c.id = "__title"; document.body.append(c); }
    c.innerHTML = "<div><h1></h1><p></p></div>";
    c.querySelector("h1").textContent = title; c.querySelector("p").textContent = sub;
    c.style.opacity = 1;
  };
})()`);
await send("Page.enable");
// The opening card goes up before the first frame, or the video opens on the app.
if (scenes[0]?.card) {
  await evaluate(`location.hash = ${JSON.stringify(scenes[0].hash ?? "#/")}`);
  await evaluate(`window.__card(${JSON.stringify(scenes[0].card[0])}, ${JSON.stringify(scenes[0].card[1] ?? "")})`);
  await sleep(1200);
}
await send("Page.startScreencast", { format: "jpeg", quality: 92, everyNthFrame: 1 });
while (!latest) await sleep(50);
const t0 = Date.now();
let written = 0;
const clock = setInterval(() => {
  const due = Math.floor((Date.now() - t0) / (1000 / 30));
  while (written <= due) { ffmpeg.stdin.write(latest); written++; }
}, 10);
await sleep(500);

for (const s of scenes) {
  if (s.hash !== undefined) {
    await evaluate(`location.hash = ${JSON.stringify(s.hash)}`);
    await sleep(900);
    await evaluate(helpers);
  }
  if (s.card !== undefined) await evaluate(`window.__card(${JSON.stringify(s.card?.[0] ?? null)}, ${JSON.stringify(s.card?.[1] ?? "")})`);
  if (s.caption !== undefined) await evaluate(`window.__caption(${JSON.stringify(s.caption)})`);
  if (s.js) await evaluate(s.js);
  if (s.drag) await drag(...s.drag);
  if (s.dblclick) await dblclick();
  if (s.drag || s.dblclick) evaluate(`setTimeout(() => document.getElementById("__cursor")?.style.setProperty("opacity", "0"), 900)`);
  if (s.scroll !== undefined) await evaluate(`window.__scroll(${s.scroll}, ${s.scrollMs ?? 2500})`);
  await sleep(s.hold ?? 3000);
}

clearInterval(clock);
await send("Page.stopScreencast");
ffmpeg.stdin.end();
await new Promise((r) => ffmpeg.on("exit", r));
ws.close();
console.log("wrote", out);
