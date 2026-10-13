defmodule Docserver.Socket do
  @moduledoc """
  WebSocket of the editor (`/api/ws`): JSON messages between one browser
  and the room of its document.

      → {"t":"join","session":id,"have":version}   ← {"t":"welcome",...}
      → {"t":"push","v":base,"c":change}            ← {"t":"ack","v":n} | {"t":"nack","v":head}
      → {"t":"cursor",...}                           ← changes, cursors and people of the others
  """
  @behaviour :cowboy_websocket
  alias Docserver.{Room, Sessions}

  @impl true
  def init(req, _opts) do
    {:cowboy_websocket, req, %{room: nil, session: nil}, %{idle_timeout: 90_000, max_frame_size: 64_000_000}}
  end

  @impl true
  def websocket_init(st), do: {[], st}

  @impl true
  def websocket_handle({:text, text}, st) do
    case Jason.decode(text) do
      {:ok, msg} -> handle(msg, st)
      _ -> reply(%{t: "error", error: "bad_json"}, st)
    end
  end

  def websocket_handle(_, st), do: {[], st}

  defp handle(%{"t" => "join", "session" => sid} = m, %{room: nil} = st) do
    with {:ok, s} <- Sessions.get(sid),
         {:ok, room} <- Room.ensure(s.src, s),
         {:ok, welcome} <- Room.join(room, s, m["have"]) do
      Sessions.touch(sid)
      reply(welcome, %{st | room: room, session: sid})
    else
      _ -> {[{:text, Jason.encode!(%{t: "error", error: "no_session"})}, :close], st}
    end
  end

  defp handle(%{"t" => "push", "v" => v, "c" => c}, %{room: room} = st) when room != nil do
    case Room.push(room, v, c) do
      {:ack, n} -> reply(%{t: "ack", v: n}, st)
      {:nack, head} -> reply(%{t: "nack", v: head}, st)
      {:error, e} -> reply(%{t: "nack", error: e}, st)
    end
  end

  defp handle(%{"t" => "cursor"} = m, %{room: room} = st) when room != nil do
    Room.cursor(room, Map.take(m, ["v", "a", "f", "story"]))
    {[], st}
  end

  defp handle(%{"t" => "leave"}, %{room: room} = st) when room != nil do
    Room.leave(room)
    {[], %{st | room: nil}}
  end

  defp handle(%{"t" => "ping"}, st) do
    if st.session, do: Sessions.touch(st.session)
    reply(%{t: "pong"}, st)
  end

  defp handle(_, st), do: reply(%{t: "error", error: "bad_message"}, st)

  defp reply(msg, st), do: {[{:text, Jason.encode!(msg)}], st}

  @impl true
  def websocket_info({:room, msg}, st), do: reply(msg, st)
  def websocket_info(_, st), do: {[], st}
end
