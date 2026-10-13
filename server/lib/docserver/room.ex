defmodule Docserver.Room do
  @moduledoc """
  One document edited by several people (one process per WOPISrc).

  The room puts the changes of all participants into one order: a change
  made on top of the latest version is accepted, numbered, written to the
  journal (before the author hears "ack") and sent to everybody else; a
  change made on an older version is refused and its author rebases it on
  the changes it has not seen yet. Cursors are passed around and not kept.

  The journal (base document + changes) survives a restart of the server.
  Only an explicit save writes to the host: when the last person who may
  edit has gone, the changes are dropped, unless `save_on_exit` is on;
  then the room builds the document from the journal (`docconv apply`, the
  same core as in the browser) and writes it to the host once.

  Viewers see the saved document: they are in the list of people, but get
  neither the changes nor the cursors, and they do not keep the room open.
  """
  use GenServer, restart: :transient
  require Logger
  alias Docserver.{Sessions, Wopi}

  @colors ~w(#e8590c #2f9e44 #1971c2 #c2255c #6741d9 #0c8599 #e67700 #5c940d)
  # seconds the room waits after the last participant (page reloads, short drops)
  defp grace, do: Application.get_env(:docserver, :room_grace, 20)

  # ---- API ----

  def via(src), do: {:via, Registry, {Docserver.Rooms, src}}

  @doc "The room of `src`, started if needed."
  def ensure(src, session) do
    case Registry.lookup(Docserver.Rooms, src) do
      [{pid, _}] ->
        {:ok, pid}

      [] ->
        case DynamicSupervisor.start_child(Docserver.RoomSup, {__MODULE__, {src, session}}) do
          {:ok, pid} -> {:ok, pid}
          {:error, {:already_started, pid}} -> {:ok, pid}
          err -> err
        end
    end
  end

  def lookup(src) do
    case Registry.lookup(Docserver.Rooms, src) do
      [{pid, _}] -> {:ok, pid}
      _ -> :error
    end
  end

  def join(room, session, have), do: GenServer.call(room, {:join, self(), session, have}, 30_000)
  def push(room, v, change), do: GenServer.call(room, {:push, self(), v, change}, 30_000)
  def cursor(room, msg), do: GenServer.cast(room, {:cursor, self(), msg})
  def leave(room), do: GenServer.cast(room, {:leave, self()})
  def base(room), do: GenServer.call(room, :base, 30_000)
  def info(room), do: GenServer.call(room, :info)

  @doc "Explicit save by a writer whose copy is at the room's head."
  def save(room, session, body, version), do: GenServer.call(room, {:save, session, body, version}, 90_000)

  def start_link({src, session}), do: GenServer.start_link(__MODULE__, {src, session}, name: via(src))

  def dir(src) do
    root = Application.get_env(:docserver, :rooms_dir) || Path.join(System.tmp_dir!(), "papir-rooms")
    Path.join(root, Base.url_encode64(:crypto.hash(:sha256, src), padding: false))
  end

  # ---- server ----

  @impl true
  def init({src, session}) do
    Process.flag(:trap_exit, true)
    dir = dir(src)
    File.mkdir_p!(dir)

    with {:ok, base} <- load_base(dir, src, session) do
      log = load_log(dir)
      head = case log do
        [{v, _, _} | _] -> v
        [] -> 0
      end

      next_no = log |> Enum.map(fn {_, by, _} -> no_of(by) end) |> Enum.max(fn -> 0 end)
      if log != [], do: Logger.info("room: recovered #{length(log)} changes for #{Wopi.redact(src)}")

      {:ok,
       %{src: src, dir: dir, base: base, log: log, head: head, people: %{}, next_no: next_no + 1, epoch: epoch(),
         writer: if(session.can_write, do: session.id), saved: false, close_timer: nil, journal: File.open!(Path.join(dir, "log.jsonl"), [:append, :binary])}}
    else
      {:error, e} -> {:stop, {:base, e}}
    end
  end

  defp epoch, do: Base.url_encode64(:crypto.strong_rand_bytes(6), padding: false)

  defp save_on_exit?, do: Application.get_env(:docserver, :save_on_exit, false)

  defp no_of("c" <> n), do: String.to_integer(n)
  defp no_of(_), do: 0

  defp load_base(dir, src, session) do
    path = Path.join(dir, "base.docx")

    case File.read(path) do
      {:ok, b} ->
        {:ok, b}

      _ ->
        case (save_on_exit?() && session.draft && {:ok, session.draft}) || Wopi.get_file(src, session.token) do
          {:ok, b} ->
            File.write!(path <> ".tmp", b)
            File.rename!(path <> ".tmp", path)
            {:ok, b}

          err ->
            err
        end
    end
  end

  # newest first
  defp load_log(dir) do
    case File.read(Path.join(dir, "log.jsonl")) do
      {:ok, text} ->
        text
        |> String.split("\n", trim: true)
        |> Enum.flat_map(fn l ->
          case Jason.decode(l) do
            {:ok, %{"v" => v, "by" => by, "c" => c}} -> [{v, by, c}]
            _ -> []
          end
        end)
        |> Enum.reverse()

      _ ->
        []
    end
  end

  defp person(p), do: %{id: p.id, name: p.name, color: p.color, write: p.can_write}
  defp people(st), do: st.people |> Map.values() |> Enum.sort_by(& &1.no) |> Enum.map(&person/1)

  defp broadcast(st, msg, except \\ nil) do
    for {pid, _} <- st.people, pid != except, do: send(pid, {:room, msg})
    :ok
  end

  # changes and cursors: to those who edit, viewers see the saved document
  defp broadcast_editors(st, msg, except) do
    for {pid, p} <- st.people, pid != except, p.can_write, do: send(pid, {:room, msg})
    :ok
  end

  defp editors?(st), do: Enum.any?(st.people, fn {_, p} -> p.can_write end)

  # nobody left who may edit: close after a while (page reloads, short drops)
  defp close_later(st) do
    if editors?(st) or st.close_timer, do: st, else: %{st | close_timer: Process.send_after(self(), :close, grace() * 1000)}
  end

  @impl true
  def handle_call({:join, pid, session, have}, _from, st) do
    no = st.next_no
    p = %{id: "c#{no}", no: no, name: session.info["UserFriendlyName"] || "Користувач", color: Enum.at(@colors, rem(no - 1, length(@colors))),
          can_write: session.can_write and not st.saved, session: session.id, ref: Process.monitor(pid)}

    st =
      if p.can_write and is_reference(st.close_timer) do
        Process.cancel_timer(st.close_timer)
        %{st | close_timer: nil}
      else
        st
      end

    st = %{st | people: Map.put(st.people, pid, p), next_no: no + 1}
    st = if session.can_write, do: %{st | writer: session.id}, else: st
    since = if is_integer(have), do: have, else: 0

    changes =
      if p.can_write,
        do: st.log |> Enum.take_while(fn {v, _, _} -> v > since end) |> Enum.reverse() |> Enum.map(fn {v, by, c} -> [v, by, c] end),
        else: []

    broadcast(st, %{t: "people", people: people(st)}, pid)
    reply = %{t: "welcome", me: person(p) |> Map.put(:no, no), version: st.head, epoch: st.epoch, changes: changes, people: people(st), saved: st.saved}
    {:reply, {:ok, reply}, st}
  end

  def handle_call({:push, pid, v, change}, _from, st) do
    p = st.people[pid]

    cond do
      p == nil -> {:reply, {:error, "not_joined"}, st}
      not p.can_write -> {:reply, {:error, "read_only"}, st}
      st.saved -> {:reply, {:error, "saved"}, st}
      v != st.head -> {:reply, {:nack, st.head}, st}
      true ->
        n = st.head + 1
        # in the journal before anybody hears of it
        line = Jason.encode!(%{v: n, by: p.id, c: change})
        :ok = IO.binwrite(st.journal, line <> "\n")
        :ok = :file.datasync(st.journal)
        broadcast_editors(st, %{t: "change", v: n, by: p.id, c: change}, pid)
        {:reply, {:ack, n}, %{st | head: n, log: [{n, p.id, change} | st.log]}}
    end
  end

  def handle_call(:base, _from, st), do: {:reply, {:ok, st.base, st.head}, st}

  def handle_call(:info, _from, st), do: {:reply, %{head: st.head, people: people(st), saved: st.saved}, st}

  def handle_call({:save, session, body, version}, from, st) do
    cond do
      st.saved -> {:reply, {:error, :superseded}, st}
      version != st.head -> {:reply, {:error, :not_synced}, st}
      true ->
        me = self()
        name = Enum.find_value(st.people, "", fn {_, p} -> p.session == session && p.name end)

        Task.start(fn ->
          res = Sessions.save(session, body, version, :manual)
          send(me, {:saved, from, res, name})
        end)

        {:noreply, st}
    end
  end

  @impl true
  def handle_cast({:cursor, pid, msg}, st) do
    case st.people[pid] do
      %{can_write: true} = p -> broadcast_editors(st, Map.merge(msg, %{"t" => "cursor", "id" => p.id}), pid)
      _ -> :ok
    end

    {:noreply, st}
  end

  # the page is closed (not a dropped connection): no waiting
  def handle_cast({:leave, pid}, st) do
    case st.people[pid] do
      nil ->
        {:noreply, st}

      p ->
        Process.demonitor(p.ref, [:flush])
        st = %{st | people: Map.delete(st.people, pid)}
        broadcast(st, %{t: "people", people: people(st)})

        if editors?(st) do
          {:noreply, st}
        else
          if is_reference(st.close_timer), do: Process.cancel_timer(st.close_timer)
          send(self(), :close)
          {:noreply, %{st | close_timer: :now}}
        end
    end
  end

  @impl true
  def handle_info({:saved, from, res, name}, st) do
    GenServer.reply(from, res)

    case res do
      :ok ->
        # the host made a new version; this one is closed for edits
        broadcast(st, %{t: "saved", by: name})
        finish(st)
        {:noreply, %{st | saved: true}}

      _ ->
        {:noreply, st}
    end
  end

  def handle_info({:DOWN, ref, :process, pid, _}, st) do
    case st.people[pid] do
      %{ref: ^ref} ->
        st = %{st | people: Map.delete(st.people, pid)}
        broadcast(st, %{t: "people", people: people(st)})
        {:noreply, close_later(st)}

      _ ->
        {:noreply, st}
    end
  end

  def handle_info(:close, st) do
    st = %{st | close_timer: nil}

    cond do
      editors?(st) ->
        {:noreply, st}

      st.saved ->
        finish(st)
        {:stop, :normal, st}

      save_on_exit?() and st.head > 0 ->
        case exit_save(st) do
          :ok ->
            finish(st)
            {:stop, :normal, st}

          err ->
            # keep the journal; try again later
            Logger.warning("room: exit save failed for #{Wopi.redact(st.src)}: #{inspect(err)}")
            {:noreply, %{st | close_timer: Process.send_after(self(), :close, 60_000)}}
        end

      map_size(st.people) == 0 ->
        if st.head > 0, do: Logger.info("room: closed without saving, #{st.head} changes dropped, #{Wopi.redact(st.src)}")
        finish(st)
        {:stop, :normal, st}

      true ->
        # only viewers left: the unsaved changes go, the room stays for them
        if st.head > 0, do: Logger.info("room: closed without saving, #{st.head} changes dropped, #{Wopi.redact(st.src)}")
        {:noreply, drop_changes(st)}
    end
  end

  def handle_info(_, st), do: {:noreply, st}

  @impl true
  def terminate(_, st), do: File.close(st.journal)

  defp drop_changes(%{head: 0} = st), do: st

  defp drop_changes(st) do
    File.close(st.journal)
    path = Path.join(st.dir, "log.jsonl")
    File.write!(path, "")
    %{st | log: [], head: 0, epoch: epoch(), journal: File.open!(path, [:append, :binary])}
  end

  defp finish(st) do
    File.close(st.journal)
    File.rm_rf(st.dir)
  end

  @doc "The document after all changes (base + journal), built by the core."
  def materialize(dir) do
    bin = Keyword.fetch!(Docserver.Convert.config(), :bin)
    out = Path.join(dir, "result.docx")

    case System.cmd(bin, ["apply", Path.join(dir, "base.docx"), Path.join(dir, "log.jsonl"), "-o", out], stderr_to_stdout: true) do
      {_, 0} -> File.read(out)
      {msg, code} -> {:error, {code, String.slice(msg, 0, 500)}}
    end
  end

  defp exit_save(st) do
    with {:ok, s} <- (st.writer && Sessions.get(st.writer)) || {:error, :no_writer},
         {:ok, body} <- materialize(st.dir),
         :ok <- Wopi.put_file(s.src, s.token, body, :exit) do
      Sessions.mark_saved(st.writer)
      Logger.info("room: exit save, #{st.head} changes, #{Wopi.redact(st.src)}")
      :ok
    end
  end
end
