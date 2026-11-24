// Shared by both pages: status stream, formatting, transport controls, keyboard.
"use strict";

const MC = (() => {
  const $ = (id) => document.getElementById(id);
  let status = null;
  let names = [];
  let paths = [];
  let armedAt = 0; // stop needs a second press within 2 s
  const listeners = [];

  // ---- formatting ---------------------------------------------------------
  const pad = (n) => String(n).padStart(2, "0");
  function hms(sec) {
    const s = Math.max(0, Math.floor(sec || 0));
    return `${pad(Math.floor(s / 3600))}:${pad(Math.floor(s / 60) % 60)}:${pad(s % 60)}`;
  }
  const thousands = (n) => Math.round(n).toLocaleString("en-US");
  function bytesHuman(b) {
    if (b >= 1e12) return (b / 1e12).toFixed(2) + " TB";
    if (b >= 1e9) return thousands(b / 1e9) + " GB";
    if (b >= 1e6) return (b / 1e6).toFixed(1) + " MB";
    if (b >= 1e3) return (b / 1e3).toFixed(1) + " kB";
    return b + " B";
  }
  function rateHuman(bps) {
    if (bps >= 1e9) return (bps / 1e9).toFixed(2) + " GB/s";
    if (bps >= 1e6) return (bps / 1e6).toFixed(1) + " MB/s";
    return (bps / 1e3).toFixed(0) + " kB/s";
  }
  const db = (v) => (v <= -100 ? "−∞" : v.toFixed(1));
  const ch3 = (i) => String(i + 1).padStart(3, "0");
  const esc = (s) => String(s ?? "").replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c]);

  function setText(id, text) {
    const el = $(id);
    if (el && el.textContent !== text) el.textContent = text;
  }

  // ---- API ----------------------------------------------------------------
  let toastTimer = 0;
  function toast(msg, err) {
    const t = $("toast");
    if (!t) return;
    t.textContent = msg;
    t.className = "toast" + (err ? " err" : "");
    clearTimeout(toastTimer);
    toastTimer = setTimeout(() => t.classList.add("hidden"), 4000);
  }

  async function api(method, path, body) {
    const opts = { method };
    if (body !== undefined) {
      opts.headers = { "Content-Type": "application/json" };
      opts.body = JSON.stringify(body);
    }
    let r;
    try {
      r = await fetch(path, opts);
    } catch (e) {
      toast("Recorder not reachable", true);
      throw e;
    }
    if (!r.ok) {
      const t = (await r.text()) || r.statusText;
      toast(t, true);
      throw new Error(t);
    }
    return r;
  }

  // ---- transport ------------------------------------------------------------
  const recording = () => status?.state === "Recording";
  const armed = () => armedAt && Date.now() - armedAt < 2000;

  function toggleRecord() {
    if (!status) return;
    if (recording()) {
      if (armed()) {
        armedAt = 0;
        api("POST", "/api/record/stop").catch(() => {});
      } else {
        armedAt = Date.now();
      }
    } else {
      armedAt = 0;
      api("POST", "/api/record/start").catch(() => {});
    }
    render();
  }

  function mark() {
    if (recording()) api("POST", "/api/marker", { label: "" }).then(() => toast("Marker placed")).catch(() => {});
  }

  function health(s) {
    const faults = s.drops_total > 0 || s.clips_total > 0 || s.silent_count > 0 || s.disks.some((d) => d.failed);
    if (s.ui.backend_error) return ["fault", "Input not available"];
    if (s.not_ready && s.state !== "Recording") return ["warn", "Not ready · " + s.not_ready];
    if (faults) return ["fault", "Faults present"];
    return ["ok", "All channels nominal"];
  }

  function render() {
    const s = status;
    if (!s) return;
    const rec = recording();
    document.body.classList.toggle("is-recording", rec);

    const failed = s.ui.backend_error;
    $("failed")?.classList.toggle("hidden", !failed);
    if (failed) setText("failed-msg", failed);

    const [cls, text] = health(s);
    const pill = $("health");
    if (pill) {
      pill.className = "pill " + cls;
      setText("health", text);
    }

    const btn = $("rec");
    if (btn) {
      btn.classList.toggle("recording", rec);
      btn.classList.toggle("armed", rec && !!armed());
      btn.disabled = !!failed || (!rec && !!s.not_ready);
      setText("rec-label", rec ? (armed() ? "PRESS AGAIN" : "STOP") : "RECORD");
      btn.title = !rec && s.not_ready ? "Cannot record: " + s.not_ready : rec ? "Press twice (or Ctrl+R twice) within 2 s to stop" : "Start recording (Ctrl+R)";
    }
    const mk = $("mark");
    if (mk) mk.disabled = !rec;

    $("clock")?.classList.toggle("recording", rec);
    setText("elapsed", hms(s.elapsed_seconds));
    setText("take", s.take_name ? `${s.take_name} · ${s.channels} ch · ${s.sample_rate / 1000} kHz` : `${s.channels} ch · ${s.sample_rate / 1000} kHz · standby`);
    setText("remaining", s.remaining_seconds == null ? "—" : hms(s.remaining_seconds));
    setText("free", thousands(s.free_bytes_total / 1e9) + " GB");
    setText("cache", Math.round(s.cache_fill * 100) + "%");
    $("cache")?.classList.toggle("warn", s.cache_fill > 0.5);
    setText("drops", thousands(s.drops_total));
    $("drops")?.classList.toggle("drop", s.drops_total > 0);
    setText("clips", thousands(s.clips_total));
    $("clips")?.classList.toggle("bad", s.clips_total > 0);
    setText("silent", String(s.silent_count));
    $("silent")?.classList.toggle("warn", s.silent_count > 0);
    setText("markers", String(s.markers));
    setText("device", s.device_name);

    const sess = $("session");
    if (sess && document.activeElement !== sess) {
      sess.value = s.session;
    }
    if (sess) sess.disabled = rec;
  }

  // ---- stream -------------------------------------------------------------
  function connect() {
    const es = new EventSource("/api/status/stream");
    es.onopen = () => {
      setText("conn", "● live");
      $("conn")?.classList.remove("down");
    };
    es.onerror = () => {
      setText("conn", "● reconnecting…");
      $("conn")?.classList.add("down");
    };
    es.onmessage = (e) => {
      const s = JSON.parse(e.data);
      if (s.channel_names) names = s.channel_names;
      if (s.channel_paths) paths = s.channel_paths;
      s.channel_names = names;
      s.channel_paths = paths;
      status = s;
      render();
      for (const f of listeners) f(s);
    };
  }

  // ---- wiring -----------------------------------------------------------------
  function init() {
    $("rec")?.addEventListener("click", toggleRecord);
    $("mark")?.addEventListener("click", mark);
    $("use-sim")?.addEventListener("click", () => api("POST", "/api/use-sim").catch(() => {}));

    const sess = $("session");
    if (sess) {
      const commit = () => {
        const name = sess.value.trim();
        if (status && name && name !== status.session) {
          api("POST", "/api/session", { name }).then(() => toast("Session set to " + name)).catch(() => (sess.value = status.session));
        }
      };
      sess.addEventListener("change", commit);
      sess.addEventListener("keydown", (e) => {
        if (e.key === "Enter") sess.blur();
        if (e.key === "Escape") {
          sess.value = status?.session ?? "";
          sess.blur();
        }
      });
    }

    document.addEventListener("keydown", (e) => {
      if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "r") {
        e.preventDefault();
        toggleRecord();
        return;
      }
      const tag = e.target?.tagName;
      if (e.key === " " && tag !== "INPUT" && tag !== "TEXTAREA" && tag !== "SELECT") {
        e.preventDefault(); // also stops Space from "clicking" a focused button
        mark();
      }
    });

    // the "press again" state times out without a status update
    setInterval(() => {
      if (armedAt && !armed()) {
        armedAt = 0;
        render();
      }
    }, 250);

    connect();
  }

  return {
    $, esc, hms, bytesHuman, rateHuman, thousands, db, ch3, setText, api, toast, init,
    on: (f) => listeners.push(f),
    status: () => status,
    recording,
  };
})();
