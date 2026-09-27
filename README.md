# jev-proxy

Local proxy in front of the TypeSafe `/v1/systemone` API. It records, caches and replays every call, with a web UI to inspect them.

One Rust binary. SQLite for storage, HTMX for the UI.

Unofficial. Not affiliated with or endorsed by TypeSafe.

## Run

```bash
cp .env.example .env    # set TYPESAFE_API_KEY
cargo run --release --manifest-path rust/Cargo.toml    # http://127.0.0.1:8420
```

Call it like TypeSafe:

```bash
curl -s http://127.0.0.1:8420/v1/systemone \
  -H "Content-Type: application/json" -H "X-Jev-Caller: my-script" \
  -d '{"state": {"msg": "Thanks!"}, "questions": {"msg_positive": {"instructions": "Is `msg` positive?"}}}'
```

## Config

Environment variables or `.env`.

| Variable            | Default                   | Meaning                                    |
| ------------------- | ------------------------- | ------------------------------------------ |
| `TYPESAFE_API_KEY`  |                           | Required                                   |
| `TYPESAFE_BASE_URL` | `https://api.typesafe.ai` | Upstream                                   |
| `PORT`              | `8420`                    | Listens on `127.0.0.1` only                |
| `CACHE_TTL_SECONDS` | `3600`                    | Only 2xx responses are cached              |
| `JEV_DATA_DIR`      | `data`                    | Where `jev.db` lives                       |
| `SECOND_BASE_URL`   |                           | Optional second engine, for Shadow Compare |
| `SECOND_API_KEY`    |                           | Its bearer token, if any                   |
| `SECOND_LABEL`      | `Second`                  | Its name in the UI                         |
| `SECOND_MODEL`      |                           | Forces the `model` sent to it              |
| `JEV_STATIC_DIR`    |                           | Serve `static/` from disk, no rebuild      |

## Response headers

| Header                  | Meaning                                                   |
| ----------------------- | --------------------------------------------------------- |
| `X-Jev-Cache`           | `HIT` or `MISS`. Send `X-Jev-Cache: 0` to bypass          |
| `X-Jev-Call-Id`         | Recorded row id                                           |
| `X-Jev-Elapsed-Ms`      | Round trip                                                |
| `X-Jev-Attempts`        | A `429` or `529` is retried twice                         |
| `X-Jev-Upstream`        | Engine that answered                                      |
| `X-Jev-Upstream-Status` | `0` when the upstream was unreachable (local 502)         |
| `X-Jev-Shadows`         | Engines run as shadows of this call                       |

## Security

Prompts and responses are stored in plain text in `data/jev.db`. The server has no authentication and listens on `127.0.0.1` only: do not expose it.

## Maintenance

```bash
cargo test --manifest-path rust/Cargo.toml
rust/target/release/jev-proxy --prune-calls 90    # manual only
```
