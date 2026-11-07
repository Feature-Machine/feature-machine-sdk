# Page SDK v2

Link `_fm/2/page.js`, `_fm/2/kit.js` and `_fm/2/kit.css` relative to the page.
Call `fm.ready()` after the page is ready. The holder then sends neutral context and sessions.
Use `fm.application()` and `{ application, pid }` for session selection. Messages use `fm: 2`.
Feature message data stays opaque. The v1 files and messages remain available unchanged.
