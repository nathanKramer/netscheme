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
cargo run -- page.sx --screenshot out.png --width 800 --height 600
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
| `dom.rs` | `Element` / `Node` tree | DOM |
| `style.rs` | Tag defaults, then attributes, with inheritance | CSS cascade |
| `layout.rs` | Block flow, inline runs, and word wrapping, producing a display list and link boxes | Layout engine |
| `paint.rs` | Rectangles and glyphs into a pixel buffer | Painting and compositing |
| `window.rs` | Event loop and re-render on resize | Browser chrome |

## The language today

- Atoms: numbers, `"strings"`, `true`, `false`, `nil`, `symbols`, `:keywords`
- Special forms: `define`, `fn`, `if`, `do`, `quote` / `'`
- Builtins: `+ - * / = < > str list`
- Block elements: `page div h1 h2 h3 p`. Inline elements: `span b a`.
  All of them are ordinary functions:
  `(tag :attr value ... children ...)`. Lists of children are spliced in,
  and `nil` is skipped, so `if` and functions that return lists compose naturally.
- Style attributes: `:color`, `:background`, `:size`, `:bold`, `:underline`, `:padding`, `:margin`.
  Padding and margin apply only to block elements.
- Links: `(a :href "other.sx" ...)` opens a local page, resolved relative to the
  current page. Go back with Alt+← or the mouse's back button.

## Roadmap

1. **Scrolling** and a viewport larger than the window.
2. **Reusable styles**: `(style ...)` rules, where selectors are just predicates on elements.
3. **Interactivity**: `:on-click` handlers. State changes re-evaluate the page and
   re-render it, React-style, so there is no mutable DOM API to design.
4. **Networking**: load `http://` pages, and add forward history.
5. **Chrome**: an address bar drawn with the same layout engine.
6. More of the language: `let`, `map`, `filter`, macros, tail calls.
