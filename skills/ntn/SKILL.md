---
name: ntn
description: Read and search Notion from the terminal with the read-only `ntn` CLI. Use when the user asks about content in Notion, wants to find a Notion page or doc, read a Notion URL, check comments or recent changes on a page, review meeting notes and action items, or list and filter rows of a Notion database.
---


ntn can only read. It cannot create, edit, move or delete anything in Notion.
Output is JSON whenever stdout is not a terminal (or pass `--json`).
Errors go to stderr as `{"error":{"code","message","retryable","details"}}`.

## Exit codes
0 ok · 2 invalid input · 3 auth/permission · 4 not found · 5 ambiguous · 6 rate limited · 7 network · 8 Notion/other · 9 config/io

## Targets
Any Notion URL (`https://www.notion.so/<ws>/Title-<32hex>[?v=<view>][&p=<page>]`) or a raw page/database id.

## Commands
```
ntn search "<query>" [--limit N] [--offset N] [--sort relevance|last-edited] [--in <page>] [--since 7d] [--edited-by me|@Name]
ntn page read <url|id> [<url|id> ...] [--section "Heading"] [--transcript]   # Markdown; several pages -> JSON array
ntn page comments <url> [--all]          # open threads (with --all: resolved too): quote, author, time
ntn page changes <url> [--since 2d]      # blocks added/edited since, with the heading they sit under
ntn db query <url|id> [--where 'Status=Done'] [--where 'Owner=@Ana'] [--sort 'Date desc'] [--limit N] [--search q]
ntn meetings list [--since 7d] [--query q]
ntn meetings read <url> [--transcript]   # summary, action_items {text,done,owners,due,evidence}, notes, transcript
ntn spaces                               # workspaces; pick one with --space <id>
ntn auth whoami | list [--check]
```
`--where` operators: `=` `!=` `>` `<` `>=` `<=` `~` (contains). Values match the displayed cell text, case-insensitive; multi-value cells match if any value does.
Times: `30m`, `2h`, `7d`, `2w` or `YYYY-MM-DD`.
Global: `-a/--account <profile>`, `--space <id>`, `--fields a,b.c` (trims items; implies JSON), `--json`, `--text`, `-v`.

## Credentials
Normally a saved login (`pbpaste | ntn auth parse-curl --login`). With no saved login, set env vars and every read command (search, page read, auth whoami…) runs on them in memory; nothing is written to disk and the token is never printed.
- `NTN_TOKEN_V2` (required): the `token_v2` cookie. `NTN_USER_ID` (optional): found from the token when unset; if that fails, exit 9 `configuration` naming NTN_USER_ID. `NTN_SPACE`/`--space` picks the workspace (default: the first), `NTN_USER_AGENT` replays a browser UA.
- Precedence: `-a/--account` (or `NTN_ACCOUNT`) names a saved profile and wins; otherwise `NTN_TOKEN_V2`; otherwise the default saved profile. `-a env` forces the env credentials.
- `ntn auth list` shows them as profile `env` (`active` marks the one in use).

## Tips
- Paging search: the JSON has `next` (null on the last page); pass it as `--offset <next>` with the same query and flags. Search reaches the top 100 hits only: past that, narrow with `--in`, `--since` or a sharper query.
- Search first, then `page read` the `url` of the best hit. On long pages use `--section`. Hits carry a `snippet` only if Notion returned highlight text (usually it does not); judge by `title`/`path`.
- Meeting action items: `ntn meetings read <url> --fields action_items`. AI items rarely carry @mentions, so owners/due are often empty; `evidence` holds the cited transcript lines {at,speaker,text} (speakers are anonymous `speaker0`, `speaker1`…).
- A missing or inaccessible page exits 4 with `not_found`. With several pages, one missing page fails the whole call (nothing on stdout; `error.details.missing` lists the ids): drop those ids and re-run.
- Links to other pages in Markdown are full Notion URLs you can `page read` next.
- Inline databases render as `🗃 Database [...]`; read rows with `ntn db query <url>`.
- Keep output small: `ntn db query <url> --fields Name,Status,url`.
