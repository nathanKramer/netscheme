# Toy Browser

A toy web browser whose pages are written in a small Lisp instead of
HTML, CSS, and JavaScript. One language covers all three jobs:

```lisp
(define (greeting name) (str "Hello, " name "!"))   ; behavior

(page :title "Hello"
  (h1 :color "#b8336a" (greeting "world"))           ; structure + style
  (p "6 × 7 = " (* 6 7)))
```

A page is a program. The browser evaluates it, and the last value it
produces, a `(page ...)` element, is the document.

```sh
cargo run                                   # opens pages/hello.sx
cargo run -- path/to/page.sx
cargo run -- page.sx --screenshot out.png --width 800 --height 600 --scroll 200

# Screenshots show the bare page. To include the toolbar, screenshot the chrome:
cargo run -- 'browser:chrome?location=pages/hello.sx' --screenshot out.png
```

## Technology

| Concern | Choice | Why |
|---|---|---|
| Language | Rust | Fast enough to re-layout every frame, and enums plus pattern matching suit an interpreter. |
| Window and input | `winit` | Cross-platform, including Wayland and X11. |
| Pixels | `softbuffer` | A plain CPU framebuffer. We paint every pixel ourselves, the way a simple browser engine does. |
| Text | `fontdue` + bundled Liberation Sans | Rasterizes glyphs. Shaping and kerning are left out for now. |
| Screenshots | `png` | Headless rendering for checking pages and, later, golden-image tests. |

No GUI toolkit is used on purpose. Parsing, styling, layout, and
painting are the interesting parts of a browser, so we write them.

## Pipeline

```
source ──reader──▶ s-expressions ──eval──▶ element tree ──style──▶ styled boxes
       ──layout──▶ display list ──paint──▶ pixels
```

| Module | Role | Browser analogue |
|---|---|---|
| `lisp/reader.rs` | Text to `Value`s | HTML tokenizer and parser |
| `lisp/eval.rs`, `builtins.rs` | Runs the page program | JS engine, plus building the DOM |
| `loader.rs` | Locations, query parameters, and loading pages into frames | Networking and navigation |
| `dom.rs` | `Element` / `Node` tree | DOM |
| `style.rs` | Tag defaults, then attributes, with inheritance | CSS cascade |
| `layout.rs` | Block flow, inline runs, and word wrapping, producing a display list and link boxes | Layout engine |
| `paint.rs` | Rectangles and glyphs into a pixel buffer | Painting and compositing |
| `window.rs` | Event loop, scrolling, and navigation; shows `browser:chrome` | The browser process |

## The language today

- Atoms: numbers, `"strings"`, `true`, `false`, `nil`, `symbols`, `:keywords`
- Special forms: `define`, `define-state`, `set!`, `fn`, `if`, `cond`, `let`, `and`, `or`, `do`,
  `quote` / `'`, `quasiquote` / `` ` `` with `,` and `,@`, and `eval`
- Builtins: `+ - * / mod = < > not str repr url-encode`, the list functions
  `list first rest nth cons length empty? map filter`, the predicates
  `list? string? number? symbol? keyword? fn?`, and `tag`, `attr` and
  `children` for looking inside elements
- Block elements: `page div row h1 h2 h3 p frame`. Inline elements: `span b a button`.
  All of them are ordinary functions:
  `(tag :attr value ... children ...)`. Lists of children are spliced in,
  and `nil` is skipped, so `if` and functions that return lists compose naturally.
- Style attributes: `:color`, `:background`, `:size`, `:bold`, `:underline`, `:padding`,
  `:padding-left`, `:margin`.
  Margin applies only to block elements; padding on an inline element pads its
  left and right.
- Links: `(a :href "other.sx" ...)` opens a local page, resolved relative to the
  current page. An href starting with `/` is relative to the working directory.
  Go back with Alt+←, the mouse's back button, or the toolbar.

Scroll with a trackpad, mouse wheel, arrow keys, Page Up/Down, Space and
Shift+Space, or Home/End. This scrolls the frame under the cursor, if it has a
fixed height, and otherwise the page. Going back restores where you'd scrolled to.

## State and clicks

```lisp
(define-state count 0)

(page
  (h1 "Clicked " count " times")
  (button :on-click (fn () (set! count (+ count 1))) "Click me"))
```

- `(define-state name initial)` defines a variable that keeps its value when
  the page runs again. `initial` is only evaluated the first time.
- `(set! name value)` changes a variable, state or not.
- `:on-click` takes a function of no arguments, and works on any element.
  Clicking runs it, then the browser runs the whole page again with the new
  state and redraws. Nothing edits the document in place.

Each frame keeps its own state, keyed by its position among the frames, so two
frames of the same page count separately. Following a link starts the new
page with fresh state; going back restores the old page's state.

`--click X,Y` (repeatable) clicks before taking a screenshot:
`cargo run -- pages/counter.sx --screenshot out.png --click 60,92`.

## Code as data

Elements print as the code that builds them, so `(repr (p :color "red" "hi"))`
gives `"(p :color \"red\" \"hi\")"`, and reading that back builds the same element.

- `(source "page.sx")` returns another page's code as a list of forms without
  running it. Comments aren't kept.
- `(param "name")` reads a query parameter from the page's location, as in
  `view-source.sx?page=hello.sx`.
- `(frame :src "page.sx" :scale 0.5)` embeds another page, which runs in its own
  environment and is drawn at the given scale. A page may frame itself: frames
  more than 8 deep, or drawn at under 4% of full size, are left empty.
  With `:height` (pixels, or `"fill"` to reach the bottom of the window) the
  frame clips its page and scrolls it independently. `:border false` hides the border.
- `(row ...)` lays its children out side by side in equal columns.

`pages/fractal.sx` frames itself twice.

## The browser is a page

Locations starting with `browser:` are the browser's own pages, kept in `ui/`:

- `browser:chrome?location=<site>` is the toolbar. The window always shows this
  page, which puts the site in a `(frame :height "fill")` below the toolbar. It
  reads the site's title out of the frame's document tree, and that becomes the
  window title. Edit `ui/chrome.sx` to change the browser's interface.
- `browser:view-source?page=<site>` shows a page's source, and can show its own.
- `browser:back` isn't a page: following a link to it goes back.

Links on browser pages are relative to the working directory, and any link,
whether in the toolbar or the site, navigates the site. Values inside a query,
like the site's own location, are percent-encoded.

The chrome can frame itself, too:
`cargo run -- 'browser:chrome?location=pages/hello.sx'` opens a browser inside a browser.

## Roadmap

1. **Reusable styles**: `(style ...)` rules, where selectors are just predicates on elements.
2. **Text input**: an `input` element with keyboard focus and editing.
3. **Networking**: load `http://` pages, and add forward history.
4. **An editable address bar** in `chrome.sx`, built from `input` and `define-state`.
5. More of the language: macros, tail calls.
