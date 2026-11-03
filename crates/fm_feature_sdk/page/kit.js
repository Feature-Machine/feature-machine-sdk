// Feature Machine page kit, version 1 (feature surfaces plan, section 6, FS2): web components for
// feature pages, styled by kit.css. Load page.js first. The elements use the light DOM (no shadow
// root), so the page CSP needs no inline styles and a page can style them. Setting elements write
// through fm.setSetting, which the app checks against the feature's settings, as Manage does.
//   <fm-stat label="Speed" unit="kt"></fm-stat>          el.value = 118
//   <fm-toggle setting="alert" label="Alert"></fm-toggle>
//   <fm-range setting="strength" label="Strength" min="0" max="1" step="0.05"></fm-range>
(function () {
  "use strict";
  if (!window.fm || !window.customElements || window.customElements.get("fm-stat")) return;
  var fm = window.fm;

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

  // A label, a value and a unit. `value`: a number (shown with `digits`) or a text.
  class Stat extends HTMLElement {
    connectedCallback() {
      if (this.built) return;
      this.built = true;
      this.labelEl = el("span", "fm-label", this.getAttribute("label") || "");
      var line = el("span");
      this.valueEl = el("span", "fm-value", "-");
      this.unitEl = el("span", "fm-unit", this.getAttribute("unit") || "");
      line.append(this.valueEl, this.unitEl);
      this.append(this.labelEl, line);
      this.show();
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
      var d = Number(this.getAttribute("digits") || 0);
      var v = this.v;
      this.valueEl.textContent = typeof v === "number" && isFinite(v) ? (Math.round(v * Math.pow(10, d)) / Math.pow(10, d) || 0).toFixed(d) : v === undefined || v === null ? "-" : String(v);
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

  customElements.define("fm-stat", Stat);
  customElements.define("fm-toggle", Toggle);
  customElements.define("fm-range", Range);
})();
