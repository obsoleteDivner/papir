# docserver with the editor: Rust → WASM, TypeScript, Elixir release.

FROM rust:1.96 AS wasm
RUN rustup target add wasm32-unknown-unknown && cargo install wasm-bindgen-cli --version 0.2.129 --locked
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY crates crates
RUN cargo build -p papir-wasm --target wasm32-unknown-unknown --release \
 && wasm-bindgen --target web --out-dir /pkg target/wasm32-unknown-unknown/release/papir_wasm.wasm \
 && cargo build -p docconv --release && cp target/release/docconv /docconv

FROM node:20-alpine AS web
WORKDIR /web
COPY web/package.json web/package-lock.json* web/tsconfig.json ./
RUN npm install --no-audit --no-fund
COPY web/src src
COPY --from=wasm /pkg pkg
RUN npx tsc -p .

# the runtime stage below must use the same Debian release as this image
FROM elixir:1.18 AS server
ENV MIX_ENV=prod
WORKDIR /server
COPY server/mix.exs server/mix.lock ./
RUN mix local.hex --force && mix local.rebar --force && mix deps.get --only prod
COPY server/config config
COPY server/lib lib
RUN mix release

FROM debian:trixie-slim
# WITH_SOFFICE=1 adds LibreOffice for formats other than .docx (much bigger image)
ARG WITH_SOFFICE=0
RUN apt-get update && apt-get install -y --no-install-recommends libssl3t64 libncurses6 ca-certificates locales hunspell-uk hunspell-en-us \
 && if [ "$WITH_SOFFICE" = "1" ]; then apt-get install -y --no-install-recommends libreoffice-writer-nogui libreoffice-calc-nogui libreoffice-impress-nogui; fi \
 && rm -rf /var/lib/apt/lists/* && sed -i 's/# en_US.UTF-8/en_US.UTF-8/' /etc/locale.gen && locale-gen \
 # spelling dictionaries for the browser, compressed next to the originals
 && mkdir -p /app/dict /var/lib/papir/rooms && for f in uk_UA en_US; do cp /usr/share/hunspell/$f.aff /usr/share/hunspell/$f.dic /app/dict/; done \
 && gzip -k9 /app/dict/* && chown nobody /var/lib/papir/rooms
ENV LANG=en_US.UTF-8 PAPIR_FONTS=/app/fonts
WORKDIR /app
COPY --from=server /server/_build/prod/rel/docserver ./
COPY --from=wasm /docconv /app/bin/docconv
COPY web/index.html web/sheet.html web/style.css web/sheet.css web/icons.css web/
COPY --from=web /web/dist web/dist
COPY --from=wasm /pkg web/pkg
COPY fonts fonts
# journals of documents being edited together: keep them across restarts
VOLUME /var/lib/papir/rooms
EXPOSE 8780
USER nobody
CMD ["/app/bin/docserver", "start"]
