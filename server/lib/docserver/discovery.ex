defmodule Docserver.Discovery do
  @moduledoc """
  `/hosting/discovery`: our editor for .docx, everything else passes
  through to Collabora's discovery (when configured).

  Hosts may read `<action ext name urlsrc>` only, take the last one per
  name+ext and append query parameters directly to `urlsrc`, so it ends
  with `?`.
  """
  use Agent
  require Logger

  @ttl 300

  def start_link(_), do: Agent.start_link(fn -> {0, nil} end, name: __MODULE__)

  def ours(ext), do: Application.fetch_env!(:docserver, :public_url) <> "/#{ext}?"

  def xml do
    now = System.monotonic_time(:second)

    case Agent.get(__MODULE__, & &1) do
      {t, x} when is_binary(x) and now - t < @ttl ->
        x

      _ ->
        x = build()
        Agent.update(__MODULE__, fn _ -> {now, x} end)
        x
    end
  end

  def build do
    case fetch_collabora() do
      {:ok, body} -> rewrite(body)
      _ -> minimal()
    end
  end

  defp fetch_collabora do
    case Application.get_env(:docserver, :collabora_discovery) do
      nil ->
        :none

      url ->
        opts = [timeout: 5_000, ssl: [verify: :verify_none]]

        case :httpc.request(:get, {String.to_charlist(url), []}, opts, body_format: :binary) do
          {:ok, {{_, 200, _}, _, body}} ->
            {:ok, body}

          other ->
            Logger.warning("collabora discovery unavailable: #{inspect(other, limit: 3)}")
            :error
        end
    end
  end

  @exts ["docx", "xlsx"]

  @doc "Collabora's own editing address for `ext` (the viewer's «Редагувати»), if known."
  def collabora_edit(ext) do
    _ = xml()
    :persistent_term.get({__MODULE__, :edit, ext}, nil)
  end

  @doc "Points our formats to this server, leaves the rest as is."
  def rewrite(body) do
    # spreadsheets are edited in Collabora: remember where
    for [tag] <- Regex.scan(~r/<action\b[^>]*\bext="xlsx"[^>]*>/, body),
        Regex.match?(~r/\bname="edit"/, tag),
        [_, u] <- [Regex.run(~r/\burlsrc="([^"]*)"/, tag)] do
      :persistent_term.put({__MODULE__, :edit, "xlsx"}, String.replace(u, "&amp;", "&"))
    end

    body =
      Regex.replace(~r/<action\b[^>]*>/, body, fn tag ->
        case Regex.run(~r/\bext="([^"]*)"/, tag) do
          [_, ext] when ext in @exts -> Regex.replace(~r/\burlsrc="[^"]*"/, tag, ~s(urlsrc="#{ours(ext)}"))
          _ -> tag
        end
      end)

    body = if String.contains?(body, ~s(ext="docx")), do: body, else: String.replace(body, "</net-zone>", writer_xml() <> "</net-zone>", global: false)
    if String.contains?(body, ~s(ext="xlsx")), do: body, else: String.replace(body, "</net-zone>", calc_xml() <> "</net-zone>", global: false)
  end

  defp writer_xml do
    u = ours("docx")
    ~s(<app name="writer"><action ext="docx" name="view" urlsrc="#{u}"/><action ext="docx" name="edit" urlsrc="#{u}"/></app>)
  end

  # a host may use the edit address for viewing as well, so both point to
  # the viewer; its «Редагувати» goes on to Collabora
  defp calc_xml do
    u = ours("xlsx")
    ~s(<app name="calc"><action ext="xlsx" name="view" urlsrc="#{u}"/><action ext="xlsx" name="edit" urlsrc="#{u}"/></app>)
  end

  defp app_xml, do: writer_xml() <> calc_xml()

  def minimal do
    ~s(<?xml version="1.0" encoding="utf-8"?>\n<wopi-discovery><net-zone name="external-http">#{app_xml()}</net-zone></wopi-discovery>\n)
  end
end
