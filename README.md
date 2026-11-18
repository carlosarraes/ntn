# ntn

Read-only Notion CLI for agents, written in Rust. It's the Notion sibling of [slk](https://github.com/carlosarraes/slk).
It uses your browser session, so it sees everything you can see in Notion, with no integration to set up and nothing to share page by page. It only reads: the HTTP client refuses any endpoint that isn't on a read-only allowlist.

## Install

```sh
curl -sSf https://raw.githubusercontent.com/carlosarraes/ntn/main/install.sh | sh
```

## Login

1. Open Notion in your browser. Open DevTools → Network, then filter by `api/v3`.
2. Click around until requests show up. Right-click one, such as `loadUserContent` or `syncRecordValues`, then choose **Copy as cURL**.
3. Run `pbpaste | ntn auth parse-curl --login`, then check it with `ntn auth whoami`.

The session is stored in `~/.config/ntn/accounts.json` (mode 0600). `token_v2` gives full access to your Notion account, so keep that file private.

## Run without a saved login

For CI, containers or agents that shouldn't keep `~/.config/ntn` around, pass the session through the environment instead:

```sh
export NTN_TOKEN_V2='v03%3A...'      # the token_v2 cookie from your browser
export NTN_USER_ID='...'             # optional: looked up from the token when unset
ntn search "onboarding"
```

The credentials live in memory for that process: nothing is written to disk and the token is never printed. `NTN_SPACE` (or `--space`) picks the workspace and `NTN_USER_AGENT` replays a browser User-Agent; both are optional. If the user id can't be found from the token, ntn exits 9 and names `NTN_USER_ID`.

Which credentials win: an explicit `-a <profile>` (or `NTN_ACCOUNT`) uses that saved profile; otherwise `NTN_TOKEN_V2` is used; otherwise the default saved profile. `-a env` forces the environment credentials, and `ntn auth list` shows them as profile `env` without printing the token.

## Use

```sh
ntn search "onboarding" --limit 20            # JSON has "next"; page on with --offset <next>
ntn page read https://www.notion.so/acme/Onboarding-1a2b3c4d5e6f40718293a4b5c6d7e8f9
ntn db query 'https://www.notion.so/acme/9f8e...?v=0123...' --limit 20 --fields Name,Status,url
ntn spaces
```

Output is JSON when stdout isn't a terminal. `ntn llm` prints the agent guide, and `just install-skill` installs it as a Claude Code skill.

ntn uses Notion's private web API (`/api/v3`), which isn't documented and can change without notice.
