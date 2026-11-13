# virtual-display

A 1920x1080 HiDPI virtual screen, for running the lab's bench on a Mac
that someone is using. macOS throttles a lab or browser window that is
covered, so measuring on the real screen means taking it over; on a
virtual screen nobody looks at, the window draws undisturbed.

```sh
clang -fobjc-arc -framework Foundation -framework CoreGraphics \
  scripts/virtual-display/virtual-display.m -o target/virtual-display
target/virtual-display 60 &   # prints where the screen is
# open the measuring window there, without activating it, for example:
open -gna "Google Chrome" --args --user-data-dir="$TMPDIR/gpui-av-chrome" \
  --mute-audio --window-position=<x+80>,<y+40> --window-size=1600,900 <url>
kill %1                       # removes the screen
```

- It uses CoreGraphics' private `CGVirtualDisplay` classes. Dev use only;
  any macOS release may change or remove them.
- It changes no setting. macOS may put a new display in a mirror set and
  make it main (it did on macOS 27.2); the helper undoes that at once for
  the login session only, keeps the real screens where they were, and
  places the new one beyond the leftmost. If the layout still differs after
  one correction, it exits and the screen disappears.
- Numbers from it are not the same as from the real screen: the window
  never has focus there, and GPUI's canvas in an unfocused browser window
  draws at about 26 to 30 frames a second. Browser-composited video was
  unaffected (1080p60 at 60 fps, 0 dropped) but a few clips counted 1 to
  3% dropped that did not on the focused real screen. See
  `experiments/e3-web-overlay`.
- The run on 2026-11-13 used an earlier copy with that Mac's layout
  written in; this version records the layout when it starts and has not
  been run since.
