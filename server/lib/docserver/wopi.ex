defmodule Docserver.Wopi do
  @moduledoc """
  WOPI client for the document host.

  Host behaviour this client handles:
    * CheckFileInfo has only BaseFileName, Size, UserCanWrite, UserId,
      UserFriendlyName and OwnerId; no locks.
    * PutFile is ignored unless `X-COOL-WOPI-IsModifiedByUser: true` is sent.
    * Every PutFile creates a new version with a new id; the old id is
      never written again (a second PutFile to it answers 200 but saves nothing).
  Access tokens are never logged.
  """
  require Logger

  @timeout 30_000

  @doc "Whether `src` points to an allowed WOPI host."
  def allowed?(src) when is_binary(src) do
    case URI.parse(src) do
      %URI{scheme: s, host: h} = u when s in ["http", "https"] and is_binary(h) ->
        Enum.any?(Application.get_env(:docserver, :wopi_hosts, []), fn prefix ->
          p = URI.parse(prefix)
          p.scheme == u.scheme and p.host == u.host and (p.port || default_port(p.scheme)) == (u.port || default_port(u.scheme)) and
            String.starts_with?(u.path || "/", p.path || "/")
        end)

      _ ->
        false
    end
  end

  def allowed?(_), do: false

  defp default_port("https"), do: 443
  defp default_port(_), do: 80

  defp url(src, suffix, token) do
    %URI{path: path, query: q} = u = URI.parse(src)
    params = URI.decode_query(q || "") |> Map.put("access_token", token)
    URI.to_string(%URI{u | path: path <> suffix, query: URI.encode_query(params)})
  end

  def check_file_info(src, token) do
    case request(:get, url(src, "", token), [{~c"accept", ~c"application/json"}], nil) do
      {:ok, 200, body} ->
        case Jason.decode(body) do
          {:ok, %{} = info} -> {:ok, info}
          _ -> {:error, :bad_info}
        end

      {:ok, status, _} ->
        {:error, status}

      err ->
        err
    end
  end

  def get_file(src, token) do
    case request(:get, url(src, "/contents", token), [], nil) do
      {:ok, 200, body} -> {:ok, body}
      {:ok, status, _} -> {:error, status}
      err -> err
    end
  end

  @doc "reason: :manual | :auto | :exit"
  def put_file(src, token, body, reason) do
    headers = [
      {~c"x-wopi-override", ~c"PUT"},
      {~c"x-cool-wopi-ismodifiedbyuser", ~c"true"},
      {~c"x-cool-wopi-isautosave", if(reason == :auto, do: ~c"true", else: ~c"false")},
      {~c"x-cool-wopi-isexitsave", if(reason == :exit, do: ~c"true", else: ~c"false")}
    ]

    case request(:post, url(src, "/contents", token), headers, body) do
      {:ok, status, _} when status in 200..299 -> :ok
      {:ok, status, _} -> {:error, status}
      err -> err
    end
  end

  defp request(method, url, headers, body) do
    http_opts = [timeout: @timeout, connect_timeout: 10_000, ssl: ssl_opts(), autoredirect: false]
    req =
      case method do
        :get -> {String.to_charlist(url), headers}
        :post -> {String.to_charlist(url), headers, ~c"application/octet-stream", body}
      end

    case :httpc.request(method, req, http_opts, body_format: :binary) do
      {:ok, {{_, status, _}, _h, resp}} ->
        {:ok, status, resp}

      {:error, reason} ->
        Logger.warning("wopi #{method} #{redact(url)} failed: #{inspect(reason)}")
        {:error, :unavailable}
    end
  end

  defp ssl_opts do
    if Application.get_env(:docserver, :verify_tls, true) do
      cacerts =
        case Application.get_env(:docserver, :cacertfile) do
          nil -> [cacerts: :public_key.cacerts_get()]
          file -> [cacertfile: String.to_charlist(file)]
        end

      [verify: :verify_peer, customize_hostname_check: [match_fun: :public_key.pkix_verify_hostname_match_fun(:https)]] ++ cacerts
    else
      [verify: :verify_none]
    end
  end

  def redact(url), do: Regex.replace(~r/access_token=[^&]*/, url, "access_token=***")
end
