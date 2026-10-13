defmodule Docserver.Sessions do
  @moduledoc """
  Editor sessions: one per opened file id (WOPISrc).

  The browser uploads drafts and pings. A session that stops pinging while it
  holds unsaved edits is written to the host (exit save), so closing the tab
  does not lose work. After any PutFile the file id is spent (the host makes a
  new version with a new id), so the session is marked `superseded` and never
  writes again.
  """
  use GenServer
  require Logger
  alias Docserver.Wopi

  @table :docserver_sessions
  @max_age 12 * 3600

  def start_link(_), do: GenServer.start_link(__MODULE__, nil, name: __MODULE__)

  def get(id) do
    case :ets.lookup(@table, id) do
      [{^id, s}] -> {:ok, s}
      _ -> :error
    end
  end

  @doc "Creates a session; takes over unsaved edits of an older session of the same file."
  def create(src, token, info), do: GenServer.call(__MODULE__, {:create, src, token, info})

  def touch(id), do: GenServer.cast(__MODULE__, {:touch, id})

  def put_draft(id, body, version), do: GenServer.call(__MODULE__, {:draft, id, body, version})

  @doc "Writes `body` (or the latest draft) to the host."
  def save(id, body, version, reason), do: GenServer.call(__MODULE__, {:save, id, body, version, reason}, 60_000)

  def sweep, do: GenServer.call(__MODULE__, :sweep, 60_000)

  @doc "The host has the document (written by the room): the file id is spent."
  def mark_saved(id), do: GenServer.cast(__MODULE__, {:mark_saved, id})

  @impl true
  def init(_) do
    :ets.new(@table, [:named_table, :set, :protected, read_concurrency: true])
    :timer.send_interval(5_000, :sweep)
    {:ok, nil}
  end

  defp now, do: System.monotonic_time(:second)

  defp put(s), do: :ets.insert(@table, {s.id, s})

  @impl true
  def handle_call({:create, src, token, info}, _from, st) do
    id = Base.url_encode64(:crypto.strong_rand_bytes(18), padding: false)
    can_write = info["UserCanWrite"] == true

    s = %{
      id: id,
      src: src,
      token: token,
      info: info,
      can_write: can_write,
      created: now(),
      seen: now(),
      draft: nil,
      draft_version: 0,
      saved_version: 0,
      superseded: false
    }

    # unsaved edits of an earlier session of this very file
    older =
      :ets.select(@table, [{{:_, :"$1"}, [], [:"$1"]}])
      |> Enum.filter(&(&1.src == src and not &1.superseded and &1.draft != nil and &1.draft_version > &1.saved_version))

    s =
      Enum.reduce(older, s, fn o, acc ->
        if can_write do
          put(%{o | superseded: true})
          %{acc | draft: o.draft, draft_version: 1, saved_version: 0}
        else
          exit_save(o)
          acc
        end
      end)

    put(s)
    {:reply, {:ok, s}, st}
  end

  def handle_call({:draft, id, body, version}, _from, st) do
    reply =
      with {:ok, s} <- get(id) do
        cond do
          s.superseded -> {:error, :superseded}
          not s.can_write -> {:error, :read_only}
          version <= s.draft_version -> :ok
          true -> put(%{s | draft: body, draft_version: version, seen: now()}) && :ok
        end
      else
        _ -> {:error, :not_found}
      end

    {:reply, reply, st}
  end

  def handle_call({:save, id, body, version, reason}, from, st) do
    with {:ok, s} <- get(id) do
      cond do
        s.superseded ->
          {:reply, {:error, :superseded}, st}

        not s.can_write ->
          {:reply, {:error, :read_only}, st}

        true ->
          {body, version} = if body, do: {body, version}, else: {s.draft, s.draft_version}
          # the write runs outside this process; the session is spent meanwhile
          put(%{s | superseded: true, seen: now()})
          me = self()

          Task.start(fn ->
            res = Wopi.put_file(s.src, s.token, body, reason)
            send(me, {:saved, id, from, res, body, version})
          end)

          {:noreply, st}
      end
    else
      _ -> {:reply, {:error, :not_found}, st}
    end
  end

  def handle_call(:sweep, _from, st), do: {:reply, do_sweep(), st}

  @impl true
  def handle_cast({:mark_saved, id}, st) do
    with {:ok, s} <- get(id), do: put(%{s | superseded: true, draft: nil})
    {:noreply, st}
  end

  def handle_cast({:touch, id}, st) do
    with {:ok, s} <- get(id), do: put(%{s | seen: now()})
    {:noreply, st}
  end

  @impl true
  def handle_info({:saved, id, from, res, body, version}, st) do
    with {:ok, s} <- get(id) do
      case res do
        :ok ->
          put(%{s | saved_version: version, draft: nil})

        _ ->
          # keep the edits and let the session write again later
          put(%{s | superseded: false, draft: body, draft_version: max(version, s.draft_version)})
      end
    end

    if from, do: GenServer.reply(from, res)
    {:noreply, st}
  end

  def handle_info(:sweep, st) do
    do_sweep()
    {:noreply, st}
  end

  defp do_sweep do
    idle = Application.get_env(:docserver, :idle_save_after, 30)
    t = now()

    for {id, s} <- :ets.tab2list(@table) do
      cond do
        t - s.created > @max_age ->
          :ets.delete(@table, id)

        t - s.seen > idle ->
          if Application.get_env(:docserver, :save_on_exit, false) and not s.superseded and s.can_write and s.draft != nil and
               s.draft_version > s.saved_version do
            exit_save(s)
          end

          :ets.delete(@table, id)

        true ->
          :ok
      end
    end

    :ok
  end

  defp exit_save(s) do
    put(%{s | superseded: true})

    Task.start(fn ->
      case Wopi.put_file(s.src, s.token, s.draft, :exit) do
        :ok -> Logger.info("exit save: session #{s.id}")
        err -> Logger.warning("exit save failed: session #{s.id}: #{inspect(err)}")
      end
    end)
  end
end
