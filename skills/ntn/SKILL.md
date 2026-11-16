---
name: ntn
description: Read and search Notion from the terminal with the read-only `ntn` CLI. Use when the user asks about content in Notion, wants to find a Notion page or doc, read a Notion URL, or list rows of a Notion database.
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
ntn search "<query>" [--limit N] [--sort relevance|last-edited] [--in <page>]
ntn page read <url|id>          # Markdown; JSON: {id,title,url,last_edited,markdown}
ntn db query <url|id> [--limit N] [--search q]   # rows keyed by column name
ntn spaces                      # workspaces; pick one with --space <id>
ntn auth whoami | list [--check]
```
Global: `-a/--account <profile>`, `--space <id>`, `--fields a,b.c`, `--json`, `--text`, `-v`.

## Tips
- Search first, then `page read` the `url` of the best hit.
- Links to other pages in Markdown are full Notion URLs you can `page read` next.
- Inline databases render as `🗃 Database [...]`; read rows with `ntn db query <url>`.
- Keep output small: `ntn db query <url> --fields Name,Status,url`.
