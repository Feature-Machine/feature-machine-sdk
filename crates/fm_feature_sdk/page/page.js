// Feature Machine page SDK, version 1 (feature surfaces plan, section 6). The app serves this file
// at <page base>_fm/1/page.js; a page links it relative to its own folder. It wraps the fm: 1
// messages between the page frame and its holder (the app), so old pages that use the messages
// keep working. It runs inside the sandbox: it is a helper, not a trust boundary.
// A classic script (module scripts do not load in a sandboxed frame on Windows): it sets window.fm.
(function () {
  "use strict";
  if (window.fm && window.fm.version) return;

  // A beat each second after ready(); the holder unloads a page that misses 3 (section 7).
  var BEAT_MS = 1000;
  var parent = window.parent;
  // No prototype: any topic name ("constructor", "__proto__") is a plain key (Sol r1).
  var topics = Object.create(null);
  var state = { sessions: [], settings: {}, revision: null, theme: null };
  var waits = Object.create(null);
  var next = 1;
  var beat = null;

  function post(m) {
    m.fm = 1;
    parent.postMessage(m, "*");
  }

  // Each listener runs on its own: an error in one does not stop the others.
  function fire(name, args) {
    (topics[name] || []).slice().forEach(function (fn) {
      try {
        fn.apply(null, args);
      } catch (e) {
        setTimeout(function () {
          throw e;
        });
      }
    });
  }

  function copy(v) {
    return v === undefined ? undefined : JSON.parse(JSON.stringify(v));
  }

  // The theme of the app as CSS variables on the page root (--fm-bg, --fm-text, ...); kit.css
  // uses them, and a page can too.
  function applyTheme(t) {
    var root = document.documentElement;
    root.style.colorScheme = t.scheme === "light" ? "light" : "dark";
    Object.keys(t.colors || {}).forEach(function (k) {
      if (/^[a-z0-9-]{1,32}$/.test(k) && typeof t.colors[k] === "string") root.style.setProperty("--fm-" + k, t.colors[k]);
    });
  }

  window.addEventListener("message", function (e) {
    var m = e.data;
    if (e.source !== parent || !m || m.fm !== 1) return;
    switch (m.kind) {
      case "sessions":
        state.sessions = Array.isArray(m.sessions) ? m.sessions : [];
        fire("fm:sessions", [copy(state.sessions)]);
        break;
      case "message":
        // "fm:" topics are the app's events: a feature message never fires them.
        if (typeof m.topic === "string" && m.topic.indexOf("fm:") !== 0) fire(m.topic, [m.data, { game: m.game, pid: m.pid }]);
        break;
      case "settings":
        state.settings = m.values && typeof m.values === "object" ? m.values : {};
        state.revision = typeof m.revision === "number" ? m.revision : null;
        fire("fm:settings", [copy(state.settings)]);
        break;
      case "setting_result": {
        var w = waits[m.req];
        if (!w) break;
        delete waits[m.req];
        if (m.error) w.reject(new Error(String(m.error)));
        else w.resolve();
        break;
      }
      case "theme":
        if (m.theme && typeof m.theme === "object") {
          state.theme = m.theme;
          applyTheme(m.theme);
          fire("fm:theme", [copy(m.theme)]);
        }
        break;
      case "send_failed":
        fire("fm:send_failed", [{ topic: m.topic, error: m.error }]);
        break;
    }
  });

  window.fm = {
    version: 1,
    // A message topic of the feature, or an app event: "fm:sessions", "fm:settings", "fm:theme",
    // "fm:send_failed" (feature topics never start with "fm:"). Gives a function that stops it.
    on: function (topic, fn) {
      (topics[topic] = topics[topic] || []).push(fn);
      return function () {
        topics[topic] = (topics[topic] || []).filter(function (f) {
          return f !== fn;
        });
      };
    },
    // A message to the feature in the game: to one session ({ game, pid }) or to each session.
    send: function (topic, data, session) {
      var m = { kind: "send", topic: String(topic), data: data === undefined ? null : data };
      if (session) {
        m.game = session.game;
        m.pid = session.pid;
      }
      post(m);
    },
    sessions: function () {
      return copy(state.sessions);
    },
    settings: function () {
      return copy(state.settings);
    },
    // Writes one setting of the feature through the app (the same check as Manage). The promise
    // ends when the app stored it; the new values come with "fm:settings".
    setSetting: function (id, value) {
      var req = next++;
      return new Promise(function (resolve, reject) {
        waits[req] = { resolve: resolve, reject: reject };
        post({ kind: "set_setting", req: req, id: String(id), value: value });
      });
    },
    get theme() {
      return copy(state.theme);
    },
    // The page is ready: the app then expects a beat each second and unloads a page that stops.
    ready: function () {
      post({ kind: "ready" });
      if (beat === null) {
        beat = setInterval(function () {
          post({ kind: "beat" });
        }, BEAT_MS);
      }
    },
  };
})();
