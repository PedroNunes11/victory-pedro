// Vertex Launcher v3 (Vertex-AXX Edition)
// - Real Performance / Balanced / High Quality modes
// - GTA V-style Native Controls Menu (live reload in <=1 s)
// - Win+Alt+V global In-Game Menu with Live vs Requires-Restart enforcement
"use strict";

(function () {
  const S = window.STR;
  const fmt = window.fmt;
  const invoke = (cmd, args) => window.__TAURI__.core.invoke(cmd, args);

  function h(tag, attrs, ...kids) {
    const el = document.createElement(tag);
    if (attrs) {
      for (const [k, v] of Object.entries(attrs)) {
        if (v == null || v === false) continue;
        if (k === "class") el.className = v;
        else if (k === "text") el.textContent = v;
        else if (k.startsWith("on")) el.addEventListener(k.slice(2), v);
        else el.setAttribute(k, v === true ? "" : v);
      }
    }
    for (const kid of kids.flat()) {
      if (kid == null || kid === false) continue;
      el.append(kid instanceof Node ? kid : document.createTextNode(String(kid)));
    }
    return el;
  }

  const ICONS = {
    play: ["M7 4.5v15l12-7.5z", true],
    stop: ["M6.5 6.5h11v11h-11z", true],
    graphics: ["M3 5h18v12H3z M8 21h8 M12 17v4"],
    controls: ["M3 7h18v10H3z M7 10.5h.01 M10.5 10.5h.01 M14 10.5h.01 M17.5 10.5h.01 M7 14h10"],
    engine: ["M4 17a8 8 0 1 1 16 0 M12 17l4.5-5.5 M7 17h.01 M17 17h.01"],
    cache: ["M13 2.5L4.5 14H11l-1 7.5L19 10h-6.5z"],
    advanced: ["M12 9a3 3 0 1 0 0 6a3 3 0 1 0 0-6 M12 2.5v3 M12 18.5v3 M2.5 12h3 M18.5 12h3"],
    check: ["M5 12.5l4.5 4.5L19 7.5"],
    warn: ["M12 3.5l9.5 16.5h-19z M12 10v4.5 M12 17.2h.01"],
    info: ["M12 3a9 9 0 1 0 0 18a9 9 0 1 0 0-18 M12 11v5.5 M12 7.7h.01"],
    plus: ["M12 5v14 M5 12h14"],
    folder: ["M3 6.5h6l2 2h10V19H3z"],
    bolt: ["M13 2.5L4.5 14H11l-1 7.5L19 10h-6.5z"],
    gear: ["M12 8.5a3.5 3.5 0 1 0 0 7a3.5 3.5 0 1 0 0-7 M19.4 13.5l1.6 1.2-2 3.4-1.9-.7a7 7 0 0 1-2 1.2L14.8 21h-4l-.3-2.4a7 7 0 0 1-2-1.2l-1.9.7-2-3.4 1.6-1.2a7 7 0 0 1 0-2.4L4.6 9.9l2-3.4 1.9.7a7 7 0 0 1 2-1.2L10.8 3h4l.3 2.4a7 7 0 0 1 2 1.2l1.9-.7 2 3.4-1.6 1.2a7 7 0 0 1 0 2.4z"],
    sound: ["M4 9.5h4l5-4v13l-5-4H4z M16.5 9a4 4 0 0 1 0 6 M19 6.5a7.5 7.5 0 0 1 0 11"],
    muted: ["M4 9.5h4l5-4v13l-5-4H4z M16.5 9.5l5 5 M21.5 9.5l-5 5"],
    back: ["M15 5l-7 7 7 7"],
    spark: ["M12 3v4 M12 17v4 M3 12h4 M17 12h4 M6 6l2.5 2.5 M15.5 15.5L18 18 M6 18l2.5-2.5 M15.5 8.5L18 6"],
  };

  function icon(name, width) {
    const [d, filled] = ICONS[name] || ICONS.info;
    const ns = "http://www.w3.org/2000/svg";
    const svg = document.createElementNS(ns, "svg");
    svg.setAttribute("viewBox", "0 0 24 24");
    svg.setAttribute("fill", filled ? "currentColor" : "none");
    svg.setAttribute("stroke", filled ? "none" : "currentColor");
    svg.setAttribute("stroke-width", width || "1.9");
    svg.setAttribute("stroke-linecap", "round");
    svg.setAttribute("stroke-linejoin", "round");
    svg.setAttribute("aria-hidden", "true");
    const path = document.createElementNS(ns, "path");
    path.setAttribute("d", d);
    svg.append(path);
    return svg;
  }

  function tip(text) {
    if (!text) return null;
    return h("button", { class: "tip", type: "button", "aria-label": text, "data-tip": text, "data-sfx": "none" }, "i");
  }

  const $ = (sel) => document.querySelector(sel);
  const gb = (mb) => fmt(S.common.gb, { v: (mb / 1024).toFixed(1) });
  const gbWhole = (mb) => fmt(S.common.gb, { v: Math.round(mb / 1024) });
  const clamp = (v, lo, hi) => Math.min(hi, Math.max(lo, v));
  const baseName = (p) => String(p).split(/[\\/]/).pop();
  const dirName = (p) => String(p).replace(/[\\/][^\\/]*$/, "");

  function time(secs) {
    const hh = Math.floor(secs / 3600);
    const m = Math.floor((secs % 3600) / 60);
    const s = secs % 60;
    const pad = (n) => String(n).padStart(2, "0");
    return hh ? `${hh}:${pad(m)}:${pad(s)}` : `${m}:${pad(s)}`;
  }

  let toastTimer = null;
  function toast(text, bad) {
    const el = $("#toast");
    el.textContent = text;
    el.className = "toast" + (bad ? " bad" : "");
    clearTimeout(toastTimer);
    toastTimer = setTimeout(() => el.classList.add("hidden"), bad ? 6500 : 3400);
    if (bad) sound.sfx("error");
  }

  function errText(e) {
    const code = String(e);
    const known = {
      no_game: S.common.noGame,
      no_victory: S.home.engineNone,
      busy: S.common.busy,
      missing: S.common.missing,
    };
    return known[code] || fmt(S.common.error, { e: code });
  }

  // ------------------------------------------------------------------ state

  const st = {
    settingsOpen: false,
    ingameOverlay: false,
    page: "graphics",
    start: null,
    profile: null,
    launchedProfileSnapshot: null,
    restartNeeded: false,
    restartPromptShownForThisChange: false,
    victory: null,
    dlssAvailable: false,
    paths: null,
    hotkey: { spec: "Win+Alt+V", error: null },
    system: null,
    nativeControls: null,
    controlsCategory: "All",
    controlsSearch: "",
    controlsBoundOnly: true,
    cacheInfo: null,
    lossless: null,
    tune: null,
    doctor: null,
    doctorBusy: false,
    logSize: -1,
    run: null,
    capture: null,
    startAfterPrebuild: false,
    sessionMute: false,
  };

  const logEl = h("div", { class: "log", role: "log", "aria-live": "off" });

  function isGameRunning() {
    return !!(st.run && st.run.running && st.run.kind === "play");
  }

  async function saveNow(isRestartSetting = false) {
    clearTimeout(saveTimer);
    const r = await invoke("save_profile", { profile: st.profile });
    st.victory = r.victory;
    st.dlssAvailable = r.dlss_available;
    st.paths = r.paths;
    st.restartNeeded = r.restart_needed;
    updateIngameBar();
    if (isRestartSetting && isGameRunning() && r.restart_needed) {
      promptRestartIfRunning();
    }
    return r;
  }

  let saveTimer = null;
  function changed(rerender = true, isRestartSetting = false) {
    clearTimeout(saveTimer);
    if (isRestartSetting && isGameRunning()) {
      // Save immediately and prompt user about restart cleanly.
      saveNow(true).catch((e) => toast(errText(e), true));
    } else {
      saveTimer = setTimeout(() => saveNow(false).catch((e) => toast(errText(e), true)), 220);
    }
    if (rerender) render();
  }

  function promptRestartIfRunning() {
    if (!isGameRunning() || st.capture) return;
    choose({
      iconName: "warn",
      title: S.ingame.restartTitle,
      lines: [S.ingame.restartBody],
      choices: [
        [S.ingame.saveAndRestart, true, async () => {
          try {
            await saveNow(false);
            toast("Restarting game with new settings…");
            await invoke("restart_game");
            st.restartNeeded = false;
            st.launchedProfileSnapshot = JSON.stringify(st.profile);
            updateIngameBar();
            render();
          } catch (e) {
            toast(errText(e), true);
          }
        }],
        [S.ingame.saveNextLaunch, false, () => {
          toast("Saved for next launch.");
          updateIngameBar();
          render();
        }],
        [S.ingame.discard, false, async () => {
          if (st.launchedProfileSnapshot) {
            st.profile = JSON.parse(st.launchedProfileSnapshot);
            await saveNow(false);
            st.restartNeeded = false;
            toast("Changes discarded.");
            render();
          }
        }],
      ],
    });
  }

  // ------------------------------------------------------------------ sound

  const ASSET = (name) => "assets/" + name;
  const sound = {
    music: null, fade: null, lastHover: 0,
    off() { return !st.profile || st.profile.muted || st.sessionMute; },
    level() { return st.profile ? clamp(st.profile.music_volume, 0, 100) / 100 : 0.35; },
    init() {
      try {
        const m = new Audio(ASSET("lobby-theme.mp3"));
        m.loop = true; m.preload = "auto"; m.volume = 0;
        m.addEventListener("error", () => { this.music = null; });
        this.music = m;
      } catch { this.music = null; }
    },
    fadeTo(target, ms, then) {
      const m = this.music;
      if (!m) return;
      clearInterval(this.fade);
      const from = m.volume, t0 = performance.now();
      this.fade = setInterval(() => {
        const k = clamp((performance.now() - t0) / ms, 0, 1);
        m.volume = clamp(from + (target - from) * k, 0, 1);
        if (k >= 1) { clearInterval(this.fade); if (then) then(); }
      }, 40);
    },
    play() {
      const m = this.music;
      if (!m || this.off() || document.body.classList.contains("game-on")) return;
      if (!m.paused) { this.fadeTo(this.level(), 300); return; }
      m.volume = 0;
      m.play().then(() => this.fadeTo(this.level(), 1400)).catch(() => {
        const once = () => { window.removeEventListener("pointerdown", once, true); this.play(); };
        window.addEventListener("pointerdown", once, true);
      });
    },
    pause(ms) {
      const m = this.music;
      if (!m || m.paused) return;
      this.fadeTo(0, ms || 400, () => m.pause());
    },
    setLevel() {
      const m = this.music;
      if (!m) return;
      clearInterval(this.fade);
      if (this.off()) this.pause(200);
      else if (m.paused) this.play();
      else m.volume = this.level();
    },
    sfx(name, gain) {
      if (this.off() || document.body.classList.contains("game-on")) return;
      const v = clamp(this.level() * 0.6 * (gain || 1), 0, 1);
      if (v <= 0) return;
      try { const a = new Audio(ASSET(name + ".mp3")); a.volume = v; a.play().catch(() => {}); } catch {}
    },
  };

  document.addEventListener("pointerover", (e) => {
    const t = e.target.closest && e.target.closest("button:not(:disabled), .tile, .mode-card");
    if (!t || t.contains(e.relatedTarget) || t.dataset.sfx === "none") return;
    const now = performance.now();
    if (now - sound.lastHover < 70) return;
    sound.lastHover = now;
    sound.sfx("hover", 0.35);
  });
  document.addEventListener("click", (e) => {
    const b = e.target.closest && e.target.closest("button");
    if (!b || b.disabled) return;
    let kind = b.dataset.sfx || "click";
    if (b.classList.contains("switch")) kind = b.getAttribute("aria-checked") === "true" ? "click" : "toggle";
    if (kind !== "none") sound.sfx(kind, kind === "click" ? 0.7 : 1);
  }, true);

  function renderAudio() {
    const p = st.profile;
    const mute = $("#mute");
    const off = p.muted || st.sessionMute;
    mute.replaceChildren(icon(off ? "muted" : "sound"));
    mute.setAttribute("aria-label", off ? S.home.unmute : S.home.mute);
    const vol = $("#volume");
    vol.value = p.music_volume;
    vol.style.setProperty("--fill", p.music_volume + "%");
  }

  // ---------------------------------------------------------------- tooltip

  function showTip(el) {
    const box = $("#tooltip");
    box.textContent = el.dataset.tip;
    box.classList.remove("hidden");
    const r = el.getBoundingClientRect();
    const bw = box.offsetWidth, bh = box.offsetHeight;
    let x = r.left + r.width / 2 - 24;
    let y = r.bottom + 8;
    if (x + bw > window.innerWidth - 10) x = window.innerWidth - bw - 10;
    if (y + bh > window.innerHeight - 10) y = r.top - bh - 8;
    box.style.left = Math.max(10, x) + "px";
    box.style.top = Math.max(10, y) + "px";
  }
  const hideTip = () => $("#tooltip").classList.add("hidden");
  document.addEventListener("mouseover", (e) => { const t = e.target.closest && e.target.closest("[data-tip]"); if (t) showTip(t); });
  document.addEventListener("mouseout", (e) => { const t = e.target.closest && e.target.closest("[data-tip]"); if (t && !t.contains(e.relatedTarget)) hideTip(); });

  // ------------------------------------------------------------- UI helpers

  const liveBadge = () => h("span", { class: "badge live-pill" }, S.ingame.liveBadge);
  const restartBadge = () => isGameRunning() ? h("span", { class: "badge restart-pill" }, S.ingame.restartBadge) : null;

  function head(title, lead, badge) {
    return h("div", { class: "page-head" }, h("h1", null, title, badge), lead && h("p", { class: "lead", text: lead }));
  }

  function card(title, tipText, ...kids) {
    return h("section", { class: "card" }, title && h("h2", null, title, tip(tipText)), ...kids);
  }

  function notice(kind, text) {
    const ic = kind === "ok" ? "check" : kind === "warn" || kind === "bad" ? "warn" : "info";
    return h("div", { class: "notice " + (kind || "") }, icon(ic), h("div", { text }));
  }

  function tiles(options, value, onPick, cls) {
    return h(
      "div",
      { class: "tiles " + (cls || "") },
      options.map((o) =>
        h(
          "button",
          {
            class: "tile" + (o.value === value ? " on" : ""),
            "aria-pressed": o.value === value ? "true" : "false",
            disabled: o.disabled,
            onclick: () => onPick(o.value),
          },
          h("div", { class: "tile-title" }, o.title, o.badge && h("span", { class: "badge " + (o.badgeKind || "") }, o.badge)),
          o.text && h("div", { class: "tile-text", text: o.text }),
        ),
      ),
    );
  }

  function toggle(on, onChange, label, disabled) {
    return h(
      "button",
      {
        class: "switch" + (on ? " on" : ""),
        role: "switch",
        "aria-checked": on ? "true" : "false",
        "aria-label": label,
        disabled,
        onclick: () => onChange(!on),
      },
      h("span", { class: "track" }),
      h("span", { class: "knob" }),
    );
  }

  function seg(options) {
    return h(
      "div",
      { class: "seg", role: "group" },
      options.map(([label, on, act]) => h("button", { class: on ? "on" : "", onclick: act, "aria-pressed": on ? "true" : "false" }, label)),
    );
  }

  function settingRow(triple, badges, control) {
    const [label, line, more] = triple;
    return h(
      "div",
      { class: "setting" },
      h("div", null, h("div", { class: "setting-label" }, label, tip(more), badges), line && h("div", { class: "setting-desc", text: line })),
      h("div", { class: "setting-control" }, control),
    );
  }

  function sliderRow({ triple, value, min, max, step, unit, badges, onInput, onCommit }) {
    const valueEl = h("span", { class: "value" });
    const range = h("input", { type: "range", min, max, step, "aria-label": triple[0] });
    range.value = clamp(value, min, max);
    const show = (v) => {
      valueEl.textContent = fmt(unit, { v: step < 1 ? Number(v).toFixed(2) : Math.round(v) });
      range.style.setProperty("--fill", ((v - min) / (max - min)) * 100 + "%");
    };
    show(Number(range.value));
    range.addEventListener("input", () => {
      const v = Number(range.value);
      show(v);
      if (onInput) onInput(v);
    });
    range.addEventListener("change", () => {
      if (onCommit) onCommit(Number(range.value));
    });
    return settingRow(triple, badges, h("div", { class: "slider-row" }, range, valueEl));
  }

  function boolSetting(triple, on, onChange, badges) {
    return settingRow(triple, badges, h("div", { class: "row end" }, toggle(on, onChange, triple[0])));
  }

  function updateIngameBar() {
    const bar = $("#ingame-bar");
    const running = isGameRunning();
    const show = st.ingameOverlay || running;
    bar.classList.toggle("hidden", !show);
    document.body.classList.toggle("ingame-open", show);
    if (!show) return;
    const r = st.run;
    $("#ingame-fps").textContent = r && r.fps != null ? `${Math.round(r.fps)} fps · ${r.ms != null ? r.ms.toFixed(0) + " ms" : ""}` : "Game Running";
    $("#ingame-hint").textContent = fmt(S.ingame.hint, { key: st.hotkey.spec || "Win+Alt+V" });
    $("#resume-game-btn").textContent = S.ingame.resume;
    const restartBtn = $("#ingame-restart-btn");
    restartBtn.textContent = S.ingame.restartNow;
    restartBtn.classList.toggle("hidden", !st.restartNeeded);
  }

  // ------------------------------------------------------------------- home

  async function applyMode(modeKey) {
    try {
      st.profile = await invoke("apply_preset", { preset: modeKey });
      await saveNow(true);
      const label = S.home.modes[modeKey].title;
      if (!isGameRunning()) toast(`${label} mode active.`);
      render();
    } catch (e) {
      toast(errText(e), true);
    }
  }

  function renderHome() {
    const p = st.profile;
    const H = S.home;
    const run = st.run;
    const running = !!(run && run.running);
    const v = st.victory || { path: null, how: "none" };

    const game = h(
      "div",
      { class: "field" },
      h("div", { class: "label" }, H.game, tip(H.gameTip)),
      h(
        "div",
        { class: "game-row" + (p.game_exe ? "" : " empty") },
        h(
          "div",
          { class: "game-text selectable", title: p.game_exe || "" },
          h("div", { class: "game-name" }, p.game_exe ? baseName(p.game_exe) : H.gameNone),
          p.game_exe && h("div", { class: "game-dir" }, dirName(p.game_exe)),
        ),
        h("button", { class: "btn glass small", disabled: running, onclick: pickGame }, icon("folder"), p.game_exe ? H.change : H.choose),
      ),
    );

    const modeKeys = ["performance", "balanced", "high-quality", "native-4k"];
    const modeField = h(
      "div",
      { class: "field" },
      h("div", { class: "label" }, H.mode, tip(H.modeTip), restartBadge()),
      h(
        "div",
        { class: "mode-grid" },
        modeKeys.map((mk) => {
          const m = H.modes[mk];
          const on = p.preset === mk;
          return h(
            "button",
            {
              class: "mode-card" + (on ? " on" : ""),
              "aria-pressed": on ? "true" : "false",
              onclick: () => applyMode(mk),
            },
            h("div", { class: "t" }, m.title, m.badge && h("span", { class: "badge ok" }, m.badge)),
            h("div", { class: "s", text: m.desc }),
            h("div", { class: "fps-tag", text: m.fps }),
          );
        }),
      ),
    );

    let startBtn;
    if (running) {
      startBtn = h("button", { class: "start stop", onclick: stopRun }, icon("stop"), run.kind === "play" ? H.stop : H.stopPrebuild);
    } else {
      startBtn = h("button", { class: "start", "data-sfx": "start", disabled: !v.path, onclick: () => startFlow(false) }, icon("play"), H.start);
    }

    const status = h(
      "div",
      { class: "status", id: "status" },
      h("span", { class: "dot" }),
      h("span", { id: "live-state" }),
      h("span", { class: "num", id: "live-fps" }),
      h("span", { id: "live-time" }),
    );

    const hotkeyPill = h(
      "div",
      { class: "hotkey-pill" },
      icon("controls"),
      h("span", null, "In-Game Menu: "),
      h("kbd", null, st.hotkey.spec || "Win+Alt+V"),
      st.hotkey.error && h("span", { class: "badge warn", title: st.hotkey.error }, "Hotkey conflict"),
    );

    const engine = h(
      "div",
      { class: "engine-line" + (v.path ? "" : " missing") },
      h("span", null, H.engine + ":"),
      h("span", { class: "p selectable", title: v.path || "" }, v.path || H.engineNone),
      tip(H.engineTip),
      h("button", { class: "link-btn", disabled: running, onclick: pickEngine }, H.change),
    );

    const hasLog = st.logSize > 0 || (run && run.kind);
    const showLog = (on) => {
      p.show_log = on;
      changed(true, false);
      if (on) setTimeout(() => { logEl.scrollTop = logEl.scrollHeight; }, 0);
    };
    let log = null;
    if (hasLog && p.show_log) {
      log = h(
        "div",
        { class: "log-panel" },
        h("div", { class: "log-head" }, h("span", null, H.logTitle), h("span", null, h("button", { class: "link-btn", onclick: () => openPlace("log") }, H.openLog), h("button", { class: "link-btn", onclick: () => showLog(false) }, H.hideLog))),
        logEl,
      );
    } else if (hasLog) {
      log = h("div", { class: "log-toggle" }, h("button", { class: "link-btn", onclick: () => showLog(true) }, H.showLog));
    }

    $("#home").replaceChildren(game, modeField, startBtn, status, hotkeyPill, engine, log || "");
    updateLive();
  }

  async function pickGame() {
    const f = await invoke("pick_exe", { label: S.common.exeFilter });
    if (f) { st.profile.game_exe = f; changed(true, true); }
  }

  async function pickEngine() {
    const f = await invoke("pick_exe", { label: S.common.exeFilter });
    if (!f) return;
    st.profile.victory_exe = f;
    await saveNow(true).catch((e) => toast(errText(e), true));
    render();
  }

  // ------------------------------------------------------------- settings

  function openSettings(page) {
    if (page) st.page = page;
    st.settingsOpen = true;
    $("#settings").classList.remove("hidden");
    if (st.page === "controls") loadNativeControls();
    if (st.page === "cache") refreshCache();
    if (st.page === "advanced") refreshLossless();
    render();
  }

  function closeSettings() {
    if (st.ingameOverlay && isGameRunning()) {
      invoke("resume_game");
      return;
    }
    st.settingsOpen = false;
    $("#settings").classList.add("hidden");
    hideTip();
    render();
  }

  function go(page) {
    st.page = page;
    $("#set-main").scrollTop = 0;
    if (page === "controls") loadNativeControls();
    if (page === "cache") refreshCache();
    if (page === "advanced") refreshLossless();
    render();
  }

  // ------------------------------------------------------------- Graphics

  function pageGraphics() {
    const p = st.profile;
    const G = S.graphics;
    const rb = restartBadge();

    const modeOpts = ["performance", "balanced", "high-quality", "native-4k"].map((k) => ({
      value: k,
      title: S.home.modes[k].title,
      text: S.home.modes[k].desc + " · " + S.home.modes[k].fps,
      badge: S.home.modes[k].badge,
      badgeKind: "ok",
    }));

    const resOpts = [
      { value: "1280x720", title: "1280×720", text: "720p (Max FPS)" },
      { value: "1664x936", title: "1664×936", text: "936p" },
      { value: "1920x1080", title: "1920×1080", text: "1080p (Performance)" },
      { value: "2560x1440", title: "2560×1440", text: "1440p (Monitor Native)", badge: "Native", badgeKind: "ok" },
      { value: "3840x2160", title: "3840×2160", text: "4K (Series X max)" },
    ];

    const dlssOpts = ["off", "quality", "balanced", "performance", "dlaa"].map((k) => ({
      value: k,
      title: G.dlssOpts[k][0],
      text: G.dlssOpts[k][1],
      disabled: k !== "off" && !st.dlssAvailable,
    }));

    const biasVal = p.lod_bias != null ? p.lod_bias : (p.dlss === "quality" ? -0.58 : p.dlss === "balanced" ? -0.79 : p.dlss === "performance" ? -1.0 : 0.0);

    return h(
      "div",
      { class: "page" },
      head(S.nav.graphics, G.lead, rb),
      st.restartNeeded && notice("warn", S.ingame.pendingBanner),
      card(G.modeTitle, G.modeTip, tiles(modeOpts, p.preset, (v) => applyMode(v))),
      card(G.resTitle, G.resTip, tiles(resOpts, p.output_resolution, (v) => { p.output_resolution = v; changed(true, true); })),
      card(
        G.dlssTitle,
        G.dlssTip,
        !st.dlssAvailable && notice("warn", G.dlssMissing),
        tiles(dlssOpts, p.dlss, (v) => { p.dlss = v; p.lod_bias = null; changed(true, true); }),
        sliderRow({
          triple: G.lodBias,
          value: biasVal,
          min: -2.0,
          max: 0.5,
          step: 0.05,
          unit: "{v}",
          badges: rb,
          onInput: (v) => { p.lod_bias = v; },
          onCommit: (v) => { p.lod_bias = v; changed(true, true); },
        }),
      ),
      card(
        G.displayCard,
        null,
        boolSetting(G.fullscreen, p.fullscreen, (v) => { p.fullscreen = v; changed(true, true); }, rb),
        boolSetting(G.perfPanel, p.perf_panel, (v) => { p.perf_panel = v; changed(true, true); }, rb),
        settingRow(
          G.consoleTitle,
          rb,
          seg([
            ["Series X", p.console === "series-x", () => { p.console = "series-x"; changed(true, true); }],
            ["Series S", p.console === "series-s", () => { p.console = "series-s"; changed(true, true); }],
          ]),
        ),
        boolSetting(G.reflections, p.reflections, (v) => { p.reflections = v; changed(true, true); }, h("span", { class: "badge warn" }, S.common.experimental)),
      ),
    );
  }

  // ------------------------------------------------------------- Controls

  async function loadNativeControls() {
    try {
      st.nativeControls = await invoke("read_native_keys");
    } catch (e) {
      toast(errText(e), true);
    }
    if (st.settingsOpen && st.page === "controls") render();
  }

  async function saveNativeControlsLive() {
    if (!st.nativeControls) return;
    try {
      const path = await invoke("write_native_keys", { actions: st.nativeControls.actions });
      st.nativeControls.path = path;
      st.nativeControls.exists = true;
      toast(S.controls.savedLive);
      sound.sfx("success");
    } catch (e) {
      toast(errText(e), true);
    }
  }

  function findControlsConflicts(actions) {
    const conflicts = [];
    const byCatKey = new Map();
    for (const a of actions) {
      for (const k of a.keys) {
        const ck = `${a.category}::${k}`;
        if (!byCatKey.has(ck)) byCatKey.set(ck, []);
        byCatKey.get(ck).push(a);
      }
    }
    const marked = new Set();
    for (const [ck, list] of byCatKey.entries()) {
      if (list.length > 1) {
        // Ignore expected multi-vehicle/car aliases sharing the same key
        const distinct = list.filter((x, i, arr) => arr.findIndex((y) => y.label === x.label) === i);
        if (distinct.length > 1) {
          marked.add(ck);
          if (conflicts.length < 4) {
            conflicts.push(fmt(S.controls.conflict, {
              cat: distinct[0].category,
              key: ck.split("::")[1],
              a: distinct[0].label,
              b: distinct[1].label,
            }));
          }
        }
      }
    }
    return { conflicts, marked };
  }

  function pageControls() {
    const C = S.controls;
    const p = st.profile;
    const nc = st.nativeControls;
    const sens = nc ? nc.mouse_sensitivity : (p.mouse_sensitivity || 1.0);
    const invY = nc ? nc.mouse_invert_y : !!p.mouse_invert_y;

    const saveMouseLive = async (newSens, newInv) => {
      if (nc) { nc.mouse_sensitivity = newSens; nc.mouse_invert_y = newInv; }
      p.mouse_sensitivity = newSens;
      p.mouse_invert_y = newInv;
      try {
        await invoke("write_mouse_live", { sensitivity: newSens, invertY: newInv });
        toast("Mouse updated live (reloads in game within 1 s).");
      } catch (e) {
        toast(errText(e), true);
      }
    };

    const hotkeyChoices = ["Win+Alt+V", "Win+Shift+V", "Win+F1", "Win+Backquote"];
    const hotkeySel = h(
      "select",
      {
        "aria-label": C.hotkey[0],
        onchange: async (e) => {
          try {
            st.hotkey = await invoke("set_ingame_hotkey", { spec: e.target.value });
            p.ingame_hotkey = st.hotkey.spec;
            toast(`In-Game Menu hotkey set to ${st.hotkey.spec}`);
            updateIngameBar();
            render();
          } catch (err) {
            toast(errText(err), true);
          }
        },
      },
      hotkeyChoices.map((k) => h("option", { value: k }, k)),
    );
    hotkeySel.value = hotkeyChoices.includes(st.hotkey.spec) ? st.hotkey.spec : "Win+Alt+V";

    const mouseCard = card(
      C.mouseCard,
      null,
      sliderRow({
        triple: C.sens,
        value: sens,
        min: 0.1,
        max: 4.0,
        step: 0.05,
        unit: "{v}x",
        badges: liveBadge(),
        onInput: (v) => { if (nc) nc.mouse_sensitivity = v; },
        onCommit: (v) => saveMouseLive(v, invY),
      }),
      boolSetting(C.invertY, invY, (v) => { saveMouseLive(sens, v); render(); }, liveBadge()),
      settingRow(C.hotkey, liveBadge(), hotkeySel),
      st.hotkey.error && notice("warn", st.hotkey.error),
      boolSetting(C.nativeMode, p.native_controls, (v) => { p.native_controls = v; changed(true, true); }, restartBadge()),
    );

    if (!nc) {
      return h("div", { class: "page" }, head(S.nav.controls, C.lead, liveBadge()), mouseCard, card(C.bindingsCard, null, h("p", { class: "muted", text: "Loading native-keys.txt…" })));
    }

    const categories = ["All", "On Foot", "Combat & Weapons", "Vehicle", "Aircraft", "Boat / Sub", "Bicycle", "Parachute", "Phone", "Menus & Map", "Other"];
    const q = st.controlsSearch.trim().toLowerCase();
    const filtered = nc.actions.filter((a) => {
      if (st.controlsCategory !== "All" && a.category !== st.controlsCategory) return false;
      if (st.controlsBoundOnly && a.keys.length === 0) return false;
      if (q) {
        const hay = `${a.label} ${a.id} ${a.category} ${a.keys.join(" ")}`.toLowerCase();
        if (!hay.includes(q)) return false;
      }
      return true;
    });

    const { conflicts, marked } = findControlsConflicts(nc.actions);

    const searchInput = h("input", {
      class: "search-input",
      type: "search",
      placeholder: C.searchPlaceholder,
      value: st.controlsSearch,
      oninput: (e) => {
        st.controlsSearch = e.target.value;
        // Re-render list smoothly without losing focus
        const pos = e.target.selectionStart;
        render();
        const again = document.querySelector(".search-input");
        if (again) { again.focus(); again.setSelectionRange(pos, pos); }
      },
    });

    const bindRows = filtered.map((a) => {
      const chips = a.keys.length
        ? a.keys.map((k, idx) =>
            h(
              "span",
              { class: "key" + (marked.has(`${a.category}::${k}`) ? " clash" : "") },
              k,
              h("button", {
                class: "x",
                title: `Remove ${k}`,
                onclick: () => {
                  a.keys.splice(idx, 1);
                  saveNativeControlsLive();
                  render();
                },
              }, "×"),
            ),
          )
        : [h("span", { class: "key none" }, C.none)];

      return h(
        "div",
        { class: "bind" },
        h(
          "div",
          null,
          h("div", { class: "bind-name" }, a.label, " ", h("span", { class: "badge later" }, a.category)),
          h("div", { class: "bind-meta" }, a.id + (a.is_menu ? " · menu context" : "")),
        ),
        h(
          "div",
          { class: "bind-keys" },
          chips,
          h("button", { class: "link-btn", onclick: () => startCapture(a, "replace") }, C.change),
          h("button", { class: "icon-btn", title: C.add, onclick: () => startCapture(a, "add") }, icon("plus", "2.4")),
        ),
      );
    });

    const bindingsCard = card(
      C.bindingsCard,
      null,
      h(
        "div",
        { class: "controls-toolbar" },
        searchInput,
        h("label", { class: "row small muted", style: "cursor:pointer" },
          toggle(st.controlsBoundOnly, (v) => { st.controlsBoundOnly = v; render(); }, C.boundOnly),
          C.boundOnly,
        ),
        h("button", {
          class: "btn small",
          onclick: async () => {
            try {
              st.nativeControls = await invoke("reset_native_keys");
              toast(C.resetDone);
              sound.sfx("success");
              render();
            } catch (e) { toast(errText(e), true); }
          },
        }, C.reset),
      ),
      h(
        "div",
        { class: "cat-pills" },
        categories.map((cat) =>
          h("button", {
            class: "cat-pill" + (st.controlsCategory === cat ? " on" : ""),
            onclick: () => { st.controlsCategory = cat; render(); },
          }, cat),
        ),
      ),
      conflicts.length > 0 && conflicts.map((c) => notice("warn", c)),
      h("div", { class: "controls-list binds" }, bindRows.length ? bindRows : h("p", { class: "muted", text: "No actions match your filter. Turn off 'Bound only' to see all 460 actions." })),
      h(
        "div",
        { class: "row spacer-top" },
        h("span", { class: "faint selectable", text: fmt(C.fileAt, { path: nc.path }) + ` (${filtered.length} shown)` }),
        h("div", { class: "spacer" }),
        h("button", { class: "btn ghost small", onclick: () => openPlace("keys-folder") }, icon("folder"), C.openFolder),
      ),
    );

    return h("div", { class: "page" }, head(S.nav.controls, C.lead, liveBadge()), mouseCard, bindingsCard);
  }

  // Key / Mouse / Scroll Wheel Capture for Native Controls
  const CODE_NAMES = {
    ShiftLeft: "Shift", ShiftRight: "RightShift", ControlLeft: "Ctrl", ControlRight: "RightCtrl",
    AltLeft: "Alt", AltRight: "RightAlt", Space: "Space", Enter: "Enter", NumpadEnter: "Enter",
    Escape: "Esc", Tab: "Tab", CapsLock: "CapsLock", Backspace: "Backspace",
    Insert: "Insert", Delete: "Delete", Home: "Home", End: "End", PageUp: "PageUp", PageDown: "PageDown",
    ArrowUp: "ArrowUp", ArrowDown: "ArrowDown", ArrowLeft: "ArrowLeft", ArrowRight: "ArrowRight",
    Semicolon: "Semicolon", Quote: "Quote", Comma: "Comma", Minus: "Minus", Equal: "Equals",
    Period: "Period", Slash: "Slash", Backslash: "Backslash", Backquote: "Backquote",
    BracketLeft: "LeftBracket", BracketRight: "RightBracket",
  };
  for (let i = 1; i <= 12; i++) CODE_NAMES["F" + i] = "F" + i;
  for (let i = 0; i <= 9; i++) CODE_NAMES["Numpad" + i] = "Numpad" + i;
  const MOUSE_NAMES = ["LeftClick", "MiddleClick", "RightClick", "Mouse4", "Mouse5"];

  function nameOfKey(e) {
    if (e.key && e.key.length === 1 && /[a-z]/i.test(e.key)) return e.key.toUpperCase();
    const digit = /^Digit(\d)$/.exec(e.code);
    if (digit) return digit[1];
    return CODE_NAMES[e.code] || null;
  }

  function startCapture(action, mode) {
    st.capture = { action, mode };
    const msg = h("div", { class: "modal-msg" });
    showModal(
      h(
        "div",
        { class: "modal-box", role: "dialog", "aria-modal": "true" },
        h("div", { class: "pulse" }, icon("controls")),
        h("h3", null, S.controls.captureTitle),
        h("p", { class: "muted" }, fmt(S.controls.captureFor, { action: action.label })),
        h("p", { class: "faint" }, S.controls.captureNote),
        msg,
        h("button", { class: "btn cancel", onclick: endCapture }, S.controls.cancel),
      ),
    );
    st.capture.msg = msg;
  }

  function endCapture() {
    st.capture = null;
    hideModal();
    render();
  }

  function commitCaptured(keyName) {
    const c = st.capture;
    if (!c) return;
    if (c.mode === "replace") c.action.keys = [keyName];
    else if (!c.action.keys.includes(keyName)) c.action.keys.push(keyName);
    endCapture();
    saveNativeControlsLive();
  }

  window.addEventListener("keydown", (e) => {
    if (st.capture) {
      e.preventDefault(); e.stopPropagation();
      if (e.repeat) return;
      const name = nameOfKey(e);
      if (name) commitCaptured(name);
      else st.capture.msg.textContent = "Unsupported key — try letters, digits, F1–F12, arrows, modifiers, mouse buttons, or scroll wheel.";
      return;
    }
    if (e.key === "Escape") {
      if (modalCancel) modalCancel();
      else if (st.settingsOpen) closeSettings();
    }
    if (e.key === "F5" || e.key === "F7" || ((e.ctrlKey || e.metaKey) && "rRpfgu".includes(e.key))) e.preventDefault();
  }, true);

  window.addEventListener("mousedown", (e) => {
    if (!st.capture) return;
    if (e.target.closest && e.target.closest(".cancel")) return;
    e.preventDefault();
    const name = MOUSE_NAMES[e.button];
    if (name) commitCaptured(name);
  }, true);

  window.addEventListener("wheel", (e) => {
    if (!st.capture) return;
    e.preventDefault();
    if (e.deltaY < 0) commitCaptured("WheelUp");
    else if (e.deltaY > 0) commitCaptured("WheelDown");
  }, { passive: false, capture: true });

  window.addEventListener("contextmenu", (e) => {
    if (!(e.target instanceof HTMLInputElement) && !(e.target.closest && e.target.closest(".selectable, pre, .log"))) e.preventDefault();
  });

  // ------------------------------------------------------------- Engine & Textures

  function pageEngine() {
    const p = st.profile;
    const E = S.engine;
    const rb = restartBadge();

    return h(
      "div",
      { class: "page" },
      head(S.nav.engine, E.lead, rb),
      st.restartNeeded && notice("warn", S.ingame.pendingBanner),
      card(
        E.texCard,
        null,
        boolSetting(E.honestMem, p.honest_title_memory, (v) => { p.honest_title_memory = v; changed(true, true); }, [h("span", { class: "badge ok" }, "Fixes blurry textures"), rb]),
        sliderRow({
          triple: E.partialFrames,
          value: p.texture_partial_frames,
          min: 1, max: 30, step: 1, unit: S.common.frames,
          badges: rb,
          onInput: (v) => { p.texture_partial_frames = v; },
          onCommit: (v) => { p.texture_partial_frames = v; changed(true, true); },
        }),
        sliderRow({
          triple: E.poolSpares,
          value: p.texture_pool_spares,
          min: 2, max: 32, step: 2, unit: "{v} blocks",
          badges: rb,
          onInput: (v) => { p.texture_pool_spares = v; },
          onCommit: (v) => { p.texture_pool_spares = v; changed(true, true); },
        }),
        sliderRow({
          triple: E.roomPercent,
          value: p.texture_room_percent,
          min: 70, max: 97, step: 1, unit: S.common.percent,
          badges: rb,
          onInput: (v) => { p.texture_room_percent = v; },
          onCommit: (v) => { p.texture_room_percent = v; changed(true, true); },
        }),
      ),
      card(
        E.perfCard,
        null,
        settingRow(
          E.submitFrames,
          [h("span", { class: "badge ok" }, "-10 ms stall"), rb],
          seg([1, 2, 3].map((n) => [String(n), p.submit_frames === n, () => { p.submit_frames = n; changed(true, true); }])),
        ),
        boolSetting(E.wideCopy, p.wide_copy, (v) => { p.wide_copy = v; changed(true, true); }, rb),
        boolSetting(E.portJournal, p.port_journal, (v) => { p.port_journal = v; changed(true, true); }, rb),
      ),
      card(
        E.vramCard,
        null,
        sliderRow({
          triple: E.cardBuffers,
          value: p.card_buffers_mb,
          min: 256, max: 3072, step: 128, unit: S.common.mb,
          badges: rb,
          onInput: (v) => { p.card_buffers_mb = v; },
          onCommit: (v) => { p.card_buffers_mb = v; changed(true, true); },
        }),
        sliderRow({
          triple: E.cardWindows,
          value: p.card_windows_mb,
          min: 256, max: 2048, step: 128, unit: S.common.mb,
          badges: rb,
          onInput: (v) => { p.card_windows_mb = v; },
          onCommit: (v) => { p.card_windows_mb = v; changed(true, true); },
        }),
      ),
    );
  }

  // ------------------------------------------------------------- Shaders & Auto Tune

  async function refreshCache() {
    try { st.cacheInfo = await invoke("cache_check"); } catch { st.cacheInfo = null; }
    if (st.settingsOpen && st.page === "cache") render();
  }

  function pageCache() {
    const p = st.profile;
    const K = S.cache;
    const v = st.victory || { path: null };
    const running = !!(st.run && st.run.running);
    const ci = st.cacheInfo;
    const r = st.tune;
    const sys = st.system;

    const reasonText = (x) => fmt(K.reasons[x.code] || x.code, x.args);
    const list = (items, cls, ic) => h("ul", { class: "reasons" }, items.map((x) => h("li", { class: cls }, icon(ic), h("span", null, reasonText(x)))));

    return h(
      "div",
      { class: "page" },
      head(S.nav.cache, K.lead),
      card(
        K.cacheCard,
        null,
        ci && notice(ci.cold ? "warn" : "ok", fmt(K.cacheInfo, { dir: ci.dir, mb: ci.pipelines_mb, warmed: ci.warmed_pipelines, known: ci.known_pipelines })),
        h(
          "div",
          { class: "row" },
          h("button", { class: "btn primary", disabled: running || !v.path, onclick: () => startRun("prebuild") }, icon("bolt"), K.warmBtn),
          tip(K.warmTip),
          h("button", { class: "btn", disabled: running || !v.path, onclick: () => startRun("shaders") }, icon("spark"), K.shadersBtn),
          tip(K.shadersTip),
        ),
        boolSetting(K.warmAll, p.prebuild_all, (x) => { p.prebuild_all = x; changed(true, false); }),
      ),
      card(
        K.tuneCard,
        null,
        sys && sys.gpu && h("p", { class: "muted small", text: `${sys.gpu.name} (${gb(sys.gpu.vram_mb)} VRAM) · ${sys.cpu_name} (${sys.cpu_threads} threads) · ${gbWhole(sys.ram_mb)} RAM · ${sys.display_width}×${sys.display_height}` }),
        h(
          "div",
          { class: "row spacer-top" },
          h("button", {
            class: "btn primary",
            onclick: async () => {
              try {
                st.system = await invoke("get_system");
                st.tune = await invoke("auto_tune");
                render();
              } catch (e) { toast(errText(e), true); }
            },
          }, icon("spark"), K.tuneBtn),
        ),
        r && h(
          "div",
          { class: "spacer-top" },
          list(r.picks, "pick", "check"),
          r.warnings.length > 0 && list(r.warnings, "warn", "warn"),
          r.notes.length > 0 && list(r.notes, "note", "info"),
          h("div", { class: "row end spacer-top" }, h("button", {
            class: "btn primary",
            onclick: async () => {
              st.profile = r.profile;
              await saveNow(true);
              toast(K.tuneApplied);
              sound.sfx("success");
              render();
            },
          }, K.tuneApply)),
        ),
      ),
    );
  }

  // ------------------------------------------------------------- Advanced

  async function refreshLossless() {
    try { st.lossless = await invoke("lossless_status"); } catch { st.lossless = null; }
    if (st.settingsOpen && st.page === "advanced") render();
  }

  function pageAdvanced() {
    const A = S.advanced;
    const p = st.profile;
    const v = st.victory || { path: null, how: "none" };
    const paths = st.paths || st.start.paths;
    const ls = st.lossless;
    const cmdBox = h("pre", { class: "box selectable" }, "...");
    invoke("command_preview").then((c) => { cmdBox.textContent = c; }).catch(() => {});

    const folderRow = (triple, key, folder) =>
      settingRow(
        triple,
        restartBadge(),
        [
          h("div", { class: "path selectable", title: folder.path }, folder.path),
          h("div", { class: "row" },
            h("div", { class: "spacer" }),
            p[key] && h("button", { class: "btn ghost small", onclick: () => { p[key] = ""; changed(true, true); } }, S.common.useDefault),
            h("button", { class: "btn small", onclick: async () => { const f = await invoke("pick_folder"); if (f) { p[key] = f; changed(true, true); } } }, icon("folder"), S.common.browse),
          ),
        ],
      );

    return h(
      "div",
      { class: "page" },
      head(S.nav.advanced, A.lead),
      card(
        A.victoryTitle,
        null,
        h("div", { class: "row" },
          h("div", { class: "path selectable" + (v.path ? "" : " empty") }, v.path || S.home.engineNone),
          h("button", { class: "btn small", onclick: pickEngine }, icon("folder"), S.common.browse),
          p.victory_exe && h("button", { class: "btn ghost small", onclick: async () => { p.victory_exe = ""; await saveNow(true); render(); } }, A.victoryAuto),
        ),
      ),
      card(A.foldersTitle, null, folderRow(A.cache, "cache_dir", paths.cache), folderRow(A.data, "data_dir", paths.data)),
      card(
        A.losslessTitle,
        null,
        ls && (ls.installed ? h("span", { class: "badge ok" }, "Lossless Scaling Installed") : h("span", { class: "badge later" }, "Not installed")),
        boolSetting(A.losslessStart, p.lossless.start_with_game && !!(ls && ls.installed), (x) => { p.lossless.start_with_game = x; changed(true, false); }),
        h("div", { class: "row spacer-top" },
          h("button", {
            class: "btn small",
            disabled: !(ls && ls.settings_exists) || (ls && ls.running),
            onclick: async () => {
              try {
                await saveNow(false);
                await invoke("lossless_write");
                toast("Lossless Scaling profile updated.");
                refreshLossless();
              } catch (e) { toast(errText(e), true); }
            },
          }, A.losslessWrite),
        ),
      ),
      card(
        A.doctorTitle,
        null,
        h("div", { class: "row" },
          h("button", {
            class: "btn",
            disabled: st.doctorBusy || !v.path,
            onclick: async () => {
              st.doctorBusy = true; render();
              try { st.doctor = await invoke("doctor"); } catch (e) { st.doctor = { ok: false, text: errText(e) }; }
              st.doctorBusy = false; render();
            },
          }, st.doctorBusy ? "Checking…" : A.doctorBtn),
          h("button", { class: "btn ghost small", onclick: () => openPlace("log") }, A.openLog),
          h("button", { class: "btn ghost small", onclick: () => openPlace("keys-folder") }, A.openData),
          h("button", { class: "btn ghost small", onclick: () => openPlace("launcher-folder") }, A.openLauncher),
        ),
        st.doctor && h("pre", { class: "box selectable spacer-top" }, st.doctor.text),
      ),
      card(A.commandTitle, null, cmdBox),
    );
  }

  // ------------------------------------------------------- modal & best

  let modalCancel = null;
  function showModal(box) {
    const modal = $("#modal");
    modal.replaceChildren(box);
    modal.classList.remove("hidden");
  }
  function hideModal() {
    $("#modal").classList.add("hidden");
    $("#modal").replaceChildren();
  }

  function choose({ iconName, title, lines, choices, countdown }) {
    let timer = null;
    const close = () => { clearInterval(timer); modalCancel = null; hideModal(); };
    const buttons = choices.map(([label, primary, act]) => h("button", { class: "btn " + (primary ? "primary" : "ghost"), onclick: () => { close(); act(); } }, label));
    const countEl = countdown ? h("p", { class: "faint" }) : null;
    showModal(
      h(
        "div",
        { class: "modal-box", role: "dialog", "aria-modal": "true" },
        h("div", { class: "pulse" }, icon(iconName)),
        h("h3", null, title),
        lines.filter(Boolean).map((l, i) => h("p", { class: i ? "faint" : "muted" }, l)),
        h("div", { class: "actions" }, buttons),
        countEl,
      ),
    );
    modalCancel = close;
    const def = buttons.find((b) => b.classList.contains("primary")) || buttons[0];
    setTimeout(() => def.focus(), 30);
    if (countdown) {
      let left = countdown;
      const tick = () => { countEl.textContent = left + " s"; if (left-- <= 0) def.click(); };
      tick();
      timer = setInterval(tick, 1000);
    }
  }

  async function optimise() {
    try {
      st.profile = await invoke("best_apply");
      await saveNow(true);
      toast(S.best.done);
      sound.sfx("success");
      render();
    } catch (e) { toast(errText(e), true); }
  }

  async function offerBest() {
    let offer;
    try { offer = await invoke("best_offer"); } catch { return; }
    if (offer.problem) toast(fmt(S.best.problem, { e: offer.problem }), true);
    if (!offer.due) return;
    const e = offer.pick.entry;
    choose({
      iconName: "spark",
      title: S.best.title,
      lines: [fmt(S.best.line, { pc: e.pc, fps: e.measured.fps_median, note: e.measured.note }), offer.pick.exact ? S.best.exact : S.best.guess],
      choices: [
        [S.best.use, true, optimise],
        [S.best.keep, false, async () => { try { st.profile = await invoke("best_decline"); } catch {} }],
      ],
    });
  }

  // ------------------------------------------------------------------ runs

  async function startFlow(fromCommandLine) {
    if (st.run && st.run.running) return;
    let check = null;
    try { await saveNow(false); check = await invoke("cache_check"); } catch {}
    if (check && check.cold && check.known_pipelines > 0) {
      const C = S.cold;
      const line = check.cold === "missing" ? C.missing : check.cold === "small" ? fmt(C.small, { mb: check.pipelines_mb, warmed: check.warmed_pipelines }) : C.old;
      choose({
        iconName: "bolt",
        title: C.title,
        lines: [line, C.then],
        countdown: fromCommandLine ? 10 : 0,
        choices: [
          [C.prebuild, true, () => { st.startAfterPrebuild = true; startRun("prebuild"); }],
          [C.anyway, false, () => startRun("play")],
        ],
      });
      return;
    }
    // A graphics mode not warmed yet (switched since the last warm): warm it, then play.
    let needWarm = false;
    try { needWarm = await invoke("warm_needed"); } catch {}
    if (needWarm) {
      const mode = (S.home.modes[st.profile.preset] || {}).title || st.profile.preset;
      toast(fmt(S.cold.modeChanged, { mode }));
      st.startAfterPrebuild = true;
      startRun("prebuild");
      return;
    }
    startRun("play");
  }

  async function startRun(kind) {
    try {
      await saveNow(false);
      const cmd = kind === "play" ? "start_play" : kind === "shaders" ? "start_translate_shaders" : "start_prebuild";
      const r = await invoke(cmd);
      if (kind === "play") {
        st.launchedProfileSnapshot = JSON.stringify(st.profile);
        st.restartNeeded = false;
      }
      st.logSize = -1;
      logEl.replaceChildren();
      if (r.lossless_error) toast(fmt(S.common.losslessFailed, { e: r.lossless_error }), true);
      await poll();
      render();
    } catch (e) {
      st.startAfterPrebuild = false;
      toast(errText(e), true);
    }
  }

  async function stopRun() {
    st.startAfterPrebuild = false;
    try { await invoke("stop_run"); } catch (e) { toast(errText(e), true); }
    setTimeout(poll, 300);
  }

  async function openPlace(what) {
    try { await invoke("open_place", { what }); } catch (e) { toast(errText(e), true); }
  }

  function showLog(lines) {
    const atBottom = logEl.scrollHeight - logEl.scrollTop - logEl.clientHeight < 30;
    const frag = document.createDocumentFragment();
    for (const line of lines) {
      const cls = /\bERROR\b/.test(line) ? "e" : /\bWARN\b/.test(line) ? "w" : null;
      frag.append(h("div", { class: cls }, line));
    }
    logEl.replaceChildren(frag);
    if (atBottom || logEl.scrollTop === 0) logEl.scrollTop = logEl.scrollHeight;
  }

  function updateLive() {
    const r = st.run;
    const set = (id, v) => { const el = document.getElementById(id); if (el) el.textContent = v; };
    const statusEl = document.getElementById("status");
    if (!statusEl) return;
    statusEl.className = "status";
    const T = S.state;
    if (!r || (!r.running && r.kind == null)) {
      set("live-state", T.idle); set("live-fps", ""); set("live-time", "");
      return;
    }
    let state;
    if (r.running) {
      statusEl.classList.add(r.kind === "play" && r.fps != null ? "live" : "busy");
      state = r.kind === "prebuild" ? T.prebuilding : r.kind === "translate-shaders" ? T.translating : r.window_seen ? T.playing : T.loading;
    } else if (r.exit_code === 0) state = T.endedOk;
    else if (r.exit_code != null) state = fmt(T.ended, { code: r.exit_code });
    else state = T.endedNoCode;
    set("live-state", state);
    set("live-fps", r.running && r.fps != null ? fmt(T.fps, { fps: Math.round(r.fps) }) + (r.ms != null ? " · " + fmt(T.ms, { ms: r.ms.toFixed(0) }) : "") : "");
    set("live-time", time(r.seconds));
    updateIngameBar();
  }

  function setGameOn(on) {
    const was = document.body.classList.contains("game-on");
    if (on === was) return;
    document.body.classList.toggle("game-on", on);
    if (on) sound.pause(600);
    else {
      st.ingameOverlay = false;
      st.restartNeeded = false;
      updateIngameBar();
      sound.play();
    }
  }

  async function poll() {
    try {
      const s = await invoke("run_status", { since: Math.max(0, st.logSize) });
      const prev = st.run;
      const wasRunning = !!(prev && prev.running);
      st.run = s;
      if (s.lines) {
        const hadLog = st.logSize > 0;
        st.logSize = s.log_size;
        showLog(s.lines);
        if (!hadLog && s.log_size > 0) renderHome();
      }
      setGameOn(s.running && s.kind === "play");
      if (wasRunning !== s.running) {
        if (wasRunning && !s.running && prev.kind === "prebuild") {
          if (s.exit_code === 0) {
            sound.sfx("success");
            if (st.startAfterPrebuild) { st.startAfterPrebuild = false; startRun("play"); }
          } else if (st.startAfterPrebuild) {
            st.startAfterPrebuild = false;
            toast(fmt(S.cold.failed, { code: s.exit_code == null ? "?" : s.exit_code }), true);
          }
        }
        render();
      } else updateLive();
    } catch {}
    setTimeout(poll, isGameRunning() ? 1200 : 700);
  }

  async function takeLaunch() {
    let l = null;
    try { l = await invoke("take_launch"); } catch { return false; }
    if (!l) return false;
    if (l.error) { toast(fmt(S.launch.error, { e: l.error }), true); return true; }
    if (l.applied) {
      const s = await invoke("get_start");
      st.start = s; st.profile = s.profile; st.victory = s.victory; st.paths = s.paths; st.dlssAvailable = s.dlss_available;
      toast(fmt(S.launch.applied, { list: l.notes.join(", ") }));
    }
    if (l.no_music) { st.sessionMute = true; sound.setLevel(); renderAudio(); }
    render();
    if (l.prebuild) { st.startAfterPrebuild = !!l.start; startRun("prebuild"); }
    else if (l.start) startFlow(true);
    return l.applied || l.start || l.prebuild;
  }

  // ------------------------------------------------------------- shell

  const PAGES = {
    graphics: pageGraphics,
    controls: pageControls,
    engine: pageEngine,
    cache: pageCache,
    advanced: pageAdvanced,
  };

  function render() {
    if (!st.profile) return;
    updateIngameBar();
    renderHome();
    if (!st.settingsOpen) return;
    const main = $("#set-main");
    const top = main.scrollTop;
    main.replaceChildren(PAGES[st.page]());
    main.scrollTop = top;
    for (const b of document.querySelectorAll(".nav-item")) {
      const on = b.dataset.page === st.page;
      b.classList.toggle("active", on);
      if (on) b.setAttribute("aria-current", "page");
      else b.removeAttribute("aria-current");
    }
  }

  async function init() {
    $("#brand-name").textContent = S.app.name;
    $("#brand-tag").textContent = S.app.tagline;
    const ctrlBtn = $("#open-controls");
    ctrlBtn.append(icon("controls"), S.home.controls);
    ctrlBtn.addEventListener("click", () => openSettings("controls"));
    const openBtn = $("#open-settings");
    openBtn.append(icon("gear"), S.home.settings);
    openBtn.addEventListener("click", () => openSettings("graphics"));
    const closeBtn = $("#close-settings");
    closeBtn.append(icon("back"), S.settings.back);
    closeBtn.addEventListener("click", closeSettings);
    $("#resume-game-btn").addEventListener("click", () => invoke("resume_game"));
    $("#ingame-restart-btn").addEventListener("click", () => promptRestartIfRunning());
    $("#settings-title").textContent = S.settings.title;

    const nav = $("#nav");
    for (const page of Object.keys(PAGES)) {
      const badge = page === "controls" ? h("span", { class: "badge live-pill nav-badge" }, "Live") : null;
      nav.append(h("button", { class: "nav-item", "data-page": page, "data-sfx": "page", onclick: () => go(page) }, icon(page), S.nav[page], badge));
    }
    $("#set-foot").append(
      h("button", { class: "btn primary", onclick: optimise }, icon("spark"), S.best.optimise),
      tip(S.best.optimiseTip),
    );
    $("#mute").addEventListener("click", () => {
      if (st.sessionMute) st.sessionMute = false;
      else st.profile.muted = !st.profile.muted;
      sound.setLevel(); renderAudio(); changed(false, false);
    });
    $("#volume").addEventListener("input", (e) => {
      st.profile.music_volume = Number(e.target.value);
      e.target.style.setProperty("--fill", e.target.value + "%");
      sound.setLevel(); changed(false, false);
    });

    try {
      st.start = await invoke("get_start");
      st.profile = st.start.profile;
      st.victory = st.start.victory;
      st.dlssAvailable = st.start.dlss_available;
      st.paths = st.start.paths;
      st.hotkey = st.start.hotkey || st.hotkey;
    } catch (e) {
      $("#home").replaceChildren(notice("bad", errText(e)));
      return;
    }

    sound.init();
    renderAudio();
    render();
    sound.play();

    invoke("get_system").then((s) => { st.system = s; if (st.settingsOpen) render(); }).catch(() => {});
    poll();
    const fromLine = await takeLaunch();
    if (!fromLine) offerBest();

    window.__TAURI__.event.listen("launch", () => takeLaunch());
    window.__TAURI__.event.listen("ingame-overlay", (ev) => {
      const open = !!(ev.payload && ev.payload.open);
      st.ingameOverlay = open;
      if (open) {
        // Open straight into live Controls when pulled up from inside the game!
        openSettings("controls");
      }
      updateIngameBar();
      render();
    });
  }

  init();
})();
