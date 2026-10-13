import Config

# The Docker image: where its files are. Addresses and the rest: config.exs,
# any of them can be overridden from the environment (runtime.exs).
config :docserver,
  web_dir: "/app/web",
  fonts_dir: "/app/fonts",
  dict_dir: "/app/dict",
  rooms_dir: "/var/lib/papir/rooms",
  converter: [bin: "/app/bin/docconv", fonts: "/app/fonts", soffice: nil]
