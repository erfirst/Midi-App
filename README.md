# Midi Reader and Editor

A MIDI player and piano-roll editor written in Rust with [egui](https://github.com/emilk/egui).
It runs as a desktop app and in the browser (WebAssembly).

Demo: [ethanfirst.com/midi.html](https://ethanfirst.com/midi.html)

## Features

- Open `.mid` files with the **Open** button, or drag and drop them onto the window.
- A full 128-key piano roll with note names, bar numbers and per-channel colors.
- Playback with a moving playhead that follows along.
  - **Web:** a built-in synthesizer (Web Audio).
  - **Desktop:** sends notes to your first MIDI output device (on Windows this is the
    built-in "Microsoft GS Wavetable Synth").
- Editing:
  - **Click** an empty spot to add a note. Snap and default note length are in the toolbar.
  - **Click** a note (or right-click it) to delete it.
  - **Drag** a note to move it, or drag its right edge to resize it.
  - **Drag** empty space to pan.
- Save or download your edits as a `.mid` file. Tempo, channels and timing are preserved.
- Undo and redo.

### Controls

| Action | Shortcut |
| --- | --- |
| Play / stop | `Space` |
| Jump to start | `Home` |
| Move playhead | Click or drag the bar ruler |
| Undo / redo | `Ctrl+Z` / `Ctrl+Shift+Z` or `Ctrl+Y` |
| Open / save | `Ctrl+O` / `Ctrl+S` |
| Scroll | Mouse wheel (`Shift`+wheel scrolls sideways) |
| Zoom | `Ctrl`+wheel, pinch, or the Zoom slider |

## Building

You need Rust 1.95 or newer (`rustup update`).

### Desktop

```sh
cargo run --release
```

On Linux you'll need a few system packages first:

```sh
sudo apt-get install pkg-config libasound2-dev libxkbcommon-dev libwayland-dev libdbus-1-dev
```

### Web

```sh
rustup target add wasm32-unknown-unknown
cargo install --locked trunk
trunk serve            # dev server at http://127.0.0.1:8080
trunk build --release  # production build into ./dist
```

## Putting it on your website

1. Run `trunk build --release`.
2. Upload **everything** in `dist/` to a folder on your site, for example `/midi/`.
   The build uses relative paths (see `Trunk.toml`), so any folder works.
3. Link to it (`https://yoursite.com/midi/`) or embed it in another page:

   ```html
   <iframe src="/midi/" style="width:100%;height:80vh;border:0" allow="autoplay"></iframe>
   ```

Make sure your web server sends `.wasm` files with the `application/wasm` MIME type.
GitHub Pages, Netlify, Vercel, Cloudflare Pages and most Apache/nginx setups already do this.
If you're updating an old deployment, upload the new `index.html` together with its
matching `midi_app-*.js` and `*_bg.wasm` files, and delete the old ones.

## Development

`./check.sh` runs the same checks as CI: formatting, clippy (native and wasm), tests and a web build.
