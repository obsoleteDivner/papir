defmodule Docserver.Convert do
  @moduledoc """
  PDF conversion pool. Other Erlang nodes call

      GenServer.call({Lop.Pool, node}, {:convert, in_path, out_dir, opts}, timeout)

  and get `{:ok, Path.join(out_dir, rootname(in_path) <> ".pdf")}` or
  `{:error, reason}`. `.docx` goes to `docconv` workers (the Papir core, no
  LibreOffice); other formats go to `soffice` when it is configured, else
  `{:error, :unsupported}`.

  Workers are separate OS processes: a crash or a hang kills one worker, the
  pool starts a new one and the other jobs go on.
  """

  @doc "Converts `in_path` into `out_dir`."
  def convert(in_path, out_dir, opts \\ []) do
    timeout = Keyword.get(opts, :call_timeout, 60_000)
    GenServer.call(Lop.Pool, {:convert, in_path, out_dir, opts}, timeout)
  catch
    :exit, reason -> {:error, {:unavailable, reason}}
  end

  def stats, do: GenServer.call(Lop.Pool, :stats)

  def config, do: Application.get_env(:docserver, :converter, [])
end

defmodule Docserver.Convert.Worker do
  @moduledoc "One `docconv serve` process; one job at a time."
  use GenServer
  require Logger

  def start_link(pool), do: GenServer.start_link(__MODULE__, pool)

  @impl true
  def init(pool) do
    Process.flag(:trap_exit, true)
    {:ok, %{pool: pool, port: open(), job: nil, timer: nil, buf: ""}}
  end

  defp open do
    cfg = Docserver.Convert.config()
    bin = Keyword.fetch!(cfg, :bin)
    Port.open({:spawn_executable, bin}, [:binary, :exit_status, :use_stdio, {:line, 1_000_000}, args: ["serve", "--fonts", Keyword.fetch!(cfg, :fonts)]])
  end

  @impl true
  def handle_cast({:job, ref, in_path, out}, st) do
    line = Jason.encode!(%{id: 1, in: in_path, out: out})
    Port.command(st.port, line <> "\n")
    timeout = Keyword.get(Docserver.Convert.config(), :timeout, 30_000)
    timer = Process.send_after(self(), {:timeout, ref}, timeout)
    {:noreply, %{st | job: ref, timer: timer}}
  end

  @impl true
  def handle_info({port, {:data, {:eol, chunk}}}, %{port: port} = st) do
    line = st.buf <> chunk
    st = %{st | buf: ""}
    result =
      case Jason.decode(line) do
        {:ok, %{"ok" => true} = r} -> {:ok, r["ms"]}
        {:ok, %{"error" => e}} -> {:error, e}
        _ -> {:error, "bad_reply"}
      end

    {:noreply, finish(st, result)}
  end

  def handle_info({port, {:data, {:noeol, chunk}}}, %{port: port} = st) do
    {:noreply, %{st | buf: st.buf <> chunk}}
  end

  def handle_info({port, {:exit_status, code}}, %{port: port} = st) do
    Logger.warning("docconv worker exited with #{code}, restarting")
    st = finish(st, {:error, :crash})
    {:noreply, %{st | port: open(), buf: ""}}
  end

  def handle_info({:timeout, ref}, %{job: ref} = st) do
    # a hung conversion: kill this worker only
    kill(st.port)
    st = finish(%{st | timer: nil}, {:error, :timeout})
    {:noreply, %{st | port: open(), buf: ""}}
  end

  def handle_info({:EXIT, _, _}, st), do: {:noreply, st}
  def handle_info(_, st), do: {:noreply, st}

  @impl true
  def terminate(_, st), do: kill(st.port)

  defp kill(port) do
    case Port.info(port, :os_pid) do
      {:os_pid, pid} -> System.cmd("kill", ["-9", to_string(pid)], stderr_to_stdout: true)
      _ -> :ok
    end

    try do
      Port.close(port)
    rescue
      _ -> :ok
    end
  end

  defp finish(%{job: nil} = st, _), do: st

  defp finish(st, result) do
    if st.timer, do: Process.cancel_timer(st.timer)
    send(st.pool, {:done, self(), st.job, result})
    %{st | job: nil, timer: nil}
  end
end

defmodule Docserver.Convert.Pool do
  @moduledoc """
  Registered as `Lop.Pool`. Free workers take jobs at once; a job waits only
  when every worker is busy (each docx takes milliseconds).
  """
  use GenServer
  require Logger

  def start_link(_), do: GenServer.start_link(__MODULE__, nil, name: Lop.Pool)

  @impl true
  def init(_) do
    cfg = Docserver.Convert.config()
    n = Keyword.get(cfg, :workers) || System.schedulers_online()
    workers = for _ <- 1..n, do: elem(Docserver.Convert.Worker.start_link(self()), 1)

    {:ok,
     %{
       free: workers,
       wait: :queue.new(),
       running: %{},
       soffice_free: Enum.to_list(1..Keyword.get(cfg, :soffice_jobs, 4)),
       soffice_wait: :queue.new(),
       stats: %{docx_ok: 0, docx_error: 0, soffice_ok: 0, soffice_error: 0, unsupported: 0, docx_ms: 0.0}
     }}
  end

  @impl true
  def handle_call(:stats, _from, st), do: {:reply, st.stats, st}

  def handle_call({:convert, in_path, out_dir, opts}, from, st) do
    format = Keyword.get(opts, :format, "pdf")
    out = Path.join(out_dir, Path.rootname(Path.basename(in_path)) <> "." <> format)
    type = (Keyword.get(opts, :in_type) || Path.extname(in_path) |> String.trim_leading(".")) |> to_string() |> String.downcase()
    job = %{in: in_path, out: out, out_dir: out_dir, type: type, from: from, started: System.monotonic_time(:millisecond)}

    cond do
      type == "docx" and format == "pdf" -> {:noreply, dispatch(job, st)}
      soffice() != nil -> {:noreply, soffice_dispatch(job, st)}
      true -> {:reply, {:error, :unsupported}, bump(st, :unsupported)}
    end
  end

  @impl true
  def handle_info({:done, worker, ref, result}, st) do
    {job, running} = Map.pop(st.running, ref)
    st = %{st | running: running}

    st =
      case {job, result} do
        {nil, _} ->
          st

        {job, {:ok, ms}} ->
          GenServer.reply(job.from, {:ok, job.out})
          st |> bump(:docx_ok) |> update_in([:stats, :docx_ms], &(&1 + (ms || 0)))

        {job, {:error, reason}} ->
          Logger.warning("docconv failed: #{inspect(reason)}")
          st = bump(st, :docx_error)
          # a file the core cannot read goes to LibreOffice when there is one
          if soffice() != nil and reason not in ["zip_bomb"] do
            soffice_dispatch(job, st)
          else
            GenServer.reply(job.from, {:error, {:docx, reason}})
            st
          end
      end

    {:noreply, release(worker, st)}
  end

  def handle_info({:soffice_done, slot, job, result}, st) do
    GenServer.reply(job.from, result)
    st = bump(st, if(match?({:ok, _}, result), do: :soffice_ok, else: :soffice_error))

    st =
      case :queue.out(st.soffice_wait) do
        {{:value, next}, rest} -> run_soffice(next, slot, %{st | soffice_wait: rest})
        {:empty, _} -> %{st | soffice_free: [slot | st.soffice_free]}
      end

    {:noreply, st}
  end

  def handle_info(_, st), do: {:noreply, st}

  defp bump(st, key), do: update_in(st, [:stats, key], &(&1 + 1))

  defp dispatch(job, %{free: [w | rest]} = st) do
    ref = make_ref()
    GenServer.cast(w, {:job, ref, job.in, job.out})
    %{st | free: rest, running: Map.put(st.running, ref, job)}
  end

  defp dispatch(job, st), do: %{st | wait: :queue.in(job, st.wait)}

  defp release(worker, st) do
    case :queue.out(st.wait) do
      {{:value, job}, rest} -> dispatch(job, %{st | wait: rest, free: [worker | st.free]})
      {:empty, _} -> %{st | free: [worker | st.free]}
    end
  end

  defp soffice, do: Keyword.get(Docserver.Convert.config(), :soffice)

  # the import filter is forced by the type, nothing is guessed
  @infilters %{
    "docx" => "MS Word 2007 XML",
    "doc" => "MS Word 97",
    "odt" => "writer8",
    "rtf" => "Rich Text Format",
    "xlsx" => "Calc MS Excel 2007 XML",
    "xls" => "MS Excel 97",
    "pptx" => "Impress MS PowerPoint 2007 XML",
    "ppt" => "MS PowerPoint 97",
    "txt" => "Text"
  }

  defp soffice_dispatch(job, %{soffice_free: [slot | rest]} = st), do: run_soffice(job, slot, %{st | soffice_free: rest})
  defp soffice_dispatch(job, st), do: %{st | soffice_wait: :queue.in(job, st.soffice_wait)}

  # one soffice run per file, a persistent profile per slot
  defp run_soffice(job, slot, st) do
    pool = self()
    cfg = Docserver.Convert.config()

    Task.start(fn ->
      tmp = Path.join(System.tmp_dir!(), "papir-soffice-#{System.unique_integer([:positive])}")
      File.mkdir_p!(tmp)
      ext = if job.type in ["", nil], do: "", else: "." <> job.type
      src = Path.join(tmp, Path.rootname(Path.basename(job.in)) <> ext)
      profile = Path.join(Keyword.get(cfg, :profiles, Path.join(System.tmp_dir!(), "papir-soffice-profiles")), "job_#{slot}")

      result =
        try do
          File.cp!(job.in, src)
          filter = if f = @infilters[job.type], do: ["--infilter=" <> f], else: []
          args = ["-env:UserInstallation=file://" <> profile, "--headless", "--invisible", "--nologo", "--nodefault", "--nofirststartwizard", "--nolockcheck", "--convert-to", "pdf"] ++ filter ++ ["--outdir", tmp, src]
          task = Task.async(fn -> System.cmd(soffice(), args, stderr_to_stdout: true) end)

          case Task.yield(task, Keyword.get(cfg, :timeout, 30_000)) || Task.shutdown(task, :brutal_kill) do
            {:ok, {_, 0}} ->
              made = Path.join(tmp, Path.rootname(Path.basename(src)) <> ".pdf")
              if File.exists?(made), do: (File.cp!(made, job.out) && {:ok, job.out}) || {:ok, job.out}, else: {:error, :no_output}

            {:ok, {_, code}} ->
              {:error, {:exit, code}}

            nil ->
              {:error, :timeout}
          end
        rescue
          e -> {:error, {:crash, Exception.message(e)}}
        after
          File.rm_rf(tmp)
        end

      send(pool, {:soffice_done, slot, job, result})
    end)

    st
  end
end
