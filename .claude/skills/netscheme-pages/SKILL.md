---
name: netscheme-pages
description: Write web pages for the Netscheme browser. Netscheme pages are `.sx` files in a small Lisp that covers structure, style and behaviour in place of HTML, CSS and JavaScript. Use when creating or editing any `.sx` page, site, component or demo, or when asked to make a web page, site or app "for Netscheme" or "in Netscheme Lisp".
---

# Writing Netscheme pages

A Netscheme page is a Lisp program in a `.sx` file. The browser runs the file
top to bottom, and the value of the **last expression** is the document,
normally a `(page ...)` element. A click or keypress runs a handler, which
changes some state, and the browser then runs the whole file again.

## Setup

Call the folder Netscheme was cloned into `$NETSCHEME` below. Inside the
repository, that's the current folder. The browser's binary is named
`netscheme`, and `cargo install --path "$NETSCHEME"` puts it on the PATH.
Otherwise, run it through cargo, which works from any directory:

```sh
cargo run --release -q --manifest-path "$NETSCHEME/Cargo.toml" -- page.sx
```

Outside the repository, if `netscheme` isn't on the PATH and you don't know
where the clone is, ask.

The browser finds its own `ui/` folder through the clone's path, compiled into
it, so pages can live anywhere, but the clone has to stay where it was built.
Relative paths in pages (`:src`, `:href`, `source`) resolve against the page's
own location.

The browser's built-in guide in `$NETSCHEME/pages/` is the authoritative
reference, and every chapter is a working example. When a detail here isn't
enough, read the relevant chapter:

| Topic | Chapter |
|---|---|
| Elements, components, images, canvas, links | `tour.sx` |
| Style attributes, `style`, dark mode | `styles.sx` |
| State, events, inputs, animation, keys | `state.sx` |
| Values, special forms, errors, limits | `language.sx` |
| Every builtin, and the prelude (`$NETSCHEME/ui/prelude.sx`) | `library.sx` |
| `source`, quasiquote, `frame` | `code-as-data.sx` |
| Screenshots and CLI flags | `using.sx` |
| Larger examples | `snake.sx`, `canvas.sx`, `lab.sx`, `inputs.sx` |

## A minimal page

```lisp
; Definitions first, then the page.
(define who "world")

(page :title "Greetings"
  (h1 "Hello, " who "!")
  (p "Six times seven is " (* 6 7) "."))
```

## The language is not Scheme

It looks like Scheme but is its own dialect. Write these, not the Scheme or
Clojure spellings:

| Write | Not |
|---|---|
| `(fn (x) ...)` | `lambda` |
| `(do a b)` | `begin` |
| `true`, `false`, `nil` | `#t`, `#f`, `'()` |
| `(fn (a & rest) ...)` | `(a . rest)` |
| `(if test then)` with no else, which gives `nil` | `when`, `unless` |
| `(cond (test body...) (else body...))` | |
| `(let ((a 1) (b (+ a 1))) ...)` | `let*` (`let` is already sequential) |
| `(str "a" 1)` | `string-append`, `format` |
| `(get d :k)`, `(assoc d :k v)` | `hash-ref`, `assq` |

The special forms are only `quote`, `quasiquote`, `if`, `do`, `and`, `or`,
`cond`, `let`, `define`, `define-state`, `set!`, `fn` and `eval`. There are no
macros and no loops: iterate with `map`, `filter`, `reduce`, `range` and
`repeat`, or recurse. A **tail call**, one that's the last thing a function
does, doesn't nest, so a function can loop by calling itself last:

```lisp
(define (count-up n total) (if (= n 0) total (count-up (- n 1) (+ total n))))
```

Tail position is the last form of a function body, either branch of `if`, the
last form of `do`, `let` or a `cond` clause, and the last of `and` or `or`. A
call whose result is still used, like `(+ 1 (f n))`, isn't one, and that kind
of recursion is limited to about 3,300 calls deep. Calls made inside builtins
like `map`, `reduce` and `apply` aren't tail calls either.

Values: one number type (a float), strings with `\" \\ \n \t` escapes,
symbols, `:keywords` that evaluate to themselves, lists, `(dict :a 1)`
dictionaries, functions and elements. Only `false` and `nil` are false; `0`
and `""` are true. `nil` is also the empty list.

Builtins: `+ - * / mod floor ceil round abs sqrt pow sin cos atan2 min max
= < > <= >= not list first rest last nth cons append length empty? range
repeat reverse take drop contains? index-of map filter reduce sort sort-by
apply str repr join split upper lower trim substring starts-with? ends-with?
parse-number url-encode dict get assoc dissoc keys values list? string?
number? symbol? keyword? fn? dict? style dark-mode light-dark param go source
tag attr children`. The prelude adds `pi inc dec sum product concat without
toggle any? all? interpose chunk map-indexed update plural`. Nothing else
exists, so define anything missing yourself.

## Elements

Every element is a function. Its arguments are `:keyword value` attribute
pairs and children, mixed in any order.

- **Blocks:** `page` (takes `:title`), `div`, `row` (children side by side; see
  [Rows](#rows)), `h1`, `h2`, `h3`, `p`, `pre`, `frame`
- **Inline:** `span`, `b`, `code`, `a`, `button`, `input`, `img`, `canvas`,
  `spacer` (fills the rest of the line, pushing what follows to the right)
- **Over the page:** `overlay`, for menus, tooltips, dialogs and notices (see
  [Overlays](#overlays))
- **Shapes, inside a `canvas` only:** `rect`, `circle`, `line`, `polyline`,
  `polygon`, `text`

There are no other tags: no `ul`, `li`, `table`, `form`, `strong` or `h4`.
Make lists from `p` or `div` with `"• "`, and tables from `row`.

Children can be:

- strings and numbers, which become text
- `nil`, which is skipped, so `(if cond (p ...))` can leave something out
- lists, which are spliced in, so `(map (fn (x) (p x)) xs)` works directly

Text isn't spaced for you. Adjacent string children run together, so include
the spaces yourself: `(p "Hello, " (b name) "!")`.

## Styles

Every element styles itself with attributes. There are no stylesheets, no
selectors and no CSS.

| Attribute | Meaning | Inherited |
|---|---|---|
| `:color "#5b3ab8"` | Text colour | yes |
| `:size 20` | Font size in px (default 16) | yes |
| `:bold true`, `:mono true`, `:underline true` | Text face | yes |
| `:background "#f3eef7"` | Fills the box | no |
| `:padding 12` | Inside space on every side; only left and right on inline elements | no |
| `:padding-left 24` | Overrides the left padding | no |
| `:margin 16` | Space **below** a block | no |
| `:margin-top 8` | Space above a block (there's no left or right margin) | no |
| `:max-width 720` | Caps the content width and centres it, usually on `page` | no |
| `:width 240`, `:width "fit"` | A block's width in px, or as wide as its content; it sits at the left unless `:align "center"` or `"right"` | no |
| `:radius 8` | Rounds the corners of a background, border or `img`, capped at half the shorter side (`999` gives a circle or pill) | no |
| `:border "#ccc"` | A line just inside the box's edge, 1px unless `:border-width` says, following `:radius` | no |

Colours are `#rgb`, `#rrggbb`, `#rgba`, `#rrggbbaa`, or
`black white red green blue grey gray`. Numbers are plain numbers, never
strings with units like `"12px"`. There is no flex, grid or CSS positioning.
For layout, use `row`, `spacer`, `padding`, `margin`, `:width` and
`max-width`, and for things drawn over the page, `overlay`. A border goes all
the way round, so to draw a coloured edge on one side only, nest boxes:
`(div :background accent :padding-left 3 (div :background panel :padding 14 ...))`.

### Rows

A `row` puts its children side by side, `:gap` px apart (16 by default):

- A child with a numeric `:width` keeps that width.
- Inline children, and blocks with `:width "fit"`, are as wide as their content.
- Every other child, and every `spacer`, shares the room left equally.
- `:align "middle"` or `"bottom"` lines the columns up other than at the top.

```lisp
(row :gap 12 :align "middle"
  (img :src "logo.png" :width 48)          ; inline: as wide as it is
  (div (b "Netscheme") (p :margin 0 "A browser for a Lisp"))   ; takes the rest
  (div :width "fit" (button "Open")))      ; as wide as its button
```

Defaults: `page` has padding 24, `p` margin 12, `h1`/`h2`/`h3` are 32/24/19 px
and bold, `button` has padding 8.

`(style :attr v ...)` returns attributes as a list, and a list in an element is
spliced in, so a style is a value you can name and reuse:

```lisp
(define callout (style :background (light-dark "#fff1d6" "#3d2f1c") :padding 12 :margin 8))
(div callout (p :margin 0 "Inside a callout."))
(div (append callout (style :background "#e3f4e6")) ...)   ; later attribute wins
```

When an attribute appears twice, the later one wins, so a component that
passes `& body` through lets callers override its defaults:

```lisp
(define (card title & body)
  (div :background (light-dark "#f3eef7" "#2a2631") :padding 14 :margin 10
    (b :size 18 title)
    body))

(card "Override" :background "#fff1d6" (p :margin 0 "Custom background."))
```

**Dark mode:** a page that picks no colours follows the browser's mode
automatically. If you pick any colour, give it both modes with
`(light-dark light dark)`, which works on any value, not just colours.
`(dark-mode)` returns true in dark mode. Define a palette once at the top:

```lisp
(define ink    (light-dark "#1f1c24" "#ebe7f0"))
(define accent (light-dark "#5b3ab8" "#c3a6ff"))
(define panel  (light-dark "#f4f1f8" "#25222b"))
(define edge   (light-dark "#d8d2e0" "#3a3542"))
```

**Hover:** `:hover` takes a style that a **block** has while the pointer is
over it. Give a block `:group true` and its descendants, inline ones
included, get their `:group-hover` styles while it's hovered, so a card can
change its title along with its border:

```lisp
(div :group true :border edge :radius 8 :padding 12
     :hover (style :border accent)
  (p :margin 0 :bold true :group-hover (style :color accent) "Point at this card")
  (p :margin 0 "Its border and its title change colour."))
```

Inline elements like `a` and `span` don't take `:hover` themselves; put them
in a `:group` and use `:group-hover`.

## State and events

```lisp
(define-state count 0)          ; the initial value is used on the first run only

(page :title (str "Count: " count)
  (p :size 40 :bold true count)
  (button :on-click (fn () (set! count (inc count))) "Add one"))
```

- Put every `define-state` at **top level**, in the same order on every run.
  State is identified by position, so never put it inside an `if`, a function
  or a `map`.
- Treat state as immutable values: `(set! items (append items (list x)))`,
  `(set! d (assoc d :k v))`, `(set! xs (without x xs))`.
- `:on-click (fn () ...)` works on any element, not only `button`.
- **Inputs are controlled:**
  `(input :value draft :placeholder "..." :width 320 :on-input (fn (text) (set! draft text)) :on-submit (fn (text) ...))`.
  Without `:on-input`, typing changes nothing. There are also `:autofocus`,
  `:select-on-focus`, `:on-escape` and `:on-shift-submit`.
- `(go "other.sx")` navigates after the handler finishes, and
  `(param "name")` reads `?name=value` from the page's location.

**Animation:** `:on-frame (fn (dt) ...)` runs about 60 times a second with the
milliseconds since the last frame (at most 100), so multiply speeds by `dt`.
`:on-frame nil` means no handler, so `:on-frame (if running (fn (dt) ...))`
pauses. When a frame only changes state that a canvas's `:draw` reads, the
browser re-calls `:draw` instead of rerunning the page, so put fast-changing
visuals in `:draw`.

**Keys:** a `canvas` with `:on-key-down` / `:on-key-up` takes the keyboard
when clicked. Handlers get names like `"a"`, `" "`, `"ArrowLeft"` and
`"Enter"`.

## Overlays

An `overlay` is drawn over the page instead of taking up room in it. Write it
inside the block it's anchored to; usually it's shown by state:

```lisp
(define-state open false)

(div :width "fit"
  (button :on-click (fn () (set! open (not open))) "Menu")
  (if open
      (overlay :at "below" :margin-top 4 :padding 8 :radius 6
               :background panel :border edge
               :on-dismiss (fn () (set! open false))
        (p :margin 2 (a :href "a.sx" "First"))
        (p :margin 2 (a :href "b.sx" "Second")))))
```

- `:at "below"` (the default), `"above"`, `"left"` or `"right"` places it next
  to the block it's in, moving to the other side if there's more room there.
  It anchors to the nearest enclosing **block**, not an inline element, so
  wrap a button in `(div :width "fit" ...)` to anchor to the button.
- `:at "center"`, `"top"`, `"bottom"`, `"top-left"`, `"top-right"`,
  `"bottom-left"` or `"bottom-right"` fixes it in the window, where it stays
  as the page scrolls; `:margin` keeps it off the window's edges.
- It's as wide as its content unless it has a `:width`.
- `:backdrop "#0006"` covers the page beneath it, as for a modal dialog.
- Clicks on an overlay go to it, not to what's under it. `:on-dismiss` is
  called on a click outside it, or on Escape, and that click does nothing
  else. Set the state that shows it to false there.
- Overlays are drawn after the rest of their page, later ones over earlier
  ones, and one in a `frame` stays inside the frame. An overlay hanging off
  the bottom of the page doesn't make the page taller.

## Canvas

```lisp
(canvas :width 300 :height 80 :background panel
  (rect :x 10 :y 10 :w 60 :h 60 :fill "#b8336a")
  (circle :x 120 :y 40 :r 30 :stroke "#5b3ab8" :stroke-width 4)
  (line :x1 170 :y1 70 :x2 220 :y2 10 :stroke-width 3)
  (polygon :points '((240 70) (290 70) (265 20)))
  (text :x 10 :y 78 :size 12 "label"))
```

Coordinates are measured from the top left. Without `:fill` or `:stroke`,
shapes take the text colour, so they follow dark mode. Shapes accept
`:on-click`. The default size is 300 × 150. `:draw (fn () shapes...)` adds
shapes after the children.

## Images, links and frames

- `(img :src "pic.png" :width 128 :alt "...")` loads PNGs only, relative to
  the page. Give `:width` or `:height` to keep the proportions, and
  `:align "middle"` to centre it on the line. `:width :fill` spans the
  containing block's content width (like CSS `width: 100%`) and rescales on
  resize; add `:height` to stretch it instead. Pages served over http can only
  load images over http.
- `(a :href "other.sx" "text")` resolves relative to the page, and `http(s)://`
  works too. On file pages, a leading `/` means the folder the browser was
  started in.
- `(frame :src "page.sx" :scale 0.5 :height 300 :border false)` embeds another
  page with its own state.

## Multi-page sites

There is no import. Put shared definitions (palette, styles, components, a
layout function) in a file like `site.sx` that isn't a page itself, and
start each page with:

```lisp
(eval `(do ,@(source "site.sx")))
```

`source` reads the file's forms without running them, and `eval` runs them
where that line is. A layout component keeps pages consistent:

```lisp
; site.sx
(define (site-page title & body)
  (page :title title :max-width 720
    (p (a :href "index.sx" "Home") "  ·  " (a :href "about.sx" "About"))
    (h1 title)
    body))
```

To serve a site over http, run `python3 -m http.server` in its folder and open
`http://localhost:8000/index.sx`.

If you're editing the Netscheme guide itself (`$NETSCHEME/pages/`), use the
components its `site.sx` defines (`demo`, `chapter`, `section`, `entry`,
`example`, ...). Register new pages in its `demos` or `chapters` list.

## Limits

A page or handler gets a million evaluation steps, 10,000 levels of nesting
(which tail calls don't add to), a million items in any string or list, and 512 levels of element nesting. A
builtin call costs one step, so prefer `sort`, `map` and `reduce` to
hand-written recursion. An error while loading replaces the page with the
message. An error in a handler shows a banner and keeps the page.

## Checking your page

Always render the page and look at it before calling it done. Write the PNGs
to a scratch or temp directory:

```sh
netscheme page.sx --screenshot out.png             # light
netscheme page.sx --screenshot out-dark.png --dark
```

Use the `cargo run ... --` form from Setup if `netscheme` isn't on the PATH.
Then read the PNG. Use `--width`, `--height` (900 × 700 by default) and
`--scroll N` for long pages. To exercise interaction, run actions before the
screenshot, in order: `--click X,Y`, `--type TEXT` (`\n` is Enter),
`--key-down KEY`, `--frames N`. Coordinates are in screenshot pixels, so take
a plain screenshot first to find them. A `--click` that hits nothing fails
with `nothing to click at X,Y`. Errors print in the rendered page, and
warnings like `unknown color` print to the terminal, so read both.
