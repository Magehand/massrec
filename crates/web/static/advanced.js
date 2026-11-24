// Advanced page: channel wall (one canvas for all channels), selected channel, events, disks.
"use strict";

(() => {
  const { $, esc, hms, bytesHuman, rateHuman, db, ch3, setText, api, toast } = MC;

  const C = {
    cell: "#101216", cellBorder: "#1c2027", cellSel: "#1e242c", cellHover: "#15181d", segOff: "#191d23",
    green: "#4a9d5f", greenMeter: "#3f7d52", hot: "#b8a13a", clip: "#c0392b", drop: "#7a4bd0", silent: "#2b3038",
    text: "#e8eaec", label: "#5d646d", labelFaint: "#4b5158",
  };
  // meter array layout from the server: [peak, hold, rms, clips, dropped, state]
  const PEAK = 0, HOLD = 1, RMS = 2, CLIPS = 3, DROPPED = 4, STATE = 5;

  let view = "wall";
  let faultsOnly = false;
  let selected = 0;
  let hover = -1;
  let layout = null; // { idx, cols, cw, ch, gap }
  const rate = { bytes: 0, t: 0, ch: -1, bps: 0 };

  const canvas = $("wall");
  const ctx = canvas.getContext("2d");
  const wrap = $("wall-wrap");

  const isFault = (st) => st !== "Ok" && st !== "Hot";
  const meterPos = (v) => Math.min(1, Math.max(0, (v + 60) / 60));
  function stateColor(st, hold) {
    switch (st) {
      case "Drop": return C.drop;
      case "Silent": return C.silent;
      case "Clip": return C.clip;
      case "Hot": return C.hot;
      default: return hold > -0.5 ? C.clip : hold > -3 ? C.hot : C.greenMeter;
    }
  }
  function stateLabel(st, rec) {
    switch (st) {
      case "Drop": return ["fault", "Disk drop"];
      case "Silent": return ["warn", "No signal"];
      case "Clip": return ["fault", "Clipping"];
      default: return ["ok", rec ? "Recording" : "Standby"];
    }
  }

  function rect(x, y, w, h, fill) {
    ctx.fillStyle = fill;
    ctx.fillRect(x, y, w, h);
  }

  // ---- channel wall -----------------------------------------------------------
  function draw() {
    const s = MC.status();
    if (!s) return;
    const meters = s.meters;
    const idx = [];
    for (let i = 0; i < meters.length; i++) if (!faultsOnly || isFault(meters[i][STATE])) idx.push(i);
    setText("shown", `${idx.length} of ${s.channels} channels shown`);

    const wall = view === "wall";
    const gap = 3;
    const minW = wall ? 34 : 78;
    const ch = wall ? 66 : 26;
    const avail = Math.max(minW, wrap.clientWidth - 24);
    const cols = Math.max(1, Math.floor((avail + gap) / (minW + gap)));
    const cw = (avail - gap * (cols - 1)) / cols; // stretch to fill the row
    const rows = Math.ceil(idx.length / cols);
    const height = Math.max(1, rows * (ch + gap) - gap);
    layout = { idx, cols, cw, ch, gap };

    const dpr = window.devicePixelRatio || 1;
    if (canvas.width !== Math.round(avail * dpr) || canvas.height !== Math.round(height * dpr)) {
      canvas.width = Math.round(avail * dpr);
      canvas.height = Math.round(height * dpr);
      canvas.style.width = avail + "px";
      canvas.style.height = height + "px";
    }
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.clearRect(0, 0, avail, height);
    ctx.textAlign = "center";
    ctx.textBaseline = "middle";

    // only draw rows that are on screen
    const top = wrap.scrollTop - 12, bottom = top + wrap.clientHeight + 24;
    for (let k = 0; k < idx.length; k++) {
      const r = Math.floor(k / cols), c = k % cols;
      const x = c * (cw + gap), y = r * (ch + gap);
      if (y + ch < top || y > bottom) continue;
      const i = idx[k];
      const m = meters[i];
      const sel = i === selected;
      rect(x, y, cw, ch, sel ? C.green : wall ? C.cellBorder : "#1a1e24");
      rect(x + 1, y + 1, cw - 2, ch - 2, sel ? C.cellSel : i === hover ? C.cellHover : C.cell);
      const col = stateColor(m[STATE], m[HOLD]);
      if (wall) drawMeterCell(x, y, cw, m, col, sel, i);
      else drawMatrixCell(x, y, cw, ch, m, col, i);
    }
  }

  function drawMeterCell(x, y, w, m, col, sel, i) {
    const segs = 14;
    const ix = x + 3, iw = w - 6, iy = y + 4, ih = 44;
    const segH = (ih - (segs - 1)) / segs;
    const lit = Math.round(meterPos(m[PEAK]) * segs);
    const hold = Math.round(meterPos(m[HOLD]) * segs);
    for (let s = 0; s < segs; s++) {
      const y1 = iy + ih - s * (segH + 1);
      const on = s < lit || (s + 1 === hold && hold > 0);
      rect(ix, y1 - segH, iw, segH, !on ? C.segOff : s >= segs - 1 ? C.clip : s >= segs - 3 ? C.hot : col);
    }
    if (m[STATE] === "Silent" || m[STATE] === "Drop") rect(ix, iy + ih - segH, iw, segH, col);
    ctx.font = "9px ui-monospace, Menlo, Consolas, monospace";
    ctx.fillStyle = sel ? C.text : C.label;
    ctx.fillText(String(i + 1), x + w / 2, y + 58);
  }

  function drawMatrixCell(x, y, w, h, m, col, i) {
    ctx.globalAlpha = 0.4;
    rect(x + 1, y + 1, Math.max(0, w * meterPos(m[PEAK]) - 2), h - 2, col);
    ctx.globalAlpha = 1;
    ctx.font = "10px ui-monospace, Menlo, Consolas, monospace";
    ctx.textAlign = "left";
    ctx.fillStyle = m[STATE] === "Silent" ? C.label : C.text;
    ctx.fillText(String(i + 1), x + 6, y + h / 2);
    ctx.textAlign = "center";
    ctx.beginPath();
    ctx.arc(x + w - 8, y + h / 2, 2.5, 0, Math.PI * 2);
    ctx.fillStyle = m[STATE] === "Silent" ? "#454c58" : col;
    ctx.fill();
  }

  function channelAt(ev) {
    if (!layout) return -1;
    const b = canvas.getBoundingClientRect();
    const px = ev.clientX - b.left, py = ev.clientY - b.top;
    const { idx, cols, cw, ch, gap } = layout;
    const c = Math.floor(px / (cw + gap)), r = Math.floor(py / (ch + gap));
    if (c < 0 || c >= cols || px - c * (cw + gap) > cw || py - r * (ch + gap) > ch) return -1;
    const k = r * cols + c;
    return k >= 0 && k < idx.length ? idx[k] : -1;
  }

  canvas.addEventListener("click", (ev) => {
    const i = channelAt(ev);
    if (i >= 0) {
      selected = i;
      draw();
      renderSide(MC.status());
    }
  });
  canvas.addEventListener("mousemove", (ev) => {
    const i = channelAt(ev);
    if (i === hover) return;
    hover = i;
    const s = MC.status();
    canvas.title = i >= 0 && s ? `Ch ${ch3(i)} ${s.channel_names[i] ?? ""} · ${db(s.meters[i][HOLD])} dB` : "";
    draw();
  });
  canvas.addEventListener("mouseleave", () => {
    hover = -1;
    draw();
  });
  wrap.addEventListener("scroll", draw, { passive: true });
  window.addEventListener("resize", draw);

  // arrow keys move the selection
  document.addEventListener("keydown", (e) => {
    const s = MC.status();
    if (!s || !layout || e.target?.tagName === "INPUT") return;
    const step = { ArrowLeft: -1, ArrowRight: 1, ArrowUp: -layout.cols, ArrowDown: layout.cols }[e.key];
    if (!step) return;
    e.preventDefault();
    const { idx } = layout;
    const k = Math.max(0, idx.indexOf(selected));
    const next = idx[Math.min(idx.length - 1, Math.max(0, k + step))];
    if (next !== undefined) {
      selected = next;
      draw();
      renderSide(s);
    }
  });

  function setView(v) {
    view = v;
    $("view-wall").classList.toggle("active", v === "wall");
    $("view-matrix").classList.toggle("active", v === "matrix");
    draw();
  }
  $("view-wall").addEventListener("click", () => setView("wall"));
  $("view-matrix").addEventListener("click", () => setView("matrix"));
  $("faults-only").addEventListener("change", (e) => {
    faultsOnly = e.target.checked;
    draw();
  });
  $("show-faults").addEventListener("click", () => {
    faultsOnly = true;
    $("faults-only").checked = true;
    draw();
  });

  // ---- fault bar ----------------------------------------------------------------
  function list(v) {
    const shown = v.slice(0, 6).join(", ");
    return v.length > 6 ? `${shown} +${v.length - 6}` : shown;
  }
  function renderFaultBar(s) {
    const drops = [], clips = [];
    s.meters.forEach((m, i) => {
      if (m[STATE] === "Drop") drops.push(i + 1);
      if (m[STATE] === "Clip") clips.push(i + 1);
    });
    const parts = [];
    if (drops.length) parts.push(`Ch ${list(drops)} disk write dropped`);
    if (clips.length) parts.push(`Ch ${list(clips)} clipping`);
    if (s.silent_count > 0) parts.push(`${s.silent_count} channels with no signal`);
    for (const d of s.disks) if (d.failed) parts.push(`Disk ${d.path} failed`);
    $("faultbar").classList.toggle("hidden", parts.length === 0 || faultsOnly);
    setText("faulttext", parts.join(" · "));
  }

  // ---- selected channel -------------------------------------------------------------
  function renderSide(s) {
    if (!s || !s.meters.length) return;
    if (selected >= s.meters.length) selected = s.meters.length - 1;
    const m = s.meters[selected];
    const rec = s.state === "Recording";
    setText("sel-num", ch3(selected));
    setText("sel-name", s.channel_names[selected] ?? "");
    const [cls, label] = stateLabel(m[STATE], rec);
    const pill = $("sel-state");
    pill.className = "pill " + cls;
    setText("sel-state", label);

    setText("sel-peak", db(m[HOLD]) + " dB");
    $("sel-peak").style.color = m[HOLD] > -0.5 ? "var(--fault-text)" : m[HOLD] > -3 ? "var(--hot-text)" : "";
    setText("sel-rms", db(m[RMS]) + " dB");
    setText("sel-clips", String(m[CLIPS]));
    $("sel-clips").style.color = m[CLIPS] > 0 ? "var(--fault-text)" : "";
    setText("sel-drops", MC.thousands(m[DROPPED]));
    $("sel-drops").style.color = m[DROPPED] > 0 ? "var(--drop-text)" : "";
    drawSelMeter(m);

    const path = s.channel_paths[selected];
    if (path) {
      setText("sel-path", path);
    } else {
      const d = s.disks.find((d) => d.first_channel != null && selected >= d.first_channel && selected < d.first_channel + d.count);
      setText("sel-path", d ? `${d.path} (planned)` : "no destination planned");
    }

    // write rate from byte deltas over at least a second
    const bytes = s.channel_bytes[selected] ?? 0;
    const now = performance.now();
    if (rate.ch !== selected || !rec) {
      Object.assign(rate, { bytes, t: now, ch: selected, bps: 0 });
    } else if (now - rate.t >= 1000) {
      rate.bps = Math.max(0, (bytes - rate.bytes) / ((now - rate.t) / 1000));
      rate.bytes = bytes;
      rate.t = now;
    }
    setText("sel-bytes", bytesHuman(bytes));
    setText("sel-rate", rateHuman(rate.bps));
  }

  const selCanvas = $("selmeter");
  const sctx = selCanvas.getContext("2d");
  function drawSelMeter(m) {
    const w = 26, h = 150, segs = 26;
    sctx.clearRect(0, 0, selCanvas.width, selCanvas.height);
    const segH = (h - (segs - 1) * 2) / segs;
    const lit = Math.round(meterPos(m[PEAK]) * segs);
    const hold = Math.round(meterPos(m[HOLD]) * segs);
    for (let i = 0; i < segs; i++) {
      const y1 = h - i * (segH + 2);
      const on = i < lit || (i + 1 === hold && hold > 0);
      sctx.fillStyle = !on ? C.segOff : i >= segs - 2 ? C.clip : i >= segs - 6 ? C.hot : C.greenMeter;
      sctx.fillRect(0, y1 - segH, w, segH);
    }
    sctx.font = "8px ui-monospace, Menlo, Consolas, monospace";
    sctx.fillStyle = C.labelFaint;
    sctx.textBaseline = "middle";
    ["0", "-6", "-18", "-36", "-∞"].forEach((l, i) => sctx.fillText(l, w + 4, 4 + (h - 8) * i / 4));
  }

  // ---- event log ----------------------------------------------------------------------
  let lastEventsKey = "";
  function renderEvents(s) {
    const ev = s.events;
    const key = ev.length + "|" + (ev.length ? ev[ev.length - 1].time + ev[ev.length - 1].message : "");
    if (key === lastEventsKey) return;
    lastEventsKey = key;
    if (!ev.length) {
      $("events").innerHTML = '<span class="faint">no events yet</span>';
      return;
    }
    $("events").innerHTML = ev
      .slice()
      .reverse()
      .map((e) => {
        const cls = e.kind === "Drop" || e.kind === "TakeStarted" ? e.kind : e.severity;
        const t = e.elapsed != null ? hms(e.elapsed) : e.time;
        return `<div><span class="t">${esc(t)}</span><span class="${cls}">${esc(e.message)}</span></div>`;
      })
      .join("");
  }

  // ---- destinations ---------------------------------------------------------------------
  let lastDiskHtml = "";
  let testGbSet = false;
  function renderDisks(s) {
    const ui = s.ui;
    const rec = s.state === "Recording";
    const busy = !!ui.measuring;
    const lock = rec || busy ? " disabled" : "";
    const rows = ui.destinations.map((d) => {
      const st = s.disks.find((x) => x.path === d.path);
      const failed = st?.failed;
      const range = st && st.first_channel != null && st.count > 0 ? `${ch3(st.first_channel)}-${ch3(st.first_channel + st.count - 1)}` : "—";
      const tp = d.throughput_bps != null ? rateHuman(d.throughput_bps) : '<span class="unmeasured">unmeasured</span>';
      const p = esc(d.path);
      return `<tr class="${failed ? "failed" : ""}">
        <td class="p">${p}${failed ? " · FAILED" : ""}</td>
        <td>${st ? bytesHuman(st.free_bytes) : "—"}</td>
        <td>${tp}</td>
        <td>${range}</td>
        <td>${st ? bytesHuman(st.bytes_written) : "—"}</td>
        <td>${st ? rateHuman(st.write_rate_bps) : "—"}</td>
        <td><button class="chip" data-act="measure" data-path="${p}"${lock}>Measure</button> <button class="chip" data-act="remove" data-path="${p}"${lock}>Remove</button></td>
      </tr>`;
    });
    const html = rows.length ? rows.join("") : '<tr><td colspan="7" class="faint">No destinations yet. Add a folder on each recording disk, then measure it.</td></tr>';
    if (html !== lastDiskHtml) {
      $("disk-rows").innerHTML = html;
      lastDiskHtml = html;
    }
    const ok = s.disks.filter((d) => !d.failed).length;
    setText("disk-summary", `${ui.destinations.length} configured · ${ok} usable`);
    $("add-path").disabled = rec;
    $("add-btn").disabled = rec || busy;
    if (!testGbSet) {
      $("test-gb").value = Math.round(ui.disk_test_gb);
      testGbSet = true;
    }

    const m = ui.measuring;
    $("measure").classList.toggle("hidden", !m);
    if (m) {
      setText("measure-text", `measuring ${m.path} …`);
      $("measure-fill").style.width = (100 * m.done / Math.max(1, m.total)).toFixed(1) + "%";
      setText("measure-bytes", `${bytesHuman(m.done)} / ${bytesHuman(m.total)}`);
    }
    setText("disk-msg", ui.last_message ?? "");
  }

  $("disk-rows").addEventListener("click", (e) => {
    const b = e.target.closest("button[data-act]");
    if (!b || b.disabled) return;
    const path = b.dataset.path;
    if (b.dataset.act === "measure") {
      const gb = parseFloat($("test-gb").value);
      api("POST", "/api/destinations/measure", { path, test_gb: gb > 0 ? gb : null }).catch(() => {});
    } else if (confirm(`Remove ${path} from the destinations? Files already recorded there are kept.`)) {
      api("DELETE", "/api/destinations?path=" + encodeURIComponent(path)).catch(() => {});
    }
  });
  function addDest() {
    const path = $("add-path").value.trim();
    if (!path) return;
    api("POST", "/api/destinations", { path }).then(() => {
      $("add-path").value = "";
      toast("Added " + path + "; measure it before recording");
    }).catch(() => {});
  }
  $("add-btn").addEventListener("click", addDest);
  $("add-path").addEventListener("keydown", (e) => {
    if (e.key === "Enter") addDest();
  });

  // ---- update loop ---------------------------------------------------------------------
  MC.on((s) => {
    draw();
    renderFaultBar(s);
    renderSide(s);
    renderEvents(s);
    renderDisks(s);
  });
})();
