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

Each page instance serves one game: the game of the home that shows it (a home page, a widget,
a pop-out window). The app sends the page only the sessions and messages of that game and of
that install. `fm.game()` gives the game id, so the page can show the words and behavior of that
game.

| Call | What it does |
| --- | --- |
| `fm.on(topic, fn)` | `fn(data, { game, pid })` for each message that your feature sends with `Host::ui(topic, json)`. Gives a function that stops it. |
| `fm.on("fm:context", fn)` | `fn({ game })` when the app gives the game of the page's home. It comes before the settings and the sessions. |
| `fm.on("fm:sessions", fn)` | `fn(sessions)` when the game sessions change. |
| `fm.on("fm:settings", fn)` | `fn(values)` when the stored settings change (Manage, a widget, your page). |
| `fm.on("fm:theme", fn)` | `fn(theme)` when the app gives its theme. |
| `fm.on("fm:send_failed", fn)` | `fn({ topic, error })` when a message did not get to the game. |
| `fm.send(topic, data, session?)` | A message to your feature in the game: `session` is `{ game, pid }`, or no session for each session where the feature runs. Your feature gets it as a page event. |
| `fm.game()` | The game id of the page's home (`ats`, `msfs2024`), or `null` before `fm:context`. |
| `fm.sessions()` | The game sessions now: `game`, `pid`, `status` (`connecting`, `running`, `retrying`) and the state of your feature there. |
| `fm.settings()` | The values of your settings (the `settings` of `feature.json`) for the version of this page. |
| `fm.setSetting(id, value)` | Stores one setting. The app checks it as Manage does: a value that does not fit is refused. The promise ends when it is stored. |
| `fm.schema()` | The settings of `feature.json` for the version of this page (`fm:schema` when they come). |
| `fm.last(topic, session)` | The newest data of `topic` from one session (`{ game, pid }`) of the current start of your feature, or `undefined`. |
| `fm.theme` | `{ scheme, colors }`. page.js also sets the colors as CSS variables (`--fm-bg`, `--fm-text`, `--fm-dim`, `--fm-accent`, ...). |
| `fm.ready()` | Call it when your page is ready. Then page.js sends a beat each second. A page that misses 3 beats is unloaded and the app shows "Not responding" with Reload. A page that does not call `ready()` gets no beat check. |

Topics of your feature never start with `fm:`. Do not keep secrets in a page.

## The kit (kit.css and kit.js)

`kit.css` gives the app look: the theme colors, the sans font, cards (`.fm-card`), a grid
(`.fm-grid`), badges (`.fm-badge running|warn|bad`), `.fm-muted`, `.fm-error` and `.fm-num`
(tabular digits for numbers). Use the sans font for words, and tabular digits for numbers that
change or align.

Page layout (design 14): put `class="fm-page"` on `<body>`, an `<fm-hero>` first, then a
`<div class="fm-body">` with panels (`<section class="fm-panel">` with a `<header><h2>`),
`.fm-cols` for two columns that stack in a narrow window, `.fm-read` for a row of values in
boxes, and `.fm-button` for a button. The app's Library and Manage buttons float over the top of
the page: the hero goes under them, and a page without a hero starts below them.

`kit.js` gives these elements (load page.js first):

| Element | Use |
| --- | --- |
| `<fm-hero title="Trip">` | The banner of your feature (`_fm/art/banner`; set `src="../_fm/art/banner"` in a subfolder page), the title, and the name of the page's game. A `subtitle` attribute replaces the game name. |
| `<fm-sessions>` | One panel for each game session, with the game and its state. Put a `<template>` in it (and a `<template data-game="ats">` for one game): each session gets a copy. Each start of the feature is isolated, so each copy shows only its own session. The copy shows only while your feature runs there; else the reason. |
| `<fm-state>` | The state of the session of its panel: Running, Starting, Connecting, Reconnecting, Off in this session, Failed. |
| `<fm-stat label="Speed" unit="kt" digits="0">` | A label and a value. Set `element.value`, or give a source: `topic="state" path="/speed_kt"`. `format`: `number` (default), `duration` (seconds), `onoff`, `text`; `scale` multiplies a number. `<data value="active">On</data>` children give the words of a value. |
| `<fm-sparkline topic="state" path="/speed_kt" points="60">` | A line of the last values of a source (or `element.push(value)`), hidden until it has 2 values. |
| `<fm-list topic="stations" path="" fields="name,distance:km">` | Lines of a source that is an array (a field can have a unit after a colon); `mode="append"` adds each new value as the first line (`max`, default 50); `empty` is the text of no line; a number shows at most `digits` places, true and false show On and Off. |
| `<fm-setting setting="look">` | A setting of `feature.json`, drawn from its type with its label and info: a switch, a slider, cards for a select of up to 4 options (with the option `info`), a dropdown for more, or an input. It follows `visible_if`. |
| `<fm-set label="Use the game speeds" values='{"wet": "1", "dry": "1"}'>` | A button that writes fixed setting values, as a widget action. It is off while the stored values are already those values. |
| `<fm-toggle>`, `<fm-range>` | The setting controls of version 1 (`fm-setting` draws them for you). |

In a `<fm-sessions>` template, a source reads only the messages of that session. Outside it, a
source reads the messages of any session.

A setting element shows the stored value, writes a change at once and shows the stored value
again with the error when the app refuses it.

`_fm/art/banner` and `_fm/art/icon` are the `banner` and `icon` of your `feature.json`.

## Widgets

Widgets are not pages: the app draws them from a views file, with no code of yours. See
`crates/fm_install/VIEWS.md`.
