# Papir

A .docx editor and .xlsx viewer for the browser, and a .docx → PDF converter, built on its own core in Rust. Embeds into other applications through WOPI.

## Layout

| Directory | What it is |
| --- | --- |
| `crates/core` | The core: lossless DOCX reading and writing, layout, editing, merging edits of several people, XLSX |
| `crates/wasm` | The core for the browser: document, spreadsheet, spelling |
| `crates/cli` | `docconv`: .docx → PDF |
| `web` | The editor (`index.html`) and the spreadsheet viewer (`sheet.html`) |
| `server` | `docserver` in Elixir: WOPI, discovery, editing together, conversion |
| `dict` | Hunspell dictionaries (`uk_UA`, `en_US`), licenses in `dict/LICENSES.md` |
| `fonts` | Metric-compatible fonts, licenses in `fonts/LICENSES.md` |

## Build

You need Rust, Erlang and Elixir 1.18, and Node.js or Docker: without Node, TypeScript is compiled in a container.

Once:

```bash
rustup target add wasm32-unknown-unknown
cargo install wasm-bindgen-cli --version 0.2.129 --locked
```

Build (from the repository root):

```bash
./scripts/build-web.sh
cargo build --release -p docconv
(cd server && mix deps.get)
```

After changes in `crates/` or `web/src`, run `./scripts/build-web.sh` (editor) or `cargo build --release -p docconv` (converter) again.

## Run

```bash
cd server && elixir --name docconv@127.0.0.1 -S mix run --no-halt
```

- `http://127.0.0.1:8780/` — server status;
- `http://127.0.0.1:8780/hosting/discovery` — discovery;
- `http://127.0.0.1:8781/` — a test WOPI host: files from `corpus/local`, opened for editing or viewing, `?user=Name` to open as another person.

Editor only, without the server: `python3 scripts/dev-static.py 8765`, then `http://127.0.0.1:8765/web/index.html?open=/path/to/file.docx`.

## Converter

```bash
target/release/docconv convert file.docx -o file.pdf
target/release/docconv convert ./in -o ./pdf --jobs 8
```

`docconv` also accepts the arguments of `soffice --headless --convert-to pdf`. `docserver` has a conversion pool and `POST /api/v1/convert` (.docx in the body → PDF).

## Benchmarks

```bash
cargo run --release -p papir-core --example convert -- --runs 3 --jobs 4 dir...   # conversion to PDF: time per file, documents and pages per second, memory
cargo run --release -p papir-core --example pages -- file.docx...                  # pages, parsing and layout time
cargo run --release -p papir-core --example xlsx_scan -- dir...                    # opens every .xlsx, failures and total time
```

`corpus/local` (documents for checks) and `local/` are not in git.

## Configuration

Everything is set in `server/config/config.exs` (with comments), the image's paths in `server/config/prod.exs`. A deployment needs just two addresses:

```bash
docker build -t papir .
docker run -p 8780:8780 -e DOCSERVER_PUBLIC_URL=https://docs.example.com -e WOPI_HOSTS=https://app.example.com papir
```

`DOCSERVER_PUBLIC_URL` is the address browsers reach Papir at; `WOPI_HOSTS` is the address of the application documents are opened from (the WOPI host). Any other value from the config can be overridden by an environment variable too (see `server/config/runtime.exs`), though it is rarely needed. `--build-arg WITH_SOFFICE=1` adds LibreOffice to the image for formats other than .docx.

## License

Apache License 2.0, see [LICENSE](LICENSE) and [NOTICE](NOTICE). The fonts and the spelling dictionaries keep their own licenses: [fonts/LICENSES.md](fonts/LICENSES.md), [dict/LICENSES.md](dict/LICENSES.md).
