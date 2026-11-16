# ntn — read-only Notion CLI for agents

ntn can only read. It cannot create, edit, move or delete anything in Notion.
Output is JSON whenever stdout is not a terminal (or pass `--json`).
Errors go to stderr as `{"error":{"code","message","retryable","details"}}`.

## Exit codes
0 ok · 2 invalid input · 3 auth/permission · 4 not found · 5 ambiguous · 6 rate limited · 7 network · 8 Notion/other · 9 config/io

## Targets
Any Notion URL (`https://www.notion.so/<ws>/Title-<32hex>[?v=<view>][&p=<page>]`) or a raw page/database id.

## Commands
```
ntn search "<query>" [--limit N] [--sort relevance|last-edited] [--in <page>] [--since 7d] [--edited-by me|@Name]
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

## Tips
- Search first, then `page read` the `url` of the best hit. On long pages use `--section`.
- Meeting action items: `ntn meetings read <url> --fields action_items`. AI items rarely carry @mentions, so owners/due are often empty; `evidence` holds the cited transcript lines {at,speaker,text} (speakers are anonymous `speaker0`, `speaker1`…).
- Links to other pages in Markdown are full Notion URLs you can `page read` next.
- Inline databases render as `🗃 Database [...]`; read rows with `ntn db query <url>`.
- Keep output small: `ntn db query <url> --fields Name,Status,url`.
