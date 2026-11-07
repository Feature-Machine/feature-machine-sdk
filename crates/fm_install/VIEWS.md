# Widgets: the views format

A widget is a small card on the member's home grid. Feature Machine draws it from a views file in
your package. No code of yours runs on the home grid, and the same file shows on the dashboard.
`fm_pack` and the install check the file with the rules below (`crates/fm_install/src/views.rs`).

## Declare a widget

In `feature.json` (API 6, with `min_app`):

```json
"widgets": [
  { "id": "speed", "name": "Speed", "sizes": ["1x1", "2x1"], "views": "widgets/speed.json" }
]
```

- `id`: 1 to 32 of a-z, 0-9 and `_`, first a letter. At most 8 widgets.
- `sizes`: `1x1` (small), `2x1` (wide), `2x2` (large). The member picks one of them.
- `views`: a JSON file in `widgets/`, at most 32 KB for each size. Two widgets can name one file
  when they have the same sizes.
- Images for `image` nodes: WebP, PNG or JPEG in `widgets/`, at most 256 KB and 512 x 512 px.

## The views file

One view tree for each size of the widget (and no other size), and the actions:

```json
{
  "1x1": { "type": "stack", "children": [
    { "type": "value", "source": { "topic": "state", "path": "/speed_kt" }, "unit": "kt", "digits": 0 },
    { "type": "text", "text": "Ground speed", "style": "caption" }
  ] },
  "2x1": { "type": "row", "children": [
    { "type": "gauge", "source": { "topic": "state", "path": "/speed_kt" }, "min": 0, "max": 160, "label": "Speed", "unit": "kt" },
    { "type": "toggle", "label": "Alert", "state": { "setting": "alert" }, "on": "alert_on", "off": "alert_off" }
  ] },
  "actions": [
    { "id": "alert_on", "setting": "alert", "value": true },
    { "id": "alert_off", "setting": "alert", "value": false }
  ]
}
```

## Sources

A node reads its value from one source:

- `{ "topic": "state", "path": "/speed_kt" }`: the newest message of that topic that your feature
  sent with `Host::ui`, and a JSON Pointer (RFC 6901) into its data (`""` is the whole data). The
  widget shows the application session of its home that connected last. When no session runs the feature,
  the widget shows the last values of the same version. When that session reports the feature
  as not running (blocked, stopped), the widget shows "Off in this session" and the reason; a
  failed feature shows "Failed".
- `{ "setting": "alert" }`: the member's stored value of a setting of `feature.json`. A `value` node of a select setting shows the label of the option.

## Nodes

Each node has a `type` and only the fields below. An unknown node or field is refused.

| Type | Fields |
| --- | --- |
| `stack` | `children` (top to bottom) |
| `row` | `children` (left to right). A row of two or more `value` nodes only shows as boxes: the label over a small value. |
| `text` | `text` (1 to 80 characters), `style`: `title`, `body` or `caption` |
| `value` | `source`, `label`, `unit` (12 characters), `digits` (0 to 6) |
| `gauge` | `source`, `min`, `max`, `label`, `unit` |
| `bar` | `source`, `min`, `max`, `label` |
| `sparkline` | `source` (a topic), `points` (2 to 120) |
| `image` | `src` (a package image in `widgets/`), `alt` |
| `toggle` | `label`, `state` (a source that is true or false), `on` and `off` (action ids) |
| `button` | `label`, `action` (an action id) |
| `slider` | `setting` (a range setting), `label`. The member drags it; the app stores the value as Manage does. |
| `choice` | `setting` (a select setting of at most 4 options), `label`. One segment for each option. |
| `spacer` | none |

Limits per view tree: 64 nodes, 8 levels. Labels are at most 40 characters. Text always shows as
text: no markup.

## Actions

At most 16, each with an `id` and exactly one form:

- `{ "id": "reset", "send": { "topic": "trip.reset", "data": {} } }`: a page message to the running
  feature (needs `ui_app`). It goes only to the start of the feature that the widget shows; the
  button is off when the feature does not run.
- `{ "id": "alert_on", "setting": "alert", "value": true }`: stores a setting, as Manage does. The
  value must fit the setting.
- `{ "id": "open", "open": "page" }`: opens the feature page (needs a page).

The widget draws at most 10 times each second. Send state messages at a normal rate; the widget
shows the newest one.
