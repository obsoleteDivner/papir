# Папір

Редактор .docx і переглядач .xlsx у браузері та конвертер .docx → PDF на власному ядрі (Rust). Вбудовується в інші застосунки через WOPI.

## Склад

| Каталог | Що це |
| --- | --- |
| `crates/core` | Ядро: читання й запис DOCX без втрат, верстка, редагування, злиття правок кількох людей, XLSX |
| `crates/wasm` | Ядро для браузера: документ, таблиця, правопис |
| `crates/cli` | `docconv`: .docx → PDF |
| `web` | Редактор (`index.html`) і переглядач таблиць (`sheet.html`) |
| `server` | `docserver` на Elixir: WOPI, discovery, спільне редагування, конвертація |
| `dict` | Словники Hunspell (`uk_UA`, `en_US`), ліцензії в `dict/LICENSES.md` |
| `fonts` | Метрично сумісні шрифти |

## Збірка

Потрібні Rust, Erlang та Elixir 1.18, а також Node.js або Docker: без Node TypeScript збирається в контейнері.

Один раз:

```bash
rustup target add wasm32-unknown-unknown
cargo install wasm-bindgen-cli --version 0.2.129 --locked
```

Збірка (з кореня репозиторію):

```bash
./scripts/build-web.sh
cargo build --release -p docconv
(cd server && mix deps.get)
```

Після змін у `crates/` чи `web/src` знову запустити `./scripts/build-web.sh` (редактор) або `cargo build --release -p docconv` (конвертер).

## Запуск

```bash
cd server && elixir --name docconv@127.0.0.1 -S mix run --no-halt
```

- `http://127.0.0.1:8780/` — стан сервера («Працює»);
- `http://127.0.0.1:8780/hosting/discovery` — discovery;
- `http://127.0.0.1:8781/` — тестовий WOPI-хост: файли з `corpus/local`, відкриття для редагування чи перегляду, `?user=Ім'я` — від імені іншої людини.

Лише редактор, без сервера: `python3 scripts/dev-static.py 8765`, далі `http://127.0.0.1:8765/web/index.html?open=/шлях/до/файлу.docx`.

## Конвертер

```bash
target/release/docconv convert файл.docx -o файл.pdf
target/release/docconv convert ./вхідні -o ./pdf --jobs 8
```

`docconv` приймає й аргументи `soffice --headless --convert-to pdf`. У `docserver` є пул конвертації і `POST /api/v1/convert` (.docx у тілі → PDF).

## Бенчмарки

```bash
cargo run --release -p papir-core --example convert -- --runs 3 --jobs 4 каталог...  # конвертація в PDF: час на файл, документів і сторінок за секунду, пам'ять
cargo run --release -p papir-core --example pages -- файл.docx...      # сторінки, час розбору й верстки
cargo run --release -p papir-core --example xlsx_scan -- каталог...     # відкриває кожен .xlsx, збої й загальний час
```

`corpus/local` (документи для перевірок) і `local/` не потрапляють у git.

## Налаштування

Усе задано в `server/config/config.exs` (з коментарями), шляхи образу — у `server/config/prod.exs`. Для розгортання потрібні лише дві адреси:

```bash
docker build -t papir .
docker run -p 8780:8780 -e DOCSERVER_PUBLIC_URL=https://docs.example.com -e WOPI_HOSTS=https://app.example.com papir
```

`DOCSERVER_PUBLIC_URL` — адреса, за якою браузер бачить Папір; `WOPI_HOSTS` — адреса застосунку, з якого відкривають документи (WOPI-хоста). Будь-яке інше значення з конфігу теж можна перекрити змінною середовища (перелік — у `server/config/runtime.exs`), але зазвичай це не потрібно. `--build-arg WITH_SOFFICE=1` додає в образ LibreOffice для форматів, крім .docx.
