defmodule Docserver.Router do
  @moduledoc "HTTP entry points: discovery, editor launch, session API, assets."
  use Plug.Router
  require Logger
  alias Docserver.{Discovery, Sessions, Wopi}

  @max_body 100_000_000

  plug Plug.Static, at: "/editor", from: {__MODULE__, :web_dir, []}, only: ~w(style.css icons.css sheet.css dist pkg), gzip: false, headers: %{"cache-control" => "no-cache"}
  plug Plug.Static, at: "/fonts", from: {__MODULE__, :fonts_dir, []}, headers: %{"cache-control" => "public, max-age=86400"}
  # spelling dictionaries (Hunspell); .gz next to each file goes to browsers that accept it
  plug Plug.Static, at: "/dict", from: {__MODULE__, :dict_dir, []}, gzip: true, headers: %{"cache-control" => "public, max-age=604800"}
  plug :match
  plug :dispatch

  def web_dir, do: Application.fetch_env!(:docserver, :web_dir)
  def fonts_dir, do: Application.fetch_env!(:docserver, :fonts_dir)
  def dict_dir, do: Application.get_env(:docserver, :dict_dir) || Path.expand("../../dict", __DIR__)

  get "/health" do
    send_resp(conn, 200, "ok")
  end

  # a page for people who open the address: the service is up, where things are
  get "/" do
    base = Application.fetch_env!(:docserver, :public_url)
    test_host = Application.get_env(:docserver, :test_host) && "http://127.0.0.1:#{Application.get_env(:docserver, :test_host_port)}/"
    rooms = Registry.count(Docserver.Rooms)
    li = fn {href, text, note} -> ~s(<li><a href="#{href}">#{text}</a> <span>#{note}</span></li>) end
    links =
      [{"#{base}/hosting/discovery", "Discovery", "адреси редактора для WOPI-хоста"}, {"#{base}/health", "Стан", "відповідає «ok»"}] ++
        if(test_host, do: [{test_host, "Тестовий WOPI-хост", "відкрити документ у редакторі або таблицю в переглядачі"}], else: [])

    html = """
    <!doctype html><html lang="uk"><meta charset="utf-8"><title>Папір</title>
    <body style="font:15px system-ui;margin:0;background:#f7f8fa;color:#1f2226">
    <div style="background:#0d63b3;color:#fff;padding:14px 24px;font-weight:600">Папір</div>
    <div style="max-width:640px;margin:32px auto;background:#fff;border:1px solid #dcdde1;border-radius:8px;padding:20px 28px">
    <h2 style="margin:0 0 6px;color:#2f9e44">Працює</h2>
    <p style="color:#5d6066;margin:0 0 16px">Редактор відкривається за адресою з discovery, а не тут. Документів, які зараз редагують разом: #{rooms}.</p>
    <ul style="line-height:1.9;padding-left:18px">#{Enum.map_join(links, "", li)}</ul></div></body></html>
    """

    conn |> put_resp_content_type("text/html") |> send_resp(200, html)
  end

  get "/hosting/discovery" do
    conn |> put_resp_content_type("text/xml") |> send_resp(200, Discovery.xml())
  end

  # The host opens the editor as `urlsrc` + lang, ui_defaults, WOPISrc, access_token.
  get "/docx" do
    open_page(conn, :docx)
  end

  # spreadsheets: the viewer
  get "/xlsx" do
    open_page(conn, :xlsx)
  end

  # before the script runs: the mode is known, the tools wait (no flash of the editing ribbon)
  defp body_class(cfg) do
    ["compact", "pending", if(!cfg.canWrite, do: "readonly"), if(!cfg.review, do: "no-review")]
    |> Enum.reject(&is_nil/1)
    |> Enum.join(" ")
  end

  defp open_page(conn, kind) do
    conn = fetch_query_params(conn)
    %{"WOPISrc" => src, "access_token" => token} = Map.merge(%{"WOPISrc" => nil, "access_token" => nil}, conn.query_params)

    cond do
      not Wopi.allowed?(src) or not is_binary(token) or token == "" ->
        error_page(conn, 403, "Документ недоступний: невідоме джерело.")

      true ->
        case Wopi.check_file_info(src, token) do
          {:ok, info} ->
            {:ok, s} = Sessions.create(src, token, info)
            if kind == :xlsx, do: sheet_page(conn, s), else: editor_page(conn, s)

          {:error, 401} ->
            error_page(conn, 401, "Сесію завершено. Оновіть сторінку в системі.")

          {:error, reason} ->
            Logger.warning("CheckFileInfo failed: #{inspect(reason)}")
            error_page(conn, 502, "Не вдалося отримати відомості про документ.")
        end
    end
  end

  get "/api/session/:sid/file" do
    with {:ok, s} <- Sessions.get(sid) do
      Sessions.touch(sid)

      # editing together: editors start from the room's base and its changes;
      # viewers see the saved document
      result =
        case s.can_write && Docserver.Room.lookup(s.src) do
          {:ok, room} ->
            case Docserver.Room.base(room) do
              {:ok, b, _} -> {:ok, b}
              _ -> Wopi.get_file(s.src, s.token)
            end

          _ ->
            if s.draft && Application.get_env(:docserver, :save_on_exit, false), do: {:ok, s.draft}, else: Wopi.get_file(s.src, s.token)
        end

      case result do
        {:ok, body} ->
          conn
          |> put_resp_header("cache-control", "no-store")
          |> put_resp_content_type("application/octet-stream")
          |> send_resp(200, body)

        {:error, 401} ->
          send_resp(conn, 401, "session expired")

        _ ->
          send_resp(conn, 502, "file unavailable")
      end
    else
      _ -> send_resp(conn, 404, "no session")
    end
  end

  # explicit save: write to the host now
  post "/api/session/:sid/file" do
    with {:ok, body, conn} <- read_all(conn),
         version = version(conn) do
      result =
        with [rv | _] <- get_req_header(conn, "x-papir-room-version"),
             {:ok, s} <- Sessions.get(sid),
             {:ok, room} <- Docserver.Room.lookup(s.src) do
          case Docserver.Room.save(room, sid, body, String.to_integer(rv)) do
            {:error, :not_synced} -> {:error, :not_synced}
            other -> other
          end
        else
          _ -> Sessions.save(sid, body, version, :manual)
        end

      case result do
        {:error, :not_synced} -> json(conn, 409, %{ok: false, error: "not_synced"})
        :ok -> json(conn, 200, %{ok: true, superseded: true})
        {:error, :superseded} -> json(conn, 409, %{ok: false, error: "superseded"})
        {:error, :read_only} -> json(conn, 403, %{ok: false, error: "read_only"})
        {:error, :not_found} -> json(conn, 404, %{ok: false, error: "no_session"})
        {:error, 401} -> json(conn, 401, %{ok: false, error: "session_expired"})
        {:error, e} -> json(conn, 502, %{ok: false, error: inspect(e)})
      end
    else
      {:error, :too_large} -> json(conn, 413, %{ok: false, error: "too_large"})
    end
  end

  # unsaved edits kept here until save or exit
  post "/api/session/:sid/draft" do
    with {:ok, body, conn} <- read_all(conn) do
      case Sessions.put_draft(sid, body, version(conn)) do
        :ok -> json(conn, 200, %{ok: true})
        {:error, :superseded} -> json(conn, 409, %{ok: false, error: "superseded"})
        {:error, e} -> json(conn, 404, %{ok: false, error: to_string(e)})
      end
    else
      {:error, :too_large} -> json(conn, 413, %{ok: false, error: "too_large"})
    end
  end

  post "/api/session/:sid/ping" do
    case Sessions.get(sid) do
      {:ok, s} ->
        Sessions.touch(sid)
        json(conn, 200, %{ok: true, superseded: s.superseded})

      _ ->
        json(conn, 404, %{ok: false})
    end
  end

  # .docx in the body → PDF in the reply
  post "/api/v1/convert" do
    with {:ok, body, conn} <- read_all(conn) do
      dir = Path.join(System.tmp_dir!(), "papir-http-" <> Integer.to_string(System.unique_integer([:positive])))
      File.mkdir_p!(dir)
      src = Path.join(dir, "input")
      File.write!(src, body)

      try do
        case Docserver.Convert.convert(src, dir, in_type: "docx") do
          {:ok, out} ->
            conn |> put_resp_content_type("application/pdf", nil) |> send_resp(200, File.read!(out))

          {:error, {:unavailable, _}} ->
            conn |> put_resp_header("retry-after", "1") |> json(429, %{ok: false, error: "busy"})

          {:error, e} ->
            json(conn, 422, %{ok: false, error: inspect(e)})
        end
      after
        File.rm_rf(dir)
      end
    else
      {:error, :too_large} -> json(conn, 413, %{ok: false, error: "too_large"})
    end
  end

  get "/metrics" do
    stats =
      try do
        Docserver.Convert.stats()
      catch
        :exit, _ -> %{}
      end

    body =
      Enum.map_join(stats, "", fn
        {:docx_ms, v} -> "papir_convert_docx_seconds_total #{Float.round(v / 1000, 3)}\n"
        {k, v} -> "papir_convert_total{result=\"#{k}\"} #{v}\n"
      end)

    conn |> put_resp_content_type("text/plain") |> send_resp(200, body)
  end

  match _ do
    send_resp(conn, 404, "not found")
  end

  defp version(conn) do
    case get_req_header(conn, "x-papir-version") do
      [v | _] -> String.to_integer(v)
      _ -> 1
    end
  rescue
    _ -> 1
  end

  defp read_all(conn, acc \\ []) do
    case Plug.Conn.read_body(conn, length: 8_000_000, read_timeout: 30_000) do
      {:ok, data, conn} ->
        body = IO.iodata_to_binary([acc, data])
        if byte_size(body) > @max_body, do: {:error, :too_large}, else: {:ok, body, conn}

      {:more, data, conn} ->
        if IO.iodata_length([acc, data]) > @max_body, do: {:error, :too_large}, else: read_all(conn, [acc, data])

      {:error, _} ->
        {:error, :too_large}
    end
  end

  defp json(conn, status, data) do
    conn |> put_resp_content_type("application/json") |> send_resp(status, Jason.encode!(data))
  end

  defp security(conn, _navigate_to \\ nil) do
    fa = Enum.join(Application.get_env(:docserver, :frame_ancestors, ["'self'"]), " ")

    conn
    |> put_resp_header("content-security-policy",
      "default-src 'self'; script-src 'self' 'wasm-unsafe-eval'; style-src 'self' 'unsafe-inline'; img-src 'self' blob: data:; font-src 'self' blob: data:; connect-src 'self' #{ws_origin()}; frame-ancestors #{fa}")
    |> put_resp_header("referrer-policy", "no-referrer")
    |> put_resp_header("cache-control", "no-store")
    |> put_resp_header("x-content-type-options", "nosniff")
  end

  defp ws_origin do
    %URI{scheme: sc, host: h, port: p} = URI.parse(Application.fetch_env!(:docserver, :public_url))
    "#{String.replace(sc, "http", "ws")}://#{h}:#{p}"
  end

  defp editor_page(conn, s) do
    base = Application.fetch_env!(:docserver, :public_url)
    info = s.info

    cfg = %{
      session: s.id,
      api: base <> "/api",
      fileName: info["BaseFileName"] || "документ.docx",
      canWrite: s.can_write,
      user: info["UserFriendlyName"],
      owner: info["OwnerId"],
      fontsUrl: base <> "/fonts/",
      draftInterval: Application.get_env(:docserver, :draft_interval, 10),
      saveOnExit: Application.get_env(:docserver, :save_on_exit, false),
      review: Application.get_env(:docserver, :review, false),
      ws: String.replace(base, ~r/^http/, "ws") <> "/api/ws",
      dictUrl: base <> "/dict/",
      collab: Application.get_env(:docserver, :collab, true)
    }

    json_cfg = cfg |> Jason.encode!() |> String.replace("</", "<\\/")
    html =
      File.read!(Path.join(web_dir(), "index.html"))
      |> String.replace("<head>", ~s(<head>\n<base href="#{base}/editor/">), global: false)
      |> String.replace("<body>", ~s(<body class="#{body_class(cfg)}">), global: false)
      |> String.replace("</body>", ~s(<script type="application/json" id="papir-config">#{json_cfg}</script>\n</body>), global: false)

    conn |> security() |> put_resp_content_type("text/html") |> send_resp(200, html)
  end

  defp sheet_page(conn, s) do
    base = Application.fetch_env!(:docserver, :public_url)
    info = s.info

    edit =
      case Docserver.Discovery.collabora_edit("xlsx") do
        nil -> nil
        u -> u <> "lang=uk&WOPISrc=" <> URI.encode_www_form(s.src) <> "&access_token=" <> URI.encode_www_form(s.token)
      end

    cfg = %{
      session: s.id,
      api: base <> "/api",
      fileName: info["BaseFileName"] || "таблиця.xlsx",
      canWrite: s.can_write,
      user: info["UserFriendlyName"],
      fontsUrl: base <> "/fonts/",
      editUrl: edit
    }

    json_cfg = cfg |> Jason.encode!() |> String.replace("</", "<\\/")
    html =
      File.read!(Path.join(web_dir(), "sheet.html"))
      |> String.replace("<head>", ~s(<head>\n<base href="#{base}/editor/">), global: false)
      |> String.replace("</body>", ~s(<script type="application/json" id="papir-config">#{json_cfg}</script>\n</body>), global: false)

    conn |> security(edit) |> put_resp_content_type("text/html") |> send_resp(200, html)
  end

  defp error_page(conn, status, msg) do
    html = ~s(<!doctype html><html lang="uk"><meta charset="utf-8"><title>Папір</title><body style="font:15px system-ui;display:flex;align-items:center;justify-content:center;height:100vh;margin:0;color:#444">#{Plug.HTML.html_escape(msg)}</body></html>)
    conn |> security() |> put_resp_content_type("text/html") |> send_resp(status, html)
  end
end
