use crate::cli::Ctx;
use crate::client::{Client, get, record_value};
use crate::config::{Account, Config, Session, default_base, normalize_base_url};
use crate::curl;
use crate::error::{AppError, Result};
use clap::Subcommand;
use serde_json::{Value, json};
use std::io::Read;

#[derive(Subcommand)]
pub enum AuthCmd {
    /// Parse a DevTools "Copy as cURL" of a Notion /api/v3/ request (stdin recommended)
    ParseCurl {
        /// The cURL text; omit to read stdin (keeps secrets out of argv)
        curl: Option<String>,
        /// Store the parsed session as an account
        #[arg(long)]
        login: bool,
        /// Profile name (default: your Notion name)
        #[arg(long)]
        profile: Option<String>,
    },
    /// Store a session from its parts (prefer env vars: argv is visible in `ps`)
    Login {
        #[arg(long, env = "NTN_TOKEN_V2", hide_env_values = true)]
        token_v2: String,
        #[arg(long, env = "NTN_USER_ID")]
        user_id: String,
        #[arg(long)]
        space_id: Option<String>,
        #[arg(long, default_value_t = default_base())]
        base_url: String,
        /// Browser User-Agent to replay (recommended)
        #[arg(long)]
        user_agent: Option<String>,
        #[arg(long)]
        profile: Option<String>,
    },
    /// List stored accounts
    List {
        /// Verify each stored session
        #[arg(long)]
        check: bool,
    },
    /// Show the identity and workspaces behind the selected account
    Whoami,
    /// Make an account the default
    SetDefault { account: String },
    /// Forget a stored account
    Remove { account: String },
}

pub fn run(ctx: &Ctx, cmd: AuthCmd) -> Result<()> {
    match cmd {
        AuthCmd::ParseCurl {
            curl,
            login: store,
            profile,
        } => {
            let input = match curl {
                Some(c) => c,
                None => {
                    let mut buf = String::new();
                    std::io::stdin().read_to_string(&mut buf)?;
                    buf
                }
            };
            let s = curl::parse(&input)?;
            if store {
                let session = Session {
                    token_v2: s.token_v2,
                    user_id: s.user_id,
                    base_url: s.base_url,
                    user_agent: s.user_agent,
                };
                return login(ctx, session, s.space_id, profile);
            }
            let v = json!({
                "base_url": s.base_url, "user_id": s.user_id, "space_id": s.space_id,
                "token_v2": redact(&s.token_v2), "user_agent": s.user_agent,
            });
            ctx.out.emit(v, |v| {
                format!(
                    "base       {}\nuser       {}\nspace      {}\ntoken_v2   {}\nuser-agent {}\n\nRe-run with --login to store it.\n",
                    v["base_url"].as_str().unwrap_or_default(),
                    v["user_id"].as_str().unwrap_or_default(),
                    v["space_id"].as_str().unwrap_or("(none)"),
                    v["token_v2"].as_str().unwrap_or_default(),
                    v["user_agent"].as_str().unwrap_or("(none)")
                )
            });
            Ok(())
        }
        AuthCmd::Login {
            token_v2,
            user_id,
            space_id,
            base_url,
            user_agent,
            profile,
        } => {
            let session = Session {
                token_v2,
                user_id,
                base_url: normalize_base_url(&base_url)?,
                user_agent,
            };
            login(ctx, session, space_id, profile)
        }
        AuthCmd::List { check } => {
            let cfg = Config::load()?;
            let mut rows = Vec::new();
            for (key, a) in &cfg.accounts {
                let mut row = json!({
                    "profile": key, "name": a.name, "email": a.email,
                    "user_id": a.session.user_id, "space_id": a.space_id,
                    "default": cfg.default_account.as_deref() == Some(key.as_str()),
                });
                if check {
                    let ok = Client::new(a.session.clone(), a.space_id.clone(), ctx.verbose)
                        .and_then(|c| c.call("loadUserContent", json!({})));
                    row["valid"] = json!(ok.is_ok());
                    if let Err(e) = ok {
                        row["error"] = json!(e.message);
                    }
                }
                rows.push(row);
            }
            ctx.out.emit(json!({ "accounts": rows }), |v| {
                let rows = v["accounts"].as_array().cloned().unwrap_or_default();
                if rows.is_empty() {
                    return "No accounts. Run `pbpaste | ntn auth parse-curl --login`.\n".into();
                }
                rows.iter()
                    .map(|r| {
                        format!(
                            "{} {:<16} {:<24} {}{}\n",
                            if r["default"] == true { "*" } else { " " },
                            r["profile"].as_str().unwrap_or_default(),
                            r["name"].as_str().unwrap_or_default(),
                            r["email"].as_str().unwrap_or_default(),
                            match r.get("valid") {
                                Some(Value::Bool(true)) => "  ✓".to_string(),
                                Some(_) =>
                                    format!("  ✗ {}", r["error"].as_str().unwrap_or_default()),
                                None => String::new(),
                            }
                        )
                    })
                    .collect()
            });
            Ok(())
        }
        AuthCmd::Whoami => {
            let client = ctx.client()?;
            let (name, email, spaces) = identity(&client)?;
            let v = json!({
                "user_id": client.user_id(), "name": name, "email": email,
                "space_id": client.space_id(), "spaces": spaces,
            });
            ctx.out.emit(v, |v| {
                let mut s = format!(
                    "{} <{}> ({})\n",
                    v["name"].as_str().unwrap_or_default(),
                    v["email"].as_str().unwrap_or_default(),
                    v["user_id"].as_str().unwrap_or_default()
                );
                for sp in v["spaces"].as_array().into_iter().flatten() {
                    let current = v["space_id"] == sp["id"];
                    s.push_str(&format!(
                        "{} {} {}\n",
                        if current { "*" } else { " " },
                        sp["id"].as_str().unwrap_or_default(),
                        sp["name"].as_str().unwrap_or_default()
                    ));
                }
                s
            });
            Ok(())
        }
        AuthCmd::SetDefault { account } => {
            let mut cfg = Config::load()?;
            let (key, _) = cfg.select(Some(&account))?;
            cfg.default_account = Some(key.clone());
            cfg.save()?;
            ctx.out.emit(json!({ "default_account": key }), |_| {
                format!("Default account: {key}\n")
            });
            Ok(())
        }
        AuthCmd::Remove { account } => {
            let mut cfg = Config::load()?;
            let (key, _) = cfg.select(Some(&account))?;
            cfg.accounts.remove(&key);
            if cfg.default_account.as_deref() == Some(key.as_str()) {
                cfg.default_account = None;
            }
            cfg.save()?;
            ctx.out
                .emit(json!({ "removed": key }), |_| format!("Removed {key}\n"));
            Ok(())
        }
    }
}

/// (name, email, spaces) for the session's user, via loadUserContent.
pub fn identity(client: &Client) -> Result<(String, String, Vec<Value>)> {
    let r = client.call("loadUserContent", json!({}))?;
    let map = &r["recordMap"];
    let user = get(map, "notion_user", client.user_id())
        .ok_or_else(|| AppError::auth("session does not belong to the captured user id"))?;
    let spaces = map["space"]
        .as_object()
        .into_iter()
        .flat_map(|o| o.values())
        .filter_map(record_value)
        .map(|s| json!({ "id": s["id"], "name": s["name"], "domain": s["domain"] }))
        .collect();
    Ok((
        user["name"].as_str().unwrap_or_default().to_string(),
        user["email"].as_str().unwrap_or_default().to_string(),
        spaces,
    ))
}

fn login(
    ctx: &Ctx,
    session: Session,
    space_id: Option<String>,
    profile: Option<String>,
) -> Result<()> {
    let client = Client::new(session.clone(), space_id.clone(), ctx.verbose)?;
    let (name, email, spaces) = identity(&client)?;
    let space_id = space_id.or_else(|| {
        spaces
            .first()
            .and_then(|s| s["id"].as_str())
            .map(str::to_string)
    });
    let key = profile.unwrap_or_else(|| slug(&name, &session.user_id));
    let mut cfg = Config::load()?;
    cfg.accounts.insert(
        key.clone(),
        Account {
            name: name.clone(),
            email: email.clone(),
            space_id: space_id.clone(),
            session,
        },
    );
    if cfg.default_account.is_none() {
        cfg.default_account = Some(key.clone());
    }
    cfg.save()?;
    let v = json!({ "profile": key, "name": name, "email": email, "space_id": space_id, "spaces": spaces });
    ctx.out.emit(v, |v| {
        format!(
            "Logged in as {} <{}> (profile {}), {} workspaces\n",
            v["name"].as_str().unwrap_or_default(),
            v["email"].as_str().unwrap_or_default(),
            v["profile"].as_str().unwrap_or_default(),
            v["spaces"].as_array().map_or(0, Vec::len)
        )
    });
    Ok(())
}

fn slug(name: &str, fallback: &str) -> String {
    let s: String = name
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let s = s.trim_matches('-').to_string();
    if s.is_empty() {
        fallback.to_string()
    } else {
        s
    }
}

fn redact(secret: &str) -> String {
    let head: String = secret.chars().take(8).collect();
    format!("{head}…({} chars)", secret.chars().count())
}
