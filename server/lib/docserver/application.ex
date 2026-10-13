defmodule Docserver.Application do
  @moduledoc false
  use Application

  @impl true
  def start(_type, _args) do
    port = Application.fetch_env!(:docserver, :port)

    test_host =
      if Application.get_env(:docserver, :test_host) do
        [Docserver.TestHost, {Plug.Cowboy, scheme: :http, plug: Docserver.TestHost, options: [port: Application.get_env(:docserver, :test_host_port), ref: :test_host]}]
      else
        []
      end

    convert = if Keyword.get(Docserver.Convert.config(), :enabled, false), do: [Docserver.Convert.Pool], else: []

    children =
      convert ++
      [
        Docserver.Sessions,
        Docserver.Discovery,
        {Registry, keys: :unique, name: Docserver.Rooms},
        {DynamicSupervisor, name: Docserver.RoomSup, strategy: :one_for_one},
        {Plug.Cowboy,
         scheme: :http,
         plug: Docserver.Router,
         options: [
           port: port,
           ref: :docserver,
           protocol_options: [idle_timeout: 120_000],
           # the editor's WebSocket next to the HTTP routes
           dispatch: [{:_, [{"/api/ws", Docserver.Socket, []}, {:_, Plug.Cowboy.Handler, {Docserver.Router, []}}]}]
         ]}
      ] ++ test_host

    Supervisor.start_link(children, strategy: :one_for_one, name: Docserver.Supervisor)
  end
end
