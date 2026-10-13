defmodule Docserver.TestHost do
  @moduledoc """
  A WOPI host for dev and tests. It keeps every save as a new version:

    * CheckFileInfo: BaseFileName, Size, UserCanWrite, UserId,
      UserFriendlyName, OwnerId; `POST /files/:id` (locks) answers the same JSON;
    * PutFile without `X-COOL-WOPI-IsModifiedByUser: true` saves nothing;
    * PutFile creates a new id and drops the old record; a PutFile to a
      dropped id answers 200 and saves nothing;
    * after a save the page replaces the iframe with the new id.

  Tokens: `edit` (may write) and `view`.
  """
  use Plug.Router

  plug :match
  plug :dispatch

  @table :test_host

  @doc "Child spec: a process that owns the file table."
  def child_spec(_) do
    %{id: __MODULE__.Owner, start: {Agent, :start_link, [fn -> init_table() end, [name: __MODULE__.Owner]]}}
  end

  def init_table do
    :ets.new(@table, [:named_table, :public, :set])
    seed()
    :ok
  end

  defp table, do: @table

  defp seed do
    dir = Application.get_env(:docserver, :test_host_seed)

    if dir && File.dir?(dir) do
      for f <- File.ls!(dir), String.ends_with?(f, ".docx"), data = File.read!(Path.join(dir, f)), byte_size(data) > 0 do
        add(f, data)
      end
    end
  end

  def add(name, data) do
    id = "F" <> Base.encode16(:crypto.strong_rand_bytes(6))
    :ets.insert(@table, {id, %{name: name, data: data, live: true, next: nil, versions: 1}})
    id
  end

  def file(id) do
    case :ets.lookup(table(), id) do
      [{^id, f}] -> f
      _ -> nil
    end
  end

  def latest(id) do
    case file(id) do
      %{next: nil} -> id
      %{next: n} -> latest(n)
      nil -> nil
    end
  end

  defp token_ok(conn) do
    conn = fetch_query_params(conn)
    # "edit" or "view", optionally with a user name: "edit~Олена"
    case String.split(conn.query_params["access_token"] || "", "~", parts: 2) do
      [t | _] = parts when t in ["edit", "view"] -> {:ok, conn, {t, Enum.at(parts, 1) || "Тестовий Користувач"}}
      _ -> :error
    end
  end

  defp info(conn, id) do
    with {:ok, conn, {t, user}} <- token_ok(conn), %{} = f <- file(id) do
      body = Jason.encode!(%{BaseFileName: f.name, Size: byte_size(f.data), UserCanWrite: t == "edit", UserId: 7, UserFriendlyName: user, OwnerId: "proc-1"})
      conn |> put_resp_content_type("application/json") |> send_resp(200, body)
    else
      nil -> send_resp(conn, 500, "no session record")
      :error -> send_resp(conn, 401, "Unauthorized")
    end
  end

  get "/bin/wopi/files/:id" do
    info(conn, id)
  end

  post "/bin/wopi/files/:id" do
    info(conn, id)
  end

  get "/bin/wopi/files/:id/contents" do
    with {:ok, conn, _} <- token_ok(conn), %{} = f <- file(id) do
      send_resp(conn, 200, f.data)
    else
      _ -> send_resp(conn, 401, "Unauthorized")
    end
  end

  post "/bin/wopi/files/:id/contents" do
    with {:ok, conn, _t} <- token_ok(conn) do
      modified = get_req_header(conn, "x-cool-wopi-ismodifiedbyuser") == ["true"]

      case file(id) do
        %{live: true} = f when modified ->
          {:ok, body, conn} = read_body(conn, length: 100_000_000)
          new = "F" <> Base.encode16(:crypto.strong_rand_bytes(6))
          :ets.insert(@table, {new, %{f | data: body, next: nil, versions: f.versions + 1}})
          :ets.insert(@table, {id, %{f | live: false, next: new}})
          send_resp(conn, 200, ~s({"result":"OK"}))

        _ ->
          send_resp(conn, 200, ~s({"result":"OK"}))
      end
    else
      _ -> send_resp(conn, 401, "Unauthorized")
    end
  end

  get "/latest/:id" do
    send_resp(conn, 200, latest(id) || "")
  end

  post "/upload" do
    conn = fetch_query_params(conn)
    {:ok, body, conn} = read_body(conn, length: 100_000_000)
    id = add(conn.query_params["name"] || "документ.docx", body)
    send_resp(conn, 200, id)
  end

  get "/" do
    rows =
      :ets.tab2list(table())
      |> Enum.filter(fn {_, f} -> f.live end)
      |> Enum.sort_by(fn {_, f} -> f.name end)
      |> Enum.map(fn {id, f} ->
        ~s(<li>#{Plug.HTML.html_escape(f.name)} <small>v#{f.versions}</small> — <a href="/doc/#{id}?mode=edit">редагувати</a> · <a href="/doc/#{id}?mode=view">переглянути</a></li>)
      end)

    html(conn, """
    <h1>Тестовий WOPI-хост</h1>
    <p>Документи для перевірки редактора. Кожне збереження — нова версія з новим id, сторінка підміняє iframe.</p>
    <p><input type="file" id="f" accept=".docx"> <button onclick="up()">Додати</button></p>
    <ul>#{Enum.join(rows)}</ul>
    <script>
    async function up(){const f=document.getElementById('f').files[0]; if(!f) return;
      await fetch('/upload?name='+encodeURIComponent(f.name),{method:'POST',body:f}); location.reload();}
    </script>
    """)
  end

  get "/doc/:id" do
    conn = fetch_query_params(conn)
    mode = if conn.query_params["mode"] == "view", do: "view", else: "edit"
    token = if u = conn.query_params["user"], do: mode <> "~" <> u, else: mode
    self_url = "http://127.0.0.1:#{Application.get_env(:docserver, :test_host_port)}"
    # the address from discovery by the file's extension
    ext = (file(id) && file(id).name |> Path.extname() |> String.trim_leading(".") |> String.downcase()) || "docx"
    urlsrc = Docserver.Discovery.ours(if ext == "xlsx", do: "xlsx", else: "docx")

    html(conn, """
    <div style="display:flex;gap:16px;align-items:center;padding:6px 10px;background:#eef;font:13px system-ui">
      <a href="/">← Документи</a><span id="ver"></span><span style="margin-left:auto">Режим: #{mode}</span>
    </div>
    <div id="slot" style="position:absolute;top:32px;left:0;right:0;bottom:0"></div>
    <script>
    let id = #{Jason.encode!(id)};
    const urlsrc = #{Jason.encode!(urlsrc)}, host = #{Jason.encode!(self_url)}, token = #{Jason.encode!(token)};
    function show() {
      const src = urlsrc + 'lang=uk&ui_defaults=UIMode=tabbed;TextRuler=true;TextSidebar=false&WOPISrc=' +
        encodeURIComponent(host + '/bin/wopi/files/' + id) + '&access_token=' + encodeURIComponent(token);
      document.getElementById('slot').innerHTML = '<iframe style="width:100%;height:100%;border:0" src="' + src + '"></iframe>';
      document.getElementById('ver').textContent = 'файл ' + id;
    }
    show();
    // a saved version replaces the iframe
    setInterval(async () => {
      const r = await fetch('/latest/' + id); const n = await r.text();
      if (n && n !== id) { id = n; show(); }
    }, 1000);
    </script>
    """)
  end

  match _ do
    send_resp(conn, 404, "not found")
  end

  defp html(conn, body) do
    conn
    |> put_resp_content_type("text/html")
    |> send_resp(200, ~s|<!doctype html><html lang="uk"><meta charset="utf-8"><title>Тестовий WOPI-хост</title><body style="margin:0;font:14px system-ui">#{body}</body></html>|)
  end
end
