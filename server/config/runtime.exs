import Config

# Settings are in config.exs (and prod.exs for the image). Each can be
# overridden from the environment; usually only the addresses are:
#   DOCSERVER_PUBLIC_URL=https://docs.example.com WOPI_HOSTS=https://app.example.com
env = &System.get_env/1
list = fn v -> v |> String.split(",", trim: true) |> Enum.map(&String.trim/1) end
int = &String.to_integer/1
text = & &1
on = &(&1 == "true")
not_off = &(&1 != "false")

overrides = [
  port: {"DOCSERVER_PORT", int},
  public_url: {"DOCSERVER_PUBLIC_URL", text},
  collabora_discovery: {"COLLABORA_DISCOVERY", text},
  frame_ancestors: {"FRAME_ANCESTORS", list},
  web_dir: {"DOCSERVER_WEB_DIR", text},
  fonts_dir: {"DOCSERVER_FONTS_DIR", text},
  dict_dir: {"DOCSERVER_DICT_DIR", text},
  save_on_exit: {"SAVE_ON_EXIT", on},
  review: {"REVIEW", on},
  idle_save_after: {"IDLE_SAVE_AFTER", int},
  collab: {"COLLAB", not_off},
  rooms_dir: {"ROOMS_DIR", text},
  room_grace: {"ROOM_GRACE", int},
  verify_tls: {"WOPI_VERIFY_TLS", not_off},
  cacertfile: {"WOPI_CACERTFILE", text}
]

for {key, {var, parse}} <- overrides, v = env.(var), do: config(:docserver, [{key, parse.(v)}])

converter = [
  enabled: {"CONVERT_ENABLED", not_off},
  bin: {"DOCCONV_BIN", text},
  workers: {"CONVERT_WORKERS", int},
  timeout: {"CONVERT_TIMEOUT_MS", int},
  soffice: {"SOFFICE_BIN", text},
  soffice_jobs: {"SOFFICE_JOBS", int}
]

for {key, {var, parse}} <- converter, v = env.(var), do: config(:docserver, converter: [{key, parse.(v)}])

# LibreOffice for other formats, when the image has it
if config_env() == :prod and !env.("SOFFICE_BIN") do
  if p = System.find_executable("soffice"), do: config(:docserver, converter: [soffice: p])
end

# WOPI hosts: from the environment they replace the list; in dev they add to it
# (the test host stays). Who may embed the editor: 'self' and these hosts.
if hosts = env.("WOPI_HOSTS") do
  hosts = list.(hosts)
  hosts = if config_env() == :dev, do: ["http://127.0.0.1:8781" | hosts], else: hosts
  config :docserver, wopi_hosts: hosts
  unless env.("FRAME_ANCESTORS"), do: config(:docserver, frame_ancestors: ["'self'" | hosts])
end
