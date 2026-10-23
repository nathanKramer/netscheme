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
| `window.rs` | Event loop and re-render on resize | Browser chrome |

## The language today

- Atoms: numbers, `"strings"`, `true`, `false`, `nil`, `symbols`, `:keywords`
- Special forms: `define`, `fn`, `if`, `cond`, `let`, `and`, `or`, `do`,
  `quote` / `'`, `quasiquote` / `` ` `` with `,` and `,@`, and `eval`
- Builtins: `+ - * / mod = < > not str repr`, the list functions
  `list first rest nth cons length empty? map filter`, and the predicates
  `list? string? number? symbol? keyword? fn?`
- Block elements: `page div row h1 h2 h3 p frame`. Inline elements: `span b a`.
  All of them are ordinary functions:
  `(tag :attr value ... children ...)`. Lists of children are spliced in,
  and `nil` is skipped, so `if` and functions that return lists compose naturally.
- Style attributes: `:color`, `:background`, `:size`, `:bold`, `:underline`, `:padding`,
  `:padding-left`, `:margin`.
  Padding and margin apply only to block elements.
- Links: `(a :href "other.sx" ...)` opens a local page, resolved relative to the
  current page. Go back with Alt+← or the mouse's back button.
  A link inside a frame navigates the whole window.

Scroll with a trackpad, mouse wheel, arrow keys, Page Up/Down, Space and
Shift+Space, or Home/End. Going back restores where you'd scrolled to.

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
- `(row ...)` lays its children out side by side in equal columns.

`pages/view-source.sx` is view source written as an ordinary page, and it can
show its own source. `pages/fractal.sx` frames itself twice.

## Roadmap

1. **Reusable styles**: `(style ...)` rules, where selectors are just predicates on elements.
2. **Interactivity**: `:on-click` handlers. State changes re-evaluate the page and
   re-render it, React-style, so there is no mutable DOM API to design.
3. **Networking**: load `http://` pages, and add forward history.
4. **Chrome as a page**: the address bar, back button, and view source live in
   `chrome.sx`, which shows the current site in a `frame`.
5. More of the language: `let`, `map`, `filter`, macros, tail calls.
