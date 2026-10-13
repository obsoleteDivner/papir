import Config

config :docserver, test_host: true

config :docserver, test_host_seed: Path.expand("../../corpus/local", __DIR__)
