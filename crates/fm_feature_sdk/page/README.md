# Feature pages: the page SDK, version 1

A feature with the `ui_app` capability can have a page: your HTML in `ui/`, which Feature Machine
shows in a sandboxed frame (the feature page and its pop-out window). The page has no access to
the app or the network. It talks to your feature in the game through the page SDK.

## Files

The app serves the SDK at `_fm/1/` under the folder of your page. Link the files relative to your
page file. Do not put an `_fm` folder in your package: `fm_pack` refuses it.

```html
<!-- ui/index.html -->
<link rel="stylesheet" href="_fm/1/kit.css" />
<script src="_fm/1/page.js"></script>
<script src="_fm/1/kit.js"></script>
<script src="page.js"></script>

<!-- ui/pages/index.html -->
<script src="../_fm/1/page.js"></script>
```

Use classic scripts. Module scripts do not load in the sandboxed frame. The page CSP allows only
the files of your own page folder: no inline script or style, no network.

## `window.fm` (page.js)

| Call | What it does |
| --- | --- |
| `fm.on(topic, fn)` | `fn(data, { game, pid })` for each message that your feature sends with `Host::ui(topic, json)`. Gives a function that stops it. |
| `fm.on("fm:sessions", fn)` | `fn(sessions)` when the game sessions change. |
| `fm.on("fm:settings", fn)` | `fn(values)` when the stored settings change (Manage, a widget, your page). |
| `fm.on("fm:theme", fn)` | `fn(theme)` when the app gives its theme. |
| `fm.on("fm:send_failed", fn)` | `fn({ topic, error })` when a message did not get to the game. |
| `fm.send(topic, data, session?)` | A message to your feature in the game: `session` is `{ game, pid }`, or no session for each session where the feature runs. Your feature gets it as a page event. |
| `fm.sessions()` | The game sessions now: `game`, `pid`, `status` (`connecting`, `running`, `retrying`) and the state of your feature there. |
| `fm.settings()` | The values of your settings (the `settings` of `feature.json`) for the version of this page. |
| `fm.setSetting(id, value)` | Stores one setting. The app checks it as Manage does: a value that does not fit is refused. The promise ends when it is stored. |
| `fm.theme` | `{ scheme, colors }`. page.js also sets the colors as CSS variables (`--fm-bg`, `--fm-text`, `--fm-dim`, `--fm-accent`, ...). |
| `fm.ready()` | Call it when your page is ready. Then page.js sends a beat each second. A page that misses 3 beats is unloaded and the app shows "Not responding" with Reload. A page that does not call `ready()` gets no beat check. |

Topics of your feature never start with `fm:`. Do not keep secrets in a page.

## The kit (kit.css and kit.js)

`kit.css` gives the app look: the theme colors, the sans font, cards (`.fm-card`), a grid
(`.fm-grid`), badges (`.fm-badge running|warn|bad`), `.fm-muted`, `.fm-error` and `.fm-num`
(tabular digits for numbers). Use the sans font for words, and tabular digits for numbers that
change or align.

`kit.js` gives these elements (load page.js first):

| Element | Use |
| --- | --- |
| `<fm-stat label="Speed" unit="kt" digits="0">` | A label and a value. Set `element.value`. |
| `<fm-toggle setting="alert" label="Alert">` | Shows and changes a toggle setting. |
| `<fm-range setting="strength" label="Strength" min="0" max="1" step="0.05">` | Shows and changes a range setting. |

A setting element shows the stored value, writes a change at once and shows the stored value
again with the error when the app refuses it.

## Widgets

Widgets are not pages: the app draws them from a views file, with no code of yours. See
`crates/fm_install/VIEWS.md`.
