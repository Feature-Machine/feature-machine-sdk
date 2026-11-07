// Feature Machine page kit, version 2 (feature surfaces plan, section 6; feature refresh plan
// 2026-11-04): web components for feature pages, styled by kit.css. Load page.js first. Light DOM
// (no shadow root), so the page CSP needs no inline styles and a page can style them. Setting
// elements write through fm.setSetting, which the app checks as Manage does. README.md lists the
// elements; a part with `topic` reads the newest data of its session block (fm-sessions).
(function () {
  "use strict";
  if (!window.fm || !window.customElements || window.customElements.get("fm-stat")) return;
  var fm = window.fm;
  var SVG = "http://www.w3.org/2000/svg";

  function el(tag, cls, text) {
    var e = document.createElement(tag);
    if (cls) e.className = cls;
    if (text !== undefined) e.textContent = text;
    return e;
  }

  // The decimal places of a number attribute, also in exponent form ("1e-7" has 7).
  function decimals(attr) {
    var x = Number(attr);
    if (attr === null || attr === "" || !isFinite(x)) return 0;
    var parts = String(x).split("e");
    var frac = parts[0].split(".")[1] || "";
    return Math.max(0, frac.length - Number(parts[1] || 0));
  }

  function pad(n) {
    return (n < 10 ? "0" : "") + n;
  }

  // A value as text. `format`: number (default, with `digits`), duration (seconds as h:mm:ss or
  // m:ss), onoff, text. A number that rounds to zero shows no "-0".
  function format(v, fmt, digits) {
    if (v === undefined || v === null) return "-";
    if (fmt === "onoff") return v ? "On" : "Off";
    var num = typeof v === "number" && isFinite(v);
    if (fmt === "duration" && num) {
      var s = Math.max(0, Math.floor(v));
      var h = Math.floor(s / 3600);
      var m = Math.floor((s % 3600) / 60);
      return (h ? h + ":" + pad(m) : String(m)) + ":" + pad(s % 60);
    }
    if (num && fmt !== "text") {
      var p = Math.pow(10, digits);
      return (Math.round(v * p) / p || 0).toFixed(digits);
    }
    return typeof v === "object" ? "-" : String(v);
  }

  // RFC 6901 JSON Pointer into message data, as widget sources ("" is the whole data).
  function pointer(data, path) {
    if (!path) return data;
    if (path.charAt(0) !== "/") return undefined;
    return path
      .slice(1)
      .split("/")
      .reduce(function (v, part) {
        var k = part.replace(/~1/g, "/").replace(/~0/g, "~");
        return v !== null && typeof v === "object" && Object.prototype.hasOwnProperty.call(v, k) ? v[k] : undefined;
      }, data);
  }

  // The session ({ application, pid }) of the fm-sessions block that holds `node`, or null.
  function sessionOf(node) {
    var b = node.closest(".fm-session");
    return b && b.fmSession ? b.fmSession : null;
  }

  // Binds a part with a `topic` (and a `path`) to the newest data of its session block, or of
  // any session outside a block: part.take(value) now and at each message. Gives a stop.
  function bind(part) {
    var topic = part.getAttribute("topic");
    if (!topic) return null;
    var path = part.getAttribute("path") || "";
    var s = sessionOf(part);
    var now = s ? fm.last(topic, s) : undefined;
    if (now !== undefined) part.take(pointer(now, path));
    return fm.on(topic, function (data, from) {
      if (s && (from.application !== s.application || from.pid !== s.pid)) return;
      part.take(pointer(data, path));
    });
  }

  // A part that follows a source while it is in the document.
  class Bound extends HTMLElement {
    connectedCallback() {
      if (!this.built) {
        this.built = true;
        this.build();
      }
      this.stop = bind(this);
    }
    disconnectedCallback() {
      if (this.stop) this.stop();
      this.stop = null;
    }
  }

  // A label, a value and a unit. `value`: a number (shown with `digits`, times `scale`) or a
  // text; with `topic` and `path` it shows the source. <data value="active">On</data> children
  // give the words of a value (a state of the feature).
  class Stat extends Bound {
    build() {
      var words = (this.words = Object.create(null));
      Array.prototype.slice.call(this.querySelectorAll("data")).forEach(function (d) {
        words[d.getAttribute("value")] = d.textContent;
        d.remove();
      });
      this.labelEl = el("span", "fm-label", this.getAttribute("label") || "");
      var line = el("span");
      this.valueEl = el("span", "fm-value", "-");
      this.unitEl = el("span", "fm-unit", this.getAttribute("unit") || "");
      line.append(this.valueEl, this.unitEl);
      this.append(this.labelEl, line);
      this.show();
    }
    take(v) {
      this.value = v;
    }
    set value(v) {
      this.v = v;
      this.show();
    }
    get value() {
      return this.v;
    }
    show() {
      if (!this.valueEl) return;
      var v = this.v;
      if ((typeof v === "string" || typeof v === "number" || typeof v === "boolean") && String(v) in this.words) {
        this.valueEl.textContent = this.words[String(v)];
        return;
      }
      var scale = Number(this.getAttribute("scale") || 1);
      if (typeof v === "number" && isFinite(scale)) v = v * scale;
      this.valueEl.textContent = format(v, this.getAttribute("format"), Number(this.getAttribute("digits") || 0));
    }
  }

  // A line of the last `points` values (2 to 600, default 60) of a source, or of push(value),
  // over the full width. It is hidden until it has 2 values, so a new start leaves no gap.
  class Sparkline extends Bound {
    build() {
      this.vals = [];
      this.hidden = true;
      this.svg = document.createElementNS(SVG, "svg");
      this.svg.setAttribute("viewBox", "0 0 100 30");
      this.svg.setAttribute("preserveAspectRatio", "none");
      this.svg.setAttribute("aria-hidden", "true");
      this.line = document.createElementNS(SVG, "polyline");
      this.svg.append(this.line);
      this.append(this.svg);
    }
    take(v) {
      if (typeof v === "number" && isFinite(v)) this.push(v);
    }
    push(v) {
      var max = Math.min(600, Math.max(2, Number(this.getAttribute("points")) || 60));
      this.vals.push(v);
      while (this.vals.length > max) this.vals.shift();
      var lo = Math.min.apply(null, this.vals);
      var hi = Math.max.apply(null, this.vals);
      var span = hi - lo || 1;
      var step = 100 / Math.max(1, this.vals.length - 1);
      this.hidden = this.vals.length < 2;
      this.line.setAttribute(
        "points",
        this.vals
          .map(function (x, i) {
            return (i * step).toFixed(2) + "," + (28 - ((x - lo) / span) * 26).toFixed(2);
          })
          .join(" "),
      );
    }
  }

  // A part of a list line: a number with at most `digits` places (default 0), true or false as
  // On or Off, else the text.
  function part(v, digits) {
    if (typeof v === "boolean") return v ? "On" : "Off";
    if (typeof v === "number" && isFinite(v)) return String(Number(v.toFixed(digits)) || 0);
    return format(v, "text", 0);
  }

  // Lines of a source that is an array; `fields`: the keys of an item to show, in order, each
  // with an optional unit ("km:km" shows "12 km"). With `mode="append"` each message adds its
  // value as the first line (at most `max`, default 50).
  class List extends Bound {
    build() {
      this.items = [];
      this.ul = el("ul");
      this.append(this.ul);
      this.render();
    }
    take(v) {
      if (this.getAttribute("mode") === "append") {
        if (v === undefined) return;
        this.items.unshift(v);
        this.items.length = Math.min(this.items.length, Math.max(1, Number(this.getAttribute("max")) || 50));
      } else {
        this.items = Array.isArray(v) ? v : [];
      }
      this.render();
    }
    render() {
      var fields = (this.getAttribute("fields") || "").split(",").filter(Boolean);
      var digits = Math.min(20, Math.max(0, Number(this.getAttribute("digits")) || 0));
      var ul = this.ul;
      ul.textContent = "";
      if (this.items.length === 0 && this.hasAttribute("empty")) ul.append(el("li", "fm-muted", this.getAttribute("empty")));
      this.items.forEach(function (item) {
        var li = el("li");
        var parts = fields.length && item !== null && typeof item === "object" ? fields.map(function (f) {
          var i = f.indexOf(":");
          return i < 0 ? [item[f], ""] : [item[f.slice(0, i)], f.slice(i + 1)];
        }) : [[item, ""]];
        parts.forEach(function (p) {
          var none = p[0] === undefined || p[0] === null;
          li.append(el("span", "", part(p[0], digits) + (p[1] && !none ? " " + p[1] : "")));
        });
        ul.append(li);
      });
    }
  }

  // The state of a session in the words of the app: [text, badge class].
  function stateOf(s) {
    if (!s) return ["No application session", ""];
    if (s.status === "connecting") return ["Connecting", ""];
    if (s.status === "retrying") return ["Reconnecting", "warn"];
    var f = s.feature;
    if (!f) return ["Starting", ""];
    if (f.state === "running") return ["Running", "running"];
    if (f.state === "failed") return ["Failed", "bad"];
    return ["Off in this session", "warn"];
  }

  function runs(s) {
    return !!s && s.status === "running" && !!s.feature && s.feature.state === "running";
  }

  function find(list, s) {
    for (var i = 0; i < list.length; i++) if (s && list[i].application === s.application && list[i].pid === s.pid) return list[i];
    return null;
  }

  // The state badge of the session of its block.
  class State extends HTMLElement {
    connectedCallback() {
      this.stop = fm.on("fm:sessions", this.show.bind(this));
      this.show(fm.sessions());
    }
    disconnectedCallback() {
      if (this.stop) this.stop();
      this.stop = null;
    }
    show(list) {
      var st = stateOf(find(list, sessionOf(this)));
      this.className = "fm-badge " + st[1];
      this.textContent = st[0];
    }
  }

  // One block for each application session (two instances of the feature, one per application, are two sets of
  // values): the application, its state, and a copy of the child <template> (one with data-application for that
  // application first). The block body shows only while the feature runs there; else the host detail.
  class Sessions extends HTMLElement {
    connectedCallback() {
      if (!this.built) {
        this.built = true;
        this.templates = Array.prototype.filter.call(this.children, function (c) {
          return c.tagName === "TEMPLATE";
        });
        this.blocks = Object.create(null);
        this.empty = el("p", "fm-muted fm-empty", this.getAttribute("empty") || "No application session");
        this.append(this.empty);
      }
      this.stop = fm.on("fm:sessions", this.show.bind(this));
      this.show(fm.sessions());
    }
    disconnectedCallback() {
      if (this.stop) this.stop();
      this.stop = null;
    }
    template(application) {
      var t = this.templates;
      for (var i = 0; i < t.length; i++) if (t[i].getAttribute("data-application") === application) return t[i];
      for (var j = 0; j < t.length; j++) if (!t[j].hasAttribute("data-application")) return t[j];
      return null;
    }
    block(s) {
      var b = el("section", "fm-panel fm-session");
      // Set before the parts connect: they find their session through the block.
      b.fmSession = { application: s.application, pid: s.pid };
      var head = el("header");
      head.append(el("h2", "", s.application_name || s.application), document.createElement("fm-state"));
      b.detail = el("p", "fm-muted fm-detail");
      b.body = el("div", "fm-session-body");
      b.append(head, b.detail, b.body);
      return b;
    }
    // A new copy of the template for each start of the feature in the session (page.js drops
    // the data of an old start), so the parts of a new start never show old values.
    fill(b, s) {
      var start = s.feature ? String(s.instance) + "/" + String(s.feature.activation) : null;
      if (start === null || start === b.start) return;
      b.start = start;
      b.body.textContent = "";
      var t = this.template(s.application);
      // importNode: the copy belongs to this document, so its kit elements upgrade.
      if (t) b.body.append(document.importNode(t.content, true));
    }
    show(list) {
      var self = this;
      var now = Object.create(null);
      list.forEach(function (s) {
        var k = s.application + ":" + s.pid;
        now[k] = true;
        var b = self.blocks[k];
        if (!b) {
          b = self.blocks[k] = self.block(s);
          self.append(b);
        }
        self.fill(b, s);
        var on = runs(s);
        b.body.hidden = !on;
        var why = on ? "" : (s.feature && s.feature.detail) || s.error || "";
        b.detail.textContent = why;
        b.detail.hidden = !why;
      });
      Object.keys(this.blocks).forEach(function (k) {
        if (now[k]) return;
        self.blocks[k].remove();
        delete self.blocks[k];
      });
      this.empty.hidden = list.length > 0;
    }
  }

  // The top of a page: the banner of the feature (the app serves it at _fm/art/banner; `src` for
  // a page in a subfolder), a shade, `title` and `subtitle`. Child nodes go above the title.
  // With no `subtitle`, it shows the application of the page's home (fm.application(), "fm:context"): one page
  // instance serves one application.
  class Hero extends HTMLElement {
    connectedCallback() {
      if (this.built) return;
      this.built = true;
      var self = this;
      var art = el("div", "fm-hero-art");
      var img = el("img");
      img.alt = "";
      img.addEventListener("error", function () {
        self.classList.add("fm-no-art");
      });
      img.src = this.getAttribute("src") || "_fm/art/banner";
      art.append(img);
      var title = el("div", "fm-hero-title");
      while (this.firstChild) title.append(this.firstChild);
      title.append(el("h1", "", this.getAttribute("title") || document.title));
      var fixed = this.getAttribute("subtitle");
      var sub = el("p", "", fixed || "");
      title.append(sub);
      function application() {
        var g = fm.application();
        sub.textContent = g ? fm.applicationName() || g : "";
        sub.hidden = !g;
      }
      if (!fixed) {
        application();
        fm.on("fm:context", application);
      }
      this.append(art, el("div", "fm-hero-shade"), title);
    }
  }

  // A setting element: it shows the stored value, writes a change at once and shows the stored
  // value again when the app refuses it.
  class Setting extends HTMLElement {
    connectedCallback() {
      if (this.built) return;
      this.built = true;
      this.id_ = this.getAttribute("setting") || "";
      this.append(el("span", "fm-label", this.getAttribute("label") || this.id_));
      this.build();
      this.err = el("span", "fm-error");
      this.err.hidden = true;
      this.append(this.err);
      this.stop = fm.on("fm:settings", this.sync.bind(this));
      this.sync(fm.settings());
    }
    disconnectedCallback() {
      if (this.stop) this.stop();
      this.stop = null;
      this.built = false;
      this.textContent = "";
    }
    // The stored value is always kept, also while a write waits, so a refusal shows the newest
    // stored value (Sol r1); the control shows the written value until the answer.
    sync(values) {
      this.stored = values ? values[this.id_] : undefined;
      if (!this.busy) this.show(this.stored);
    }
    write(value) {
      var self = this;
      self.busy = true;
      self.err.hidden = true;
      self.show(value);
      fm.setSetting(self.id_, value).then(
        function () {
          self.busy = false;
          self.sync(fm.settings());
        },
        function (e) {
          self.busy = false;
          self.err.textContent = String(e && e.message ? e.message : e);
          self.err.hidden = false;
          self.show(self.stored);
        },
      );
    }
  }

  class Toggle extends Setting {
    build() {
      var self = this;
      this.btn = el("button", "fm-switch");
      this.btn.type = "button";
      this.btn.setAttribute("role", "switch");
      this.btn.setAttribute("aria-label", this.getAttribute("label") || this.id_);
      this.btn.addEventListener("click", function () {
        self.write(self.btn.getAttribute("aria-checked") !== "true");
      });
      this.append(this.btn);
    }
    show(v) {
      this.btn.setAttribute("aria-checked", v === true ? "true" : "false");
      this.btn.disabled = typeof v !== "boolean";
    }
  }

  class Range extends Setting {
    build() {
      var self = this;
      var box = el("span", "fm-control");
      this.input = el("input");
      this.input.type = "range";
      ["min", "max", "step"].forEach(function (a) {
        if (self.hasAttribute(a)) self.input.setAttribute(a, self.getAttribute(a));
      });
      this.input.setAttribute("aria-label", this.getAttribute("label") || this.id_);
      this.out = el("output");
      this.input.addEventListener("input", function () {
        self.out.textContent = self.text(Number(self.input.value));
      });
      this.input.addEventListener("change", function () {
        self.write(Number(self.input.value));
      });
      box.append(this.input, this.out);
      this.append(box);
    }
    // The digits of the grid min + k * step, as the app's settings form (SettingsForm.tsx
    // shown): min 0.5 with step 1 shows 1.5, step 1e-7 shows 7 places (Sol r3). The value
    // itself can carry float noise, so its own digits do not count.
    text(v) {
      return v.toFixed(Math.min(20, Math.max(decimals(this.getAttribute("step")), decimals(this.getAttribute("min")))));
    }
    show(v) {
      var ok = typeof v === "number" && isFinite(v);
      this.input.disabled = !ok;
      if (ok) this.input.value = String(v);
      this.out.textContent = ok ? this.text(v) : "-";
    }
  }

  // A select setting as cards (design 14): one radio for each option, with its label and info.
  // `options` is a property ([{ value, label, info }]) that fm-setting sets.
  class Choice extends Setting {
    build() {
      var self = this;
      var group = el("div", "fm-choices");
      group.setAttribute("role", "radiogroup");
      group.setAttribute("aria-label", this.getAttribute("label") || this.id_);
      this.cards = (this.options || []).map(function (o) {
        var b = el("button", "fm-choice");
        b.type = "button";
        b.setAttribute("role", "radio");
        b.fmValue = o.value;
        b.append(el("b", "", o.label));
        if (o.info) b.append(el("span", "", o.info));
        b.addEventListener("click", function () {
          self.write(o.value);
        });
        return b;
      });
      group.append.apply(group, this.cards);
      this.append(group);
    }
    show(v) {
      this.cards.forEach(function (b) {
        b.setAttribute("aria-checked", b.fmValue === v ? "true" : "false");
        b.disabled = typeof v !== "string";
      });
    }
  }

  // A select setting with more options than cards fit: a dropdown.
  class Select extends Setting {
    build() {
      var self = this;
      this.select = el("select");
      this.select.setAttribute("aria-label", this.getAttribute("label") || this.id_);
      (this.options || []).forEach(function (o) {
        var opt = el("option", "", o.label);
        opt.value = o.value;
        self.select.append(opt);
      });
      this.select.addEventListener("change", function () {
        self.write(self.select.value);
      });
      this.append(this.select);
    }
    show(v) {
      this.select.disabled = typeof v !== "string";
      if (typeof v === "string") this.select.value = v;
    }
  }

  // A number, text or color setting: one input (`kind`), written at its change.
  class Field extends Setting {
    build() {
      var self = this;
      var kind = this.getAttribute("kind") || "text";
      this.input = el("input");
      this.input.type = kind;
      ["min", "max", "step"].forEach(function (a) {
        if (self.hasAttribute(a)) self.input.setAttribute(a, self.getAttribute(a));
      });
      this.input.setAttribute("aria-label", this.getAttribute("label") || this.id_);
      // An empty or bad number is not 0: the field shows the stored value again, as Manage does
      // (Sol review r1).
      this.input.addEventListener("change", function () {
        var raw = self.input.value;
        if (kind === "number" && (raw.trim() === "" || !isFinite(Number(raw)))) return self.show(self.stored);
        self.write(kind === "number" ? Number(raw) : raw);
      });
      this.append(this.input);
    }
    show(v) {
      var ok = v !== undefined && v !== null;
      this.input.disabled = !ok;
      if (ok) this.input.value = String(v);
    }
  }

  // A button that writes fixed values (`values`: a JSON object of setting ids) through the app, as
  // a widget action does. It is off until the app gives the settings, and while the stored values
  // are already those values. A refusal shows the error.
  class SetButton extends HTMLElement {
    connectedCallback() {
      if (!this.built) {
        this.built = true;
        var self = this;
        this.btn = el("button", "fm-button", this.getAttribute("label") || "");
        this.btn.type = "button";
        this.btn.addEventListener("click", function () {
          self.write();
        });
        this.err = el("span", "fm-error");
        this.err.hidden = true;
        this.append(this.btn, this.err);
      }
      this.stop = fm.on("fm:settings", this.sync.bind(this));
      this.sync(fm.settings());
    }
    disconnectedCallback() {
      if (this.stop) this.stop();
      this.stop = null;
    }
    values() {
      try {
        var v = JSON.parse(this.getAttribute("values") || "{}");
        return v && typeof v === "object" && !Array.isArray(v) ? v : {};
      } catch (e) {
        return {};
      }
    }
    sync(stored) {
      var v = this.values();
      var keys = Object.keys(v);
      var known = !!stored && keys.length > 0 && keys.every(function (k) {
        return k in stored;
      });
      var same = known && keys.every(function (k) {
        return stored[k] === v[k];
      });
      this.btn.disabled = !!this.busy || !known || same;
    }
    write() {
      var self = this;
      var v = this.values();
      self.busy = true;
      self.err.hidden = true;
      self.btn.disabled = true;
      var done = function (e) {
        self.busy = false;
        if (e !== undefined) {
          self.err.textContent = String(e && e.message ? e.message : e);
          self.err.hidden = false;
        }
        self.sync(fm.settings());
      };
      Promise.all(
        Object.keys(v).map(function (k) {
          return fm.setSetting(k, v[k]);
        }),
      ).then(function () {
        done();
      }, done);
    }
  }

  // Cards fit up to this many options; more show as a dropdown.
  var CARDS = 4;

  // One setting of feature.json, drawn from its schema (fm.schema): toggle, range, select (cards
  // or dropdown), number, text or color, with its info line. It follows visible_if as Manage does.
  class SettingOf extends HTMLElement {
    connectedCallback() {
      this.stops = [fm.on("fm:schema", this.draw.bind(this)), fm.on("fm:settings", this.visible.bind(this))];
      this.draw(fm.schema());
    }
    disconnectedCallback() {
      this.stops.forEach(function (s) {
        s();
      });
      this.stops = [];
    }
    draw(schema) {
      var id = this.getAttribute("setting");
      var d = (schema || []).filter(function (x) {
        return x.id === id;
      })[0];
      var sig = d ? JSON.stringify(d) : "";
      if (sig === this.sig) return;
      this.sig = sig;
      this.def = d;
      this.textContent = "";
      if (!d) return;
      var tag = { toggle: "fm-toggle", range: "fm-range", number: "fm-field", text: "fm-field", color: "fm-field" }[d.type];
      if (d.type === "select") tag = d.options.length <= CARDS ? "fm-choice" : "fm-select";
      if (!tag) return;
      var c = document.createElement(tag);
      c.setAttribute("setting", d.id);
      c.setAttribute("label", d.label);
      ["min", "max", "step"].forEach(function (a) {
        if (typeof d[a] === "number") c.setAttribute(a, String(d[a]));
      });
      if (tag === "fm-field") c.setAttribute("kind", d.type);
      c.options = d.options;
      this.append(c);
      if (d.info) this.append(el("span", "fm-info", d.info));
      this.visible(fm.settings());
    }
    visible(values) {
      var v = this.def && this.def.visible_if;
      this.hidden = !!v && (values || {})[v.setting] !== v.equals;
    }
  }

  customElements.define("fm-stat", Stat);
  customElements.define("fm-sparkline", Sparkline);
  customElements.define("fm-list", List);
  customElements.define("fm-state", State);
  customElements.define("fm-sessions", Sessions);
  customElements.define("fm-hero", Hero);
  customElements.define("fm-toggle", Toggle);
  customElements.define("fm-range", Range);
  customElements.define("fm-choice", Choice);
  customElements.define("fm-select", Select);
  customElements.define("fm-field", Field);
  customElements.define("fm-setting", SettingOf);
  customElements.define("fm-set", SetButton);
})();
