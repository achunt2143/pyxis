# Pyxis identity

What Pyxis looks, moves and sounds like. This is the one place a designer
should start. It states the decisions and gives the real values, each with the
file it comes from, so the document can be kept in sync with the code.

Written 2026-11-16 from the code as built. Where this document and the code
disagree, the code is right and this file is stale; fix the file. Where a
plan document disagrees with the code (the visual identity plan quotes an
80% / 75% material mix, the code mixes 91% / 88%), this file follows the code.

Companion: `/home/andrew/Development/remochi/IDENTITY.md` (the UI library the
apps are built from, and how it sits inside this identity).

Source key used in the tables:

| Tag | Path |
|---|---|
| `identity.rs` | `geckos/evo/src/shell/identity.rs` |
| `identity.ts` | `pyxis-experience/packages/app-sdk/src/identity.ts` |
| `identity.css` | `pyxis-experience/packages/app-sdk/src/identity.css` |
| `spec.rs` | `geckos/evo/src/anim/spec.rs` |
| `depth.rs` | `geckos/evo/src/anim/depth.rs` |
| `busy.rs` | `geckos/evo/src/shell/busy.rs` |
| `palettes.ts` | `pyxis-experience/packages/app-sdk/src/wallpaper/palettes.ts` |
| `motion.ts` | `pyxis-experience/src/motion.ts` |
| glass plan | `geckos/docs/internal/pyxis-glass-and-lock-plan.md` |
| CLAUDE.md | `geckos/CLAUDE.md` |

---

## 1. The idea

Pyxis is a desktop that behaves like light. Windows do not slide and pop; they
come into focus out of a soft blur, tinted by their app's colour, and leave
the way light leaves a room. The wallpaper is the sky, and it follows the real
day: pale blue-greys at noon, gold in the evening, deep indigo at night. The
shell takes its colour from that sky, so the bar, the panels and the title
bars always look like they belong to the wallpaper behind them.

The shape language is the **waterdrop**: every corner round except one that is
nearly sharp. The motion language is **Lumen**: focus, shape and light, as if
the windows were seen through a lens. The three coloured window controls are
the shapes of the old remochi spinner, and they become that spinner when an
app stops responding.

Pyxis comes from webOS and Firefox OS, not from macOS or Windows. From webOS
it keeps cards for multitasking, "Just type", and notifications that gather at
the bottom edge instead of a bell in the corner. From Firefox OS it keeps the
idea that the whole shell is web technology. It is not a copy of any current
desktop, and early versions that looked like macOS (a floating white dock,
frosted white glass, a uniform 16 px corner, traffic lights) were deliberately
taken apart. See "Why" in `docs/internal/pyxis-visual-identity-plan.md`.

## 2. Principles

1. **Light, not chrome.** Surfaces are tinted by what is behind them and
   by the app's accent. We add light (glow, a seam, a glint) before we add
   borders, outlines or shadows.
2. **One shape.** The waterdrop is the only corner. Anything that floats and
   has a corner uses it. A sharp top-right corner is Pyxis's signature;
   nothing else on a desktop has one.
3. **Follow the day.** Colour comes from the wallpaper's sun, not from a fixed
   brand colour. The identity changes through the day and is still recognisably
   the same.
4. **Shapes carry meaning, colour is a bonus.** The window controls are
   distinguishable with no colour at all (triangle, square, circle). Inactive
   windows show them as outlines.
5. **Motion answers a question.** Where did the window go? What just opened?
   If a motion does not say where something came from or went to, it is not in
   Lumen. Every motion has a Reduced and an Off form.
6. **Say it plainly, from the user's side.** Copy tells people what will
   happen and who is asking. It does not apologise, hedge or sell.

---

## 3. The waterdrop

The corner radii, in px, in CSS order (top-left, top-right, bottom-right,
bottom-left):

| | tl | tr | br | bl |
|---|---|---|---|---|
| Waterdrop | 16 | **4** | 16 | 16 |

Source: `identity.rs` `CORNER_RADII`; `identity.ts` `CORNER`; `identity.css`
`--px-corner`. A contract test (`identityContract.test.ts`) fails if the Rust
and TypeScript copies drift apart.

Rules:

- **Square when a window fills the screen.** Maximized and fullscreen frames
  are square on all four corners (`Radii::frame(square)` in `identity.rs`).
- **Small things get a smaller drop.** `waterdrop(size)` in `identity.ts`
  scales the radii: a 40 px icon is `12 3 12 12`, a 20 px icon `7 2 7 7`, and
  from about 56 px up the full `16 4 16 16`. Below 20 px the radii shrink in
  proportion.
- **The sharp corner is on the top right**, where the window's close control
  sits and where the Evaporate close effect starts.
- **No corner is bigger than half the short side** of its box (a circle or a
  pill at most).

Where it is used:

| Surface | Drawn by |
|---|---|
| App window frame, title bar, border, drop shadow, open clip | Evo (`identity.rs` `Radii`, `ssd.rs`, `depth.rs`) |
| Client content, bottom corners clipped to 16 | Evo (`depth.rs` `CONTENT_RADIUS`) |
| App icons on the launch bar and in the drawer (40 px) | Bloom, `waterdrop(40)` |
| Panels: quick settings, notification centre, cards, drawer | Bloom |
| Toasts and dashboard chips | Bloom |
| Dialogs | Bloom's `ShellDialog` around remochi's dialog card, which uses the same `16 4 16 16` |

Known gap: the apps still carry about a dozen hand-drawn waterdrops with
slightly different numbers (`16 3 16 16`, `14 4 14 14`, `24 4 24 24`, ...).
`docs/internal/pyxis-identity-upstream.md` item P4 is the sweep that moves
them to `waterdrop(size)` or `--px-corner*`.

---

## 4. Colour

Pyxis has four colour systems. They do different jobs; do not mix them up.

1. The **wallpaper** (Daylight) is the sky. It is the only source of ambient colour.
2. The **spinner colours** are the window controls and the busy mark. Fixed.
3. The **app accent** is each app's own colour. Fixed per app.
4. The **shell tint** is derived from the wallpaper and gives every shell
   surface its fill and its ink.

### 4.1 Daylight, the wallpaper

Five colour masses on slow two-sine paths, blended in OKLab with a static
grain against banding. The palette is keyframed on the sun's elevation,
computed from the user's location (or a fixed 06:30 / 18:30 day when there is
none). Source: `palettes.ts` `PHASE_HEX`, `MORNING_KEYS`, `EVENING_KEYS`.

Each row is five colours: broad sky, second sky tone, warm accent, a glow low
in the frame, deep anchor.

| Phase | Sun elevation (rising) | Colours |
|---|---|---|
| Night | -18 deg | `#0b1026` `#1b2350` `#2a1f4d` `#0f3b4a` `#141a3a` |
| Blue hour | -8 deg | `#1d2b5c` `#3a4d8f` `#6b5ea8` `#2c6e8f` `#24366e` |
| Dawn | 0 deg (morning) | `#ff9a8b` `#ffc3a0` `#a18cd1` `#fbc2eb` `#6c7fd8` |
| Dusk | 0 deg (evening) | `#ee9ca7` `#8e54e9` `#4776e6` `#ffc6cf` `#3a2f7a` |
| Golden morning | 9 deg | `#ffd29d` `#8fd3f4` `#9ef0c0` `#fff1c1` `#7fb8f0` |
| Golden evening | 9 deg | `#f6c365` `#fd9a6e` `#e77fd8` `#ffd9a0` `#9a6cf0` |
| Day | 25 deg and above | `#4facfe` `#38e0f5` `#a6f5d6` `#dff4ff` `#5b8cff` |

Morning and evening are blended by the hour angle, so dawn leans toward
`dawn` and the evening toward `dusk`.

Fixed families on the same engine (`palettes.ts` `STATIC_HEX`):

| Family | Colours |
|---|---|
| Aurora | `#667eea` `#764ba2` `#8f6fe0` `#4fb3d9` `#3f3f9e` |
| Sunset | `#ff7e5f` `#feb47b` `#f0607a` `#ffd7a1` `#c2546e` |
| Forest | `#134e5e` `#71b280` `#2f7a6a` `#b5dca0` `#0d3640` |
| Mochi | `#4a8bc2` `#6aaedb` `#ffd479` `#bfe0f5` `#35689a` |

The wallpaper draws at 10 fps (motion is a few px a second, so it reads as
smooth) and stops drawing while a maximized app covers it. Details in
CLAUDE.md, "Daylight wallpaper and live settings".

### 4.2 The spinner colours

Sampled from the remochi spinner GIF: a red circle, a blue square, a yellow
triangle, each with a thin dark outline. They are the window controls.
Source: `identity.rs` `SPINNER_*`; `identity.ts` `SPINNER`; `identity.css`
`--px-spinner-*`.

| Shape | Control | Fill | Outline |
|---|---|---|---|
| Circle | Close | `#ff4a4a` | `#a12c2c` |
| Square | Maximize | `#69cdff` | `#33647c` |
| Triangle (pointing down) | Minimize | `#ffb80d` | `#5d4305` |

Yellow and blue are also remochi's own accents (`#ffb80d` is `--mochi-selected`;
`#69cdff` is `--mochi-accent`), which is why the spinner is a fair mark for
both projects.

### 4.3 App accents

Each built-in app has one accent. It colours the app's waterdrop icon, the
1 px seam at the bottom of its title bar, the running line on the launch bar,
the Evaporate glow, the Droplet orb, the Bloom glow and the dock ripple.
Source: `identity.ts` `BUILTIN_APP_ACCENTS` (mirrors each `manifest.webapp`
and Bloom's `APP_ACCENTS`).

| App | Accent |
|---|---|
| Terminal | `#34d399` |
| Browser | `#0284c7` |
| Files | `#d97706` |
| Notes | `#eab308` |
| Contacts | `#0d9488` |
| Tasks | `#7c3aed` |
| Settings | `#64748b` |
| Installer | `#ea580c` |

An app with no accent gets a neutral cool white for the open glow
(`bloom::NEUTRAL_ACCENT`) or an accent averaged from its picture and pushed
to saturation 0.55 or more (`lumen::accent_from_average`). Never a grey orb.

An accent is for light, not for text. Notes' `#eab308` on white fails text
contrast, so `appAccentTokens` deliberately does not map the accent onto
`--mochi-primary`.

Known gap: the accent exists only in Bloom's registry and the SDK table. It is
not yet in `manifest.webapp`, `appinfo.json` or Nova, so an installed `.ipk`
app has none (`pyxis-identity-upstream.md` P2).

### 4.4 The shell tint

`shellTint(palette, theme)` in `identity.ts` computes every shell colour from
the wallpaper's current palette. It is pure and runs separately in the desktop
and in the overlay. `useShellTint` writes the results onto `<html>` as
`--px-*` variables. There are three families. Choosing the right one is the
most common mistake, so:

| Family | Variables | Follows | Use it for |
|---|---|---|---|
| **Ink** | `--px-ink`, `--px-ink-quiet`, `--px-ink-shadow` | The **wallpaper** (not the theme): dark ink by day, light ink at night | Text and glyphs drawn **straight on the wallpaper**: the status strip, the clock, the search underline |
| **Material** | `--px-material`, `--px-material-strong`, `--px-material-ink`, `--px-material-ink-quiet` | The **theme** (light or dark) | **Panels**: quick settings, notification centre, drawer, cards, search results, dialogs |
| **Edge material** | `--px-edge-material`, `--px-edge-material-strong`, `--px-edge-ink`, `--px-edge-ink-quiet` | The **wallpaper**, like ink | The **launch bar** and what sits on it: Apps and Cards tiles, dashboard chips |

Plus `--px-glow` (the palette's most colourful mass, held between OKLab L 0.72
and 0.9) and `--px-wallpaper-mean`.

Why edge material is its own family: with the image's light theme the launch
bar was a pale band across a night wallpaper. Edge material follows the
wallpaper so the bar is dark when the wallpaper is dark.

The numbers (`identity.ts` `TINT`, `LIGHT_BASE`, `DARK_BASE`):

| Rule | Value |
|---|---|
| Light base | `#f7f6f2` (warm alabaster) |
| Dark base | `#14161f` |
| Light material | wallpaper mean mixed **91%** toward the light base, in OKLab |
| Dark material | wallpaper mean mixed **88%** toward the dark base |
| Material alpha | 0.82; strong 0.94 |
| Ink | near-black or near-white carrying a little of the background's hue, pushed toward pure black or white until it reaches contrast >= **4.5:1** |
| Quiet ink alpha | 0.72 (ink), 0.68 (material and edge ink) |
| Edge material | the dark mix if the ink is light (dark wallpaper), else the light mix |

No blur is used behind shell surfaces. The overlay is transparent, so a blur
would blur nothing; the shell fakes nothing.

Accessibility overrides in `identity.css`: `data-high-contrast` sets pure
black or white ink and solid panels; `data-reduce-transparency` replaces the
materials with solid colours (`#eef7fc` / `#ffffff` light, `#202228` / `#282a30`
dark).

### 4.5 Frosted glass (title bars)

The one place Pyxis has real glass, because Evo draws every window and can see
what is behind a title bar. Source: glass plan "The look"; code in
`geckos/evo/src/shell/frost.rs`.

| Part | Light theme | Dark theme |
|---|---|---|
| Backdrop | Bloom's wallpaper, shrunk to 1/8 and blurred | same |
| Veil | mixed about **52%** toward `#fcfbf8` in OKLab | about **50%** toward `#101228` |
| Lightness band | OKLab L at **least 0.86** | L at **most 0.30** |
| Colour | chroma x1.25, capped at **0.06** | x1.25, cap **0.07** |
| Grain | static, 9% overlay | same |
| Rim | 1 px white along the top, 75% | 14% |
| Seam | 1 px of the app accent along the bottom, 60% | same |
| Inactive | chroma x0.35, veil +20%, seam 22% | same |

The lightness band is what keeps the ink fixed and safe, so Evo never chooses
ink per frame:

| Theme | Title ink | Contrast |
|---|---|---|
| Light | `#2a2620` | at least 7:1 over any backdrop |
| Dark | `#ece9e3` | at least 7:1 over any backdrop |

A flat fallback (tier F0, one veiled colour from Bloom's `material`) is used
when frost is off, has no GL, or a bar has not yet been drawn once. Frost of
other windows under the bar (F2) is not built: over another window a bar shows
the wallpaper.

---

## 5. Type

Pyxis speaks in **Mochi**, the face family remochi inherits from webOS's
Prelude. Prelude cannot be redistributed, so each Mochi face tries an
installed Prelude first (present on webOS devices) and falls back to the
bundled open-source **Lato** (SIL OFL). Source: `identity.ts`
`IDENTITY_FONTS`, `IDENTITY_FONT_WEIGHTS`, `DISPLAY_TRACKING`; `identity.css`.

| Role | Face | Weight / style | Used for | CSS class |
|---|---|---|---|---|
| Display | Mochi Light, then `Mochi Lato`, `Lato` | 400, uppercase, tracking **0.14em** | The clock, panel heads | `.px-display` |
| UI | Mochi Medium, then `Mochi Lato`, `Lato` | 400 | Everything else | `.px-ui` |
| Accent | Mochi Bold Oblique, then `Mochi Lato`, `Lato` | 700, italic | The one emphasised word | `.px-accent` |

(Mochi Light is registered at weight 400 in remochi's CSS for the shell's use;
remochi's own headers use it at `normal` with no tracking by default.)

How it reads:

- **Display heads are uppercase, light and widely tracked**: `CARDS`,
  `APPLICATIONS`, `QUICK SETTINGS`. They are labels, not headlines. Keep them
  to one or two words.
- **The clock is Mochi Light**, centred in the status strip.
- **Use bold oblique once per view**, on the single word that matters. Not on
  every label; if everything leans, nothing does.
- **Window titles** in Evo's bar are the app's name in bold, then the
  client's own title in quiet ink, only when it adds anything.

---

## 6. Controls

The window controls are the spinner's three shapes, not traffic lights.
Source: `identity.rs` `SPINNER_*`, plan "The window controls".

| Control | Shape | Fill | Position |
|---|---|---|---|
| Minimize | downward triangle | `#ffb80d` | left of the three |
| Maximize | square | `#69cdff` | middle |
| Close | circle | `#ff4a4a` | rightmost, in the sharp corner |

- **Size:** each shape is **14 px**, centred in a **32 px** target. (The old
  full-height blue block was most of why the bar looked heavy.)
- **Triangle points down**, though the spinner's points up: down says where
  the window goes.
- **Active:** filled shapes with the thin dark outline. **Hover:** a deeper
  fill.
- **Inactive:** shapes become outlines in the title's quiet ink, crossfading
  over the 150 ms focus token. They stay readable with no colour, which is the
  point.
- **Busy** (the app has not answered `xdg_wm_base.ping` for **2 s**): the three
  slots run the spinner's morph, circle to square to triangle, a **1100 ms**
  loop, each slot a third of a loop apart. A hovered slot shows its own control
  so Close still reads as Close and still works. Evo pings on focus and on
  click, and every **5 s** otherwise. Source: `busy.rs` `PING_EVERY`,
  `PONG_WITHIN`, `SPIN_PERIOD`.
  - Reduced: each slot blinks its colour in place.
  - Off: each shape shows its own dark outline.

Everything else interactive in the shell uses remochi's controls (see the
remochi identity). Pyxis does not restyle them, except that Bloom's dialogs
use remochi's caps buttons inside the shell's own scrim.

---

## 7. Motion: Lumen

Lumen treats windows as light through a lens. Three ideas: **focus** (blur
resolves), **shape** (a window rounds into an orb of its accent colour to
travel), **light** (glow, ripple, glint). The genie funnel was retired as too
macOS. Source: `spec.rs`; the long design is `docs/internal/evo-animation-plan.md`
("Lumen") and CLAUDE.md, "Window animations (Evo)".

Animation is a render-time pose. It never changes where a window is for input,
focus or `ListWindows`; those use the final geometry from the first frame.

### 7.1 The effects

| Effect | When | Duration | What you see |
|---|---|---|---|
| **Bloom** | A window opens | **420 ms** | Scale 0.90 to 1 on a damped spring (zeta 0.86), a 16 px rise, alpha done by 35%. Blur 10 px resolves to 0 as `(1-p)^3`. Corner radius 42 settles to the frame's own. Accent tint 0.45 gone by half way. A radial glow of the accent beneath, peaking at 0.33, reaching 0.75 of the window's long side. |
| **Droplet** | A window minimizes | spring envelope **720 ms**, reads about **480 ms** | The window shrinks to its dock icon's short side while its corners round to a circle, blurs to 6 px, tints and glows to its accent, travels a curved arc (control point 70 px above the higher end), squashes and stretches along its velocity (up to 1.3 : 1/1.3), then shrinks 55% and fades in the last 14%. It lands on the dock icon, which ripples. |
| **Rise** | A window restores | spring envelope **680 ms** | Droplet run backwards, on the same spring, so an interrupted minimize turns round with no jump. |
| **Evaporate** | A window closes | about **380 ms** | The picture burns away from its close button: a threshold dissolve over noise plus distance from the button. Pixels near the edge glow toward `accent*0.45 + white*0.55`. Swells 1 to 1.04, blurs to 2.5 px, loses its shadow by 30%, fades over the last 20%. |
| **Recede** | Show desktop (Super+D) | **460 ms** each, **45 ms** apart | Every window steps back: scale 1 to 0.88, 14 px down, blur to 10 px, fading. Front-most first. No icon is landed on. |
| **Emerge** | Show desktop again | **440 ms** each, **45 ms** apart | The same set returns, back-most first, the stack restored. |
| **Settle glint** | Maximize, unmaximize, fullscreen, work-area fit | from 42% to the end of the **280 ms** geometry spring | A soft diagonal band of white light crosses from top-left to bottom-right, peak alpha `0.32 * sin(pi g)`, clipped to the frame's corners. Shows best on dark windows. |
| **Geometry** | Maximize, unmaximize, fullscreen, refit | **280 ms** | The visible rect springs (zeta 1) from the old frame to the new, drawn from a snapshot so title text is never re-rasterised. |
| **Veil / Clear** | Lock / unlock | **600 ms** / **900 ms** | Windows blur to 14 px, desaturate and fade; unlocking brings live surfaces back through Bloom's shader from blur 14 and scale 1.04, around the lock screen's field. |
| **Popup** | Menu or popup appears | in **120 ms**, out **80 ms** | Fade plus scale 0.97 from the anchor edge. |
| **Focus** | Window gains/loses focus | **150 ms** | Title glass, text and controls crossfade; the shadow grows or shrinks. |
| **Lift** | A move grab begins | **120 ms** | The shadow lifts further. |

Bloom's side of the same language (`motion.ts` `LUMEN`):

| Effect | Value |
|---|---|
| Dock ripple on landing | 0.62 s, expo out, rings 14 px past the icon in the app's hue |
| Dock icon dip | 0.55 s, a damped 5 px dip |
| Wallpaper drift on show desktop | scales 1 to 1.035 over 0.7 s, ease in-out, and back |
| Panel entrance | 8 px travel, scale 0.97 |

### 7.2 Modes

The user picks Full, Reduced or Off in Settings (also `data-motion` on
`<html>`, and `EVO_ANIMATIONS` in Evo). The rule is the same in every
effect:

| Mode | Rule |
|---|---|
| **Full** | As in the table. Springs, blur, glow, glint, travel. |
| **Reduced** | Opacity only: a fade in place, **150 ms at most**, no travel, no blur, no stagger, no glint. The cap applies before any speed scale. |
| **Off** | Nothing animates. Windows appear and vanish; state changes instantly. |

Static styling is **not** animation: shadows, rounded corners and the frost
are drawn in Reduced and Off. Only the transitions between states follow the
mode. Interactive move and resize are never smoothed in any mode.

---

## 8. Depth

A window reads as one rounded card above whatever is beneath it.
Source: `depth.rs`.

| State | Blur sigma | Offset down | Peak alpha |
|---|---|---|---|
| Inactive | 6 px | 3 px | 0.34 |
| Focused | 14 px | 9 px | 0.56 |
| Lifted (being dragged), added on top | +6 px | +8 px | +0.10 |

- Every shadow is a closed-form blurred rounded rectangle that follows the
  waterdrop corners (picked per quadrant), and is masked to the outside of the
  frame so the translucent title bar is never darkened from behind.
- **Where there is no shadow:** the shell, the overlay, popups, a window with
  no bindable app id yet, and any maximized or fullscreen window (it fades out
  as the window goes square).
- **Focus crossfade:** the title bar's glass, text and controls dim to **55%**
  when the window is inactive (`HEADER_INACTIVE`), over the 150 ms focus token.
  Only the label dims; the glass keeps its alpha so the wallpaper's colour does
  not turn muddy.
- **Content corners:** client content is clipped to 16 px at the bottom
  corners to match the frame. Maximized windows are square all round.
- **A window Evo does not decorate** (an app that draws its own frame, or an
  X11 window) gets a plain 8 px shadow radius, not the waterdrop
  (`CLIENT_RADIUS`).

---

## 9. Sound

Pyxis has four system sounds, played through `org.geckoOS.sound`, which finds
files in `system-sounds/` at the repo root by name (`echo/src/services/sound.rs`).

| Event | File |
|---|---|
| Start up | `system-sounds/boot.mp3` |
| Notification | `system-sounds/notification.wav` |
| Error | `system-sounds/error.mp3` |
| Shut down | `system-sounds/shutdown.mp3` |

Volume is set from Quick settings, and the output and its level in Settings,
Sound (`org.geckoOS.audio`, PipeWire through `wpctl`). Do Not Disturb silences
toasts.

Honest state: sound is the least designed part of the identity. The files are
webOS sample sounds kept as placeholders (licensing unconfirmed, so they are
untracked in git), and nothing is written about what Pyxis should sound like.
What can be said now, as direction, not as built fact:

- Sounds should be short, soft and tonal, like the motion: no clicks or
  buzzes.
- Notification and error are the two that fire often. They must be
  distinguishable by shape (rising versus falling), not volume.
- A designed sound set, in the spirit of Lumen, is a to-do. When it exists,
  record its rules here.

---

## 10. Layout

The shell is two Bloom windows: the desktop (wallpaper only) and the overlay
(all shell UI, above apps). Evo keeps apps out of the shell's edges by a work
area. Source: CLAUDE.md, "Visual identity (the shell)" and "Shell overlay".

| Element | Size and place | Notes |
|---|---|---|
| **Status strip** | **36 px** high, full width, top. No background | Ink straight on the wallpaper with a legibility shadow. Search on the left as an underline ("Just type"), the clock centred in Mochi Light, Wi-Fi, volume and battery on the right, which open quick settings. Work-area top inset: 36 |
| **Launch bar**, bottom (default) | **64 px** high, full width, flush to the bottom edge | Material fades up from the edge. Apps and Cards at the left end. App icons in the middle: 40 px accent waterdrops, **52 px** apart, centred on the screen. Notification dashboard at the right end. Work-area bottom inset: 64 |
| **Launch bar**, left rail | **64 px** wide, from y 36 to the bottom (`0,36 64x764` on an 800 px screen) | Apps and Cards at the top, icons 52 px apart, running lines on the left edge, dashboard count alone at the bottom. Work area `{top 36, bottom 0, left 64}`. Setting: `pyxis.launchBar.position` = `bottom` or `left` |
| **Running indicator** | A 3 px line of the app's accent on the screen edge under its icon | Focused: longer and brighter. Starting: pulses (1.2 s). Minimized: faint. Crashed: red |
| **Dashboard** | Right end of the bottom bar | Newest one or two unread notifications as chips, then a count in spinner red that opens the centre. Toasts rise from above it (stack right edge 1268, bottom 728 on 1280x800) and the centre opens upward from it |
| **Panels** | Waterdrop, strong material | Display-type heads. Quick settings opens from the strip's status cluster |
| **Cards** (overview) | Waterdrop cards 170 px high, 150 to 400 px wide by the window's aspect | A strip tinted with the app's accent, icon and name |
| **Title bar** | Evo-drawn, 32 px controls, frosted | Icon at 20 px, name in bold, then the client's title in quiet ink |
| **Work area** | Output minus the insets above | Maximize, first placement, move and resize clamps all use it. **Fullscreen covers everything** |

Panels take the whole screen as their input region and ask for the keyboard
while open; a click outside closes them.

---

## 11. Voice

Plain, short, on the user's side. Copy says what will happen and who is
asking, and puts the security fact where the decision is.

- **Name the app and the action.** "Installer wants to install **Weather** 1.2"
  (the privilege prompt builds its sentence from Nova's name for the app, never
  from text the app supplied).
- **Reassure with a fact, not a feeling.** "Pyxis never asks for your password
  inside an app's window." (`PrivilegePromptDialog.tsx`, asserted by
  `privilegePrompt.test.tsx`.) This is the model sentence for the voice.
- **Say what happens next, with the time it lasts.** "Enter your password to
  allow this for 5 minutes."
- **Errors say what went wrong and what to try** (direction, not yet audited
  across the shell). No codes in the headline, no "Oops".
- **No marketing words.** No "seamless", "powerful" or "magical". The guide
  (`docs/feature-guide/index.html`) describes features in the same flat way:
  "Pyxis has its own system sounds for starting up, notifications, errors and
  shutting down."
- **Uppercase is display type only**, and only for one- or two-word heads.
  Dialog titles in the shell today are in title case ("Launch Error",
  "Shut Down"); whether they stay so is undecided.
- **Name things by who they are.** "Pyxis" for the OS, the app's own name for an
  app. Never "the user" or "the system" in copy people read.

---

## 12. Do and Don't

Do:

- Use the waterdrop for every floating surface with a corner.
- Take shell colour from the tint variables; choose ink, material or edge
  material by what the thing sits on (wallpaper, panel, launch bar).
- Use the app's accent as light: a seam, a line, a glow. One accent per app.
- Use bold oblique once per view, on one word.
- Make every motion say where something came from or went to, and give it a
  Reduced (fade, 150 ms at most) and an Off (instant) form.
- Check ink contrast: at least 4.5:1 on shell surfaces, at least 7:1 on title
  bars.
- Keep shapes readable without colour (triangle, square, circle).

Don't:

- Don't round all four corners alike. A uniform 16 px corner is the macOS look.
- Don't put a floating, centred, rounded white dock on the screen.
- Don't use red, amber and green traffic lights, or recolour the spinner
  shapes (their colours are sampled, not chosen).
- Don't blur behind shell panels in the overlay; it has nothing to blur. Use
  the material.
- Don't draw a title bar on Bloom, the shell.
- Don't use the app accent as a text colour or as `--mochi-primary`.
- Don't write a hex colour into app CSS for chrome when a `--mochi-*` or
  `--px-*` variable exists.
- Don't let a window caption itself in the title bar's primary label; the bar
  shows the trusted app name.
- Don't add a fourth motion language. New effects use focus, shape or light.

---

## 13. Open items and holds

- Per-app accent in manifests, `appinfo.json`, Nova (upstream doc P2).
- Apps' hand-drawn waterdrops to `waterdrop(size)` (P4).
- **Held by the user, 2026-11-12:** what the title bar says, and the apps' own
  big titles, wait for the Gecko port's branding (P6).
- The frost of windows under another window's bar (F2) is not built.
- A designed sound set (section 9).
- The identity stops at Bloom's two documents and Evo's frame; reaching the
  apps is `docs/internal/pyxis-identity-upstream.md`.
