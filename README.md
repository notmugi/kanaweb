# kanaweb

A small, self-contained set of web tools for studying Japanese kana and vocabulary.

- **`Flashcards.html`** — flashcard app with drawing canvas, tablet mode, adjustable stroke width, and a custom vocabulary editor that saves to `vocab.json`.
- **`Quiz.html`** — kana quiz / drill page.
- **`kanaweb-server`** — tiny zero-dependency Rust webserver that serves the HTML files **and** accepts `PUT /vocab.json` so the Flashcards editor can save back to disk. Use this if you want to host the app for yourself or a friend.

Everything runs locally. No accounts, no telemetry, no external requests.

---

## Quick start

You can use the HTML files completely standalone — just open `Flashcards.html` in a browser. In that mode, vocab edits are saved to `localStorage` and you can use the **Export** button to download an updated `vocab.json` for safekeeping.

If you want edits to save automatically to a file, run the bundled server:

```bash
git clone https://github.com/notmugi/kanaweb.git
cd kanaweb
./serve.sh
```

Then open <http://127.0.0.1:8080/>.

The script builds the release binary on first run (you need [Rust](https://www.rust-lang.org/tools/install) installed) and then just runs it on subsequent runs.

### Server options

```text
./serve.sh                        # 127.0.0.1:8080
./serve.sh --port 9000            # custom port
./serve.sh --host 0.0.0.0         # listen on all interfaces (LAN access)
./serve.sh --dir /some/other/dir  # serve from a different directory
./serve.sh --help
```

The Flashcards app will create `vocab.json` on first save — no setup needed.

---

## How the server works

`kanaweb-server` is intentionally minimal — a single `src/main.rs` file with zero dependencies beyond `std`.

What it does:

| Request                          | Behavior                                                    |
| -------------------------------- | ----------------------------------------------------------- |
| `GET /`                          | Serves `Flashcards.html`                                    |
| `GET /<file>`                    | Serves static files from the working directory              |
| `HEAD /<file>`                   | Same as GET, no body                                        |
| `PUT /vocab.json`                | Writes the request body to `vocab.json` (atomic)            |
| `PUT /.vocab.json`               | Writes the request body to `.vocab.json` (atomic)           |
| `OPTIONS *`                      | Returns `Allow: GET, HEAD, PUT, OPTIONS`                    |
| anything else                    | `404` / `405` / `403` as appropriate                        |

Safety guards:

- `PUT` is **allowlisted** to exactly the two vocab filenames. Nothing else can be uploaded.
- Path traversal is blocked at the component level — no `..`, no absolute paths.
- Writes are atomic (temp file in the same directory, then `rename`), so an interrupted save can't corrupt `vocab.json`.
- 8 MiB body cap, 16 KiB header cap, 30-second read/write timeouts.
- Bound to `127.0.0.1` by default. Use `--host 0.0.0.0` only on a trusted network.

The server is one-thread-per-connection. Fine for a single user or a handful of devices on a LAN — not designed for the public internet.

---

## Shipping the app to a friend

After building once:

```bash
cargo build --release
```

Hand them a folder containing:

- `target/release/kanaweb-server` (the binary)
- `Flashcards.html`
- `Quiz.html` (optional)

They run `./kanaweb-server` in that folder and open <http://127.0.0.1:8080/>. The Flashcards app creates `vocab.json` automatically on first save. The binary is ~300 KB after `strip` and only depends on glibc.

---

## Repository layout

```
kanaweb/
├── Cargo.toml          # Rust manifest
├── Cargo.lock          # locked deps (none, but pinned anyway)
├── src/
│   └── main.rs         # the entire server, ~510 lines
├── serve.sh            # convenience wrapper (builds + runs)
├── Flashcards.html     # the flashcards app
├── Quiz.html           # the quiz app
├── LICENSE             # GPL-3.0
└── README.md
```

---

## License

GPL-3.0-or-later. See [`LICENSE`](LICENSE).
