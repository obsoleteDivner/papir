import Config

config :docserver,
  port: 8780,
  # address browsers use to reach docserver (urlsrc in discovery, assets)
  public_url: "http://127.0.0.1:8780",
  # Collabora discovery to pass other formats through; nil = docx only
  collabora_discovery: nil,
  # WOPISrc must start with one of these (scheme://host[:port][/path]); WOPI_HOSTS
  wopi_hosts: ["http://127.0.0.1:8781"],
  # who may embed the editor (CSP frame-ancestors)
  frame_ancestors: ["'self'", "http://127.0.0.1:8781"],
  web_dir: Path.expand("../../web", __DIR__),
  fonts_dir: Path.expand("../../fonts", __DIR__),
  dict_dir: Path.expand("../../dict", __DIR__),
  # unsaved edits are dropped when the editor is closed; true: written to the host
  save_on_exit: false,
  # the «Рецензування» tab and comments in the editor
  review: false,
  # editing together; journals of open documents (nil: a temporary directory);
  # seconds a room waits after a dropped connection
  collab: true,
  rooms_dir: nil,
  room_grace: 20,
  # seconds without a ping before a session ends (and, with save_on_exit, its edits are written)
  idle_save_after: 30,
  # client draft upload period, seconds
  draft_interval: 10,
  verify_tls: true,
  cacertfile: nil,
  # PDF conversion pool (process registered as Lop.Pool)
  converter: [
    enabled: true,
    bin: Path.expand("../../target/release/docconv", __DIR__),
    fonts: Path.expand("../../fonts", __DIR__),
    # nil = one worker per scheduler
    workers: nil,
    timeout: 30_000,
    # formats other than .docx (and .docx the core refuses) go to LibreOffice
    soffice: System.find_executable("soffice"),
    soffice_jobs: 4
  ],
  # a WOPI host for trying the editor (dev and tests)
  test_host: false,
  test_host_port: 8781

config :logger, level: :info

if File.exists?(Path.join(__DIR__, "#{config_env()}.exs")) do
  import_config "#{config_env()}.exs"
end
