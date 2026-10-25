# Netscheme

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

The browser opens on its own guide, which is written in Netscheme: start at
`pages/hello.sx` for a tour of the language, its library, and how the browser
works, with live examples that are printed from the same code they run.

```sh
cargo run                                   # opens pages/hello.sx
cargo run -- path/to/page.sx
cargo run -- http://localhost:8000/hello.sx # after: cd pages && python3 -m http.server
cargo run -- page.sx --screenshot out.png --width 800 --height 600 --scroll 200

# Screenshots show the bare page; add --chrome to include the toolbar:
cargo run -- pages/hello.sx --chrome --screenshot out.png
```

## Technology

| Concern | Choice | Why |
|---|---|---|
| Language | Rust | Fast enough to re-layout every frame, and enums plus pattern matching suit an interpreter. |
| Window and input | `winit` | Cross-platform, including Wayland and X11. |
| Pixels | `softbuffer` | A plain CPU framebuffer. We paint every pixel ourselves, the way a simple browser engine does. |
| Text | `fontdue` + bundled Liberation Sans and Mono | Rasterizes glyphs. Shaping and kerning are left out for now. |
| Screenshots | `png` | Headless rendering for checking pages and, later, golden-image tests. |
| Clipboard | `arboard`, with `wayland-data-control` | Copying text. Uses Wayland's clipboard directly where the compositor supports it, and X11's otherwise. |

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
| `lisp/library.rs`, `ui/prelude.sx` | The standard library, in Rust and in the language | JS's built-in objects |
| `lisp/budget.rs` | Limits on steps, nesting and sizes | Script timeouts |
| `loader.rs` | Locations, downloading, URL resolution, the origin rule, and loading pages into frames | Networking and navigation |
| `dom.rs` | `Element` / `Node` tree | DOM |
| `style.rs` | Tag defaults, then attributes, with inheritance | CSS cascade |
| `layout.rs` | Block flow, inline runs, and word wrapping, producing a display list and link boxes | Layout engine |
| `canvas.rs` | A canvas's shapes as polygons | SVG and `<canvas>` geometry |
| `paint.rs` | Rectangles, glyphs, images and polygons into a pixel buffer | Painting and compositing |
| `image.rs` | Decoding PNG files, and scaling images | Image decoders |
| `browser.rs` | A browsing session: navigation, history, state, scrolling, focus, editing | The browser process |
| `window.rs` | Turns window events into calls on a `Browser` | The browser's window |

## The language today

- Atoms: numbers, `"strings"`, `true`, `false`, `nil`, `symbols`, `:keywords`
- Special forms: `define`, `define-state`, `set!`, `fn`, `if`, `cond`, `let`, `and`, `or`, `do`,
  `quote` / `'`, `quasiquote` / `` ` `` with `,` and `,@`, and `eval`
- Functions take a rest parameter after `&`: `(fn (first & more) ...)`.
- Builtins, written in Rust:
  - Numbers: `+ - * / mod floor ceil round abs sqrt pow sin cos atan2 min max`
  - Comparisons: `= < > <= >= not`. `<` and friends compare two numbers or two strings.
  - Lists: `list first rest nth last cons append length empty? range repeat
    reverse take drop contains? index-of sort sort-by map filter reduce apply`
  - Strings: `str repr join split upper lower trim substring starts-with?
    ends-with? parse-number url-encode`. `length`, `empty?`, `take`, `drop`,
    `reverse`, `contains?` and `index-of` work on strings too.
  - Dictionaries: `(dict :key value ...)`, `get`, `assoc`, `dissoc`, `keys`, `values`.
    Keys are numbers, strings or keywords, kept in order. `assoc` and `dissoc`
    return a new dictionary rather than changing the old one.
  - Predicates: `list? string? number? symbol? keyword? fn? dict?`
  - Pages: `style dark-mode light-dark param go source frame`, and `tag`, `attr`
    and `children` for looking inside elements
- The prelude, `ui/prelude.sx`, is written in the language itself and
  runs before every page: `pi inc dec sum product concat without toggle any? all?
  map-indexed update plural`. A page can define its own versions instead.
- A builtin costs one evaluation step however much work it does, so
  `(sort (range 100000))` fits easily in a page's budget, where the same sort
  written in the page wouldn't.
- Block elements: `page div row h1 h2 h3 p pre frame`. Inline elements: `span b code a button input img spacer canvas`.
  Shapes, for inside a `canvas`: `rect circle line polyline polygon text`.
  `pre` keeps its spaces and line breaks, and `code` and `pre` use a monospaced font.
  `(spacer)` takes up whatever room is left on its line, pushing what follows to
  the end, like `margin-left: auto` in a flexbox; several share the room.
  All of them are ordinary functions:
  `(tag :attr value ... children ...)`. Lists are spliced in, attributes and
  all, and `nil` is skipped, so `if` and functions that return lists compose
  naturally. When an attribute is given twice, the later one wins.
- Style attributes: `:color`, `:background`, `:size`, `:bold`, `:mono`, `:underline`, `:padding`,
  `:padding-left`, `:margin`, `:max-width`.
  Margin applies only to block elements; padding on an inline element pads its
  left and right. `:max-width` narrows a block's content and centres it, while
  its background keeps the full width.
- Links: `(a :href "other.sx" ...)` opens a local page, resolved relative to the
  current page. An href starting with `/` is relative to the working directory.
  Go back with Alt+←, the mouse's back button, or the toolbar.

Scroll with a trackpad, mouse wheel, arrow keys, Page Up/Down, Space and
Shift+Space, or Home/End. This scrolls the frame under the cursor, if it has a
fixed height, and otherwise the page. Going back restores where you'd scrolled to.
Anything taller than the space it's shown in gets a scrollbar on its right
edge: drag the thumb, or press the track to jump to that point.

## Styles are values

There are no stylesheets or selectors. A style is a list of attributes,
which an element splices in like any other list:

```lisp
(define muted (style :color "#888" :size 14))   ; the list (:color "#888" :size 14)
(p muted "fine print")                          ; (p :color "#888" :size 14 "fine print")
```

- **Precedence is order.** Later attributes win, so `(p muted :color "red" ...)`
  is muted but red.
- **Combining styles is appending:** `(append card (style :background "#fff1d6"))`.
- **Components replace classes.** A function that passes its arguments through
  lets callers add children and override attributes:
  `(define (note & body) (p muted body))`, then `(note :color "red" "hi")`.
- **Variables and themes are just code.** Styles are computed when the page
  runs, so a palette can depend on `define-state` or on dark mode.

### Dark mode

Dark mode is a browser setting, toggled from the toolbar. It starts in the
system's theme when the system reports one, and follows it when it changes,
until the toolbar's switch is used. The window then remembers that choice
in `~/.config/netscheme/settings` (under `$XDG_CONFIG_HOME` if that's set),
starts in it, and stops following the system. Deleting the file goes back
to following the system. Screenshots and tests don't read or write it.
The browser's own colours (the page background, text, links, buttons and
inputs) switch with it, so a page that doesn't choose its colours goes dark
by itself. A page that does choose them can follow the setting:

- `(dark-mode)` is true in dark mode.
- `(light-dark "#f3eef7" "#2a2631")` picks the first value in light mode and
  the second in dark mode, like CSS's `light-dark()`. Any values work, such as
  whole lists of colours.

Changing the setting runs the page again. Only `browser:` pages can change it,
with `(set-dark-mode true)`.

`style` is `list` plus a check that its arguments are keyword/value pairs.

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

### Animation

```lisp
(define-state x 0)

(page
  (canvas :width 300 :height 40
          :on-frame (fn (dt) (set! x (mod (+ x (* 0.1 dt)) 300)))
    (circle :x x :y 20 :r 10)))
```

`:on-frame` takes a function of one argument, and works on any element.
While a page, or a page framed in it, has one, the browser calls it 60
times a second with the milliseconds since the last frame, then runs the
page again, as after a click. Moving things by their speed times `dt`
keeps them moving at the same speed when frames come late. After a long
gap, such as a slow frame, `dt` is at most 100. `:on-frame nil` is no
handler, so `(if running (fn (dt) ...))` pauses. Handlers run in the order
their elements appear, before the page runs again.

The window doesn't animate while it's hidden. A handler that fails stops
the animation, so the error stays on screen, until the page is loaded
again by following a link, going back or reloading. Each frame has the
same step budget as any other event.

### Text input

```lisp
(define-state name "")

(input :value name :placeholder "Your name" :width 240 :select-on-focus false
       :on-input (fn (text) (set! name text))
       :on-submit (fn (text) ...))
```

An input shows the `:value` the page gives it. Typing works out the new text
and calls `:on-input` with it; Enter calls `:on-submit` with the current text.
Without an `:on-input` handler, typing changes nothing, as in React's
controlled inputs. The browser itself only tracks which input has focus and
where the caret is.

Click an input to focus it. Arrow keys, Home/End, Backspace and Delete edit;
Tab moves to the next input; Escape, or clicking elsewhere, leaves it.

Selecting text: drag across it, double-click a word, triple-click for all of
it, hold Shift with the arrow keys or Home/End, or press Ctrl+A. Typing,
Backspace and Delete replace the selection. With `:select-on-focus true`,
a click that focuses the input selects all of its text, as the address bar
does; Tab and Ctrl+L select everything in the input they move to.

Three more attributes are for inputs that act like controls, as the find
bar's does: `:autofocus true` focuses the input when it first appears,
`:on-escape` is called with no arguments when Escape leaves it, and
`:on-shift-submit` is called instead of `:on-submit` for Shift+Enter.

### Images

`(img :src "logo.png" :width 48)` shows a PNG image, from a file or the web,
resolved relative to the page like a link. With only `:width` or `:height` it
keeps its proportions; with neither, it's its natural size. It sits on the
baseline like a letter, or with `:align "middle"` centred on the text beside
it, and inside an `a` it's a link. An image that can't be loaded keeps its
space as an empty box, and the reason is printed to stderr.

Images are decoded once and kept, as premultiplied RGBA, until a reload (F5).
The painter scales each to the size it's drawn at, averaging pixels to shrink
it or blending them to enlarge it, and keeps the scaled copies too. Files can
be up to 16 MB, and 4096 × 4096 pixels once decoded. Web pages can only load
images from the web. The site's pages show `assets/images/logo.png`. Images at `browser:` locations,
like the toolbar's `browser:icons/moon.png`, come from the browser's `ui` folder.

The window identifies itself as `netscheme`: that's its app ID on Wayland and
its class on X11, so window manager rules (Hyprland's `class:netscheme`) and
desktop entries can match it. On X11 and Windows the window also carries the
logo as its icon. Wayland compositors take icons from a desktop entry instead:
put the logo at `~/.local/share/icons/hicolor/256x256/apps/netscheme.png`, and
a `~/.local/share/applications/netscheme.desktop` with `Icon=netscheme`,
`StartupWMClass=netscheme`, `Exec=` the release binary, and `Path=` this folder,
so that pages and the logo are found when it's started from a launcher.

### Canvas

```lisp
(canvas :width 300 :height 100 :background "#fff"
  (rect :x 10 :y 10 :w 80 :h 80 :fill "#b8336a")
  (circle :x 150 :y 50 :r 40 :stroke "#5b3ab8" :stroke-width 4)
  (line :x1 200 :y1 90 :x2 290 :y2 10)
  (polygon :points '((200 10) (240 10) (220 40)) :fill "#2a7a3a")
  (text :x 200 :y 90 :size 14 "hello"))
```

A canvas is an inline box, 300 × 150 unless given a `:width` and `:height`,
that draws the shapes inside it, measured from its top-left corner and cut
off at its edges. Shapes are elements, so `map` can make them and lists of
them are spliced in.

| Shape | Attributes |
|---|---|
| `rect` | `:x :y :w :h` |
| `circle` | `:x :y :r`, the centre and radius |
| `line` | `:x1 :y1 :x2 :y2` |
| `polyline`, `polygon` | `:points`, a list of `(x y)` lists. A polygon joins the last point to the first and fills. |
| `text` | `:x :y`, with `:y` the baseline, and the usual text attributes |

`:fill` and `:stroke` take colours, and `:stroke-width` (1 by default)
is the outline's thickness, centred on the shape's edge. A shape given
neither is filled in the text colour; lines and polylines are only
stroked, in the text colour unless given a `:stroke`. Corners where a
stroked line bends are rounded. `:on-click` works on each shape, over the
box around it, and on the canvas; a shape drawn later is clicked first.
Other children of a canvas are left out.

Layout turns every shape into polygons, in `canvas.rs`: a circle has
enough sides to look round at its size, a stroke is a rectangle for each
segment and a circle at each joint, and an outline is a ring with a hole.
The painter fills them by the nonzero rule, sampling four lines across
each row of pixels and blending each pixel by how much of it is covered,
which smooths the edges.

### Selecting, copying and pasting

Text on pages can be selected as in inputs: drag across it, double-click a
word, or triple-click a paragraph. Ctrl+C copies the selection, or the
selection in the focused input if it has one. Ctrl+V pastes into the focused
input, replacing its selection; inputs hold one line, so line breaks and tabs
in the pasted text become spaces. Lines that wrapped from one
paragraph are copied joined by spaces, and separate paragraphs and lines of
`pre` text by line breaks. Clicking without dragging still follows links;
dragging across one selects its text instead. A selection stays inside the
frame it started in, so dragging from the site into the toolbar doesn't
select the toolbar too. Navigating clears it.

Layout numbers every character it lays out, in order, so a selection is a
range of those positions, highlighted as the page is laid out, like find's
matches. Text inside inputs isn't part of a page selection, and dragging
past the edge of the window doesn't scroll.

### Finding text

Ctrl+F opens a find bar under the toolbar. Matches in the site are
highlighted as you type, ignoring case, with the current one in orange and
scrolled into view. Enter or F3 or Ctrl+G goes to the next, and with Shift
the previous; Escape closes the bar. Matches are found line by line, so a
phrase that wraps onto the next line isn't found, and text in inputs isn't searched.

The find bar is part of `ui/chrome.sx`. It asks the browser to search with
`set-find`, `find-next`, `find-previous` and `close-find`, which only
`browser:` pages may call. The browser finds the matches as it lays the page
out, and passes the query, the number of matches and the current one back to
the chrome as `find`, `found` and `at` in its location.

### The REPL

Ctrl+Shift+J opens a REPL under the toolbar, and pressing it again closes
it, as Escape in its input does. Code typed into it runs in the site, like a
browser's JavaScript console: it can read the site's variables, call its
functions, and `set!` its state, after which the site runs again and shows the
change. The REPL shows the last eight entries, each with the value printed as
code or the error in red, and its log lasts until the browser closes.

The REPL runs code in the site's global environment from the site's latest
run. The site runs again after every click and keystroke, starting from a fresh
environment each time, so anything `define`d in the REPL is gone by the next
entry; `define-state` variables are what last. A runaway entry stops with an
error, as an event handler does.

The REPL is part of `ui/chrome.sx` too. It hands code to the browser with
`repl-eval`, closes with `close-repl`, and reads what has run with
`repl-log`, which only `browser:` pages may call.

### Screenshots of interactions

`--click X,Y`, `--drag X,Y,X,Y` and `--type TEXT` act on the page before
the screenshot, in order. In typed text, `\n` is Enter, `\t` is Tab and `\b` is Backspace.
`--frames N` runs N frames of animation, each a 60th of a second after the
last, so the same steps always draw the same picture.
`--chrome` includes the toolbar, as the window shows it, and `--dark` turns on dark mode:

```sh
cargo run -- pages/inputs.sx --screenshot out.png --click 250,332 --type $'Ada\tWrite docs\n'
cargo run -- pages/hello.sx --chrome --screenshot out.png --drag 200,330,300,420   # select text
cargo run -- pages/hello.sx --chrome --screenshot out.png --click 300,21 --type $'\b\b\b\b\b\b\b\binputs.sx\n'
cargo run -- pages/library.sx --chrome --screenshot out.png --find $'dict\n\n'   # Ctrl+F, type, Enter twice
cargo run -- pages/counter.sx --chrome --screenshot out.png --repl $'(set! count 41)\n'   # Ctrl+Shift+J, type, Enter
cargo run -- pages/canvas.sx --screenshot out.png --frames 120   # two seconds of animation
```

## Runaway code

Pages can't freeze or crash the browser. Each run of a page, and each event
handler, gets a budget; frames share the budget of the page they're on.

- **One million evaluation steps.** The pages here use at most about 20,000.
- **10,000 levels of nesting**, which is about 3,300 recursive calls.
  Evaluation moves to a fresh stack segment as it needs one (with the
  `stacker` crate), so deep recursion within the limit is fine.
- **A million characters or items** for strings and lists that builtins build,
  checked as they're built, so a string that doubles in a loop stops early.
- **512 levels of element nesting**, and source nested at most 1,000 deep.

Going over a limit stops the run with an error. A page that fails while it
loads shows an error page in its frame; a failed event handler is reported in
a banner under the toolbar, and the page stays usable. See `pages/runaway.sx`.

## Code as data

Elements print as the code that builds them, so `(repr (p :color "red" "hi"))`
gives `"(p :color \"red\" \"hi\")"`, and reading that back builds the same element.

- `(source "page.sx")` returns another page's code as a list of forms without
  running it. Comments aren't kept.
- `(go "page.sx")`, called from an event handler, navigates as if a link to that
  location were clicked, once the handler returns.
- `(param "name")` reads a query parameter from the page's location, as in
  `view-source.sx?page=hello.sx`.
- `(frame :src "page.sx" :scale 0.5)` embeds another page, which runs in its own
  environment and is drawn at the given scale. A page may frame itself: frames
  more than 8 deep, or drawn at under 4% of full size, are left empty.
  With `:height` (pixels, or `"fill"` to reach the bottom of the window) the
  frame clips its page and scrolls it independently. `:border false` hides the border.
- `(row ...)` lays its children out side by side in equal columns.

`pages/fractal.sx` frames itself twice.

There's no import statement. The pages in `pages/` share their theme and
components by reading `site.sx` as data and running it in place:

```lisp
(eval `(do ,@(source "site.sx")))
```

`site.sx` also has the guide's code printer. Examples in the guide are quoted
forms, which a page both prints, with line breaks and colours, and evaluates,
so an example always shows what its code makes.

## The web

Pages can be loaded over `http://` and `https://`, from any server that serves
`.sx` files. To try it, run `python3 -m http.server` in `pages/` and open
`http://localhost:8000/hello.sx`.

- **Links resolve as URLs** on web pages, against the address the page was
  actually served from, after any redirects.
- **Web pages can only reach web pages.** They can't frame, `source`, link to,
  or `go` to local files or `browser:` pages. Local pages can load web pages.
- **Each page is downloaded once per visit.** Pages run again on every click
  and keystroke, and reuse the download. Navigating, or reloading with F5 or
  Ctrl+R, downloads afresh; reloading also starts the page's state over.
- **Downloading blocks the window**, with a 5 second limit on connecting and
  10 seconds overall. Pages over 4 MB aren't downloaded.
- **Failures show in the frame:** a missing page, an unreachable server, or a
  response that's HTML rather than this browser's Lisp.

## The browser is a page

Locations starting with `browser:` are the browser's own pages, kept in `ui/`:

- `browser:chrome?location=<site>` is the toolbar. The window always shows this
  page, which puts the site in a `(frame :height "fill")` below the toolbar. It
  reads the site's title out of the frame's document tree, and that becomes the
  window title. Its address bar is an `input` whose `:on-submit` calls `go`.
  Edit `ui/chrome.sx` to change the browser's interface.
- `browser:view-source?page=<site>` shows a page's source, and can show its own.
- `browser:back` isn't a page: following a link to it goes back.

Links on browser pages are relative to the working directory, and any link,
whether in the toolbar or the site, navigates the site. Clicking the address
bar, or pressing Ctrl+L, selects the address, so typing replaces it. Going back restores the site's state, but not the
toolbar's, so the address bar shows where you are. Values inside a query,
like the site's own location, are percent-encoded.

The chrome can frame itself, too:
`cargo run -- 'browser:chrome?location=pages/hello.sx'` opens a browser inside a browser.

## Roadmap

1. **Loading in the background**, so a slow server doesn't freeze the window,
   with a loading indicator in the toolbar. Forward history too.
2. More of the language: macros, tail calls.
