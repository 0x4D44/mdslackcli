use anyhow::{anyhow, Context, Result};
use clap::{Args, Parser, Subcommand};
use dialoguer::Password;
use keyring::Entry;
use reqwest::blocking::Client;
use serde::Deserialize;
use serde_json::Value;

const SERVICE: &str = "slackcli_user";
const USERNAME: &str = "token";
const API_BASE: &str = "https://slack.com/api";

#[derive(Parser, Debug)]
#[command(name = "slack", version, about = "A tiny Slack CLI in Rust")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
#[command(rename_all = "lowercase")]
enum Commands {
    /// Initialize credentials (stores/validates token in Windows Credential Manager)
    Init(InitArgs),
    /// Show who you are (team, user)
    Whoami,
    /// Join a public channel so you can read/post
    Join(JoinArgs),
    /// List recent 1:1 DMs you have access to
    DirectMsgs(DirectArgs),
    /// List recent multi-person DMs (MPIMs)
    DirectMpMsgs(DirectArgs),
    /// Find a person by name or email and show IDs
    FindPerson(FindArgs),
    /// Open a DM/MPDM with one or more users (requires conversations:write)
    Open(OpenArgs),
    /// List channels/DMs you can see
    Channels(ListArgs),
    /// List recent messages in a channel
    Msgs(MsgsArgs),
    /// Send a message
    Send(SendArgs),
}

#[derive(Args, Debug)]
struct InitArgs {
    /// Clear stored token first
    #[arg(long)]
    reset: bool,
    /// Replace existing token even if valid (re-prompt)
    #[arg(long)]
    force: bool,
    /// Provide token non-interactively (xoxp-…)
    #[arg(long)]
    token: Option<String>,
}

#[derive(Args, Debug)]
struct JoinArgs {
    /// Channel ID (e.g., C01234567)
    #[arg(long)]
    channel: String,
}

#[derive(Args, Debug)]
struct DirectArgs {
    /// Max number of conversations to list
    #[arg(long, default_value_t = 100)]
    limit: u32,
}

#[derive(Args, Debug)]
struct ListArgs {
    /// conversation types (comma-separated)
    #[arg(long, default_value = "public_channel,private_channel,mpim,im")]
    types: String,
    #[arg(long, default_value_t = 200)]
    limit: u32,
}

#[derive(Args, Debug)]
struct MsgsArgs {
    /// Channel ID (e.g., C123ÃƒÆ’Ã‚Â¢ÃƒÂ¢Ã¢â‚¬Å¡Ã‚Â¬Ãƒâ€šÃ‚Â¦ or D123ÃƒÆ’Ã‚Â¢ÃƒÂ¢Ã¢â‚¬Å¡Ã‚Â¬Ãƒâ€šÃ‚Â¦)
    #[arg(long)]
    channel: String,
    #[arg(long, default_value_t = 25)]
    limit: u32,
}

#[derive(Args, Debug)]
struct SendArgs {
    #[arg(long)]
    channel: String,
    #[arg(long)]
    text: String,
    /// Optional thread timestamp (to reply in a thread)
    #[arg(long)]
    thread_ts: Option<String>,
}

#[derive(Args, Debug)]
struct FindArgs {
    /// Substring to match against display name, real name, email, or user ID
    #[arg(long)]
    query: String,
    /// Max matches to show
    #[arg(long, default_value_t = 50)]
    limit: usize,
}

#[derive(Args, Debug)]
struct OpenArgs {
    /// Comma-separated list of user IDs (e.g., U123,U456)
    #[arg(long)]
    users: String,
    /// Optional text to send immediately in the opened conversation
    #[arg(long)]
    text: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AuthTest {
    ok: bool,
    team: Option<String>,
    team_id: Option<String>,
    user_id: Option<String>,
    bot_id: Option<String>,
    error: Option<String>,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Commands::Init(args) => init(args),
        Commands::Whoami => {
            let token = ensure_token()?;
            let client = http();
            let info = auth_test(&client, &token)?;
            println!(
                "ok: {}\nteam: {:?}\nteam_id: {:?}\nuser_id: {:?}\nbot_id: {:?}",
                info.ok, info.team, info.team_id, info.user_id, info.bot_id
            );
            Ok(())
        }
        Commands::Join(args) => {
            let token = ensure_token()?;
            let client = http();
            let resp = slack_post(
                &client,
                "conversations.join",
                &token,
                Some(&[("channel", args.channel.as_str())]),
            )?;
            let name = resp
                .get("channel")
                .and_then(|c| c.get("name"))
                .and_then(|v| v.as_str())
                .unwrap_or("(unknown)");
            println!("Joined #{name}");
            Ok(())
        }
        Commands::DirectMsgs(args) => {
            let token = ensure_token()?;
            let client = http();
            let resp = slack_post(
                &client,
                "conversations.list",
                &token,
                Some(&[("types", "im"), ("limit", &args.limit.to_string())]),
            )?;
            let ims = resp
                .get("channels")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            let users = fetch_users_map(&client, &token)?;
            for im in ims {
                let id = im.get("id").and_then(|v| v.as_str()).unwrap_or("-");
                let uid = im.get("user").and_then(|v| v.as_str()).unwrap_or("-");
                let (disp, real, email) =
                    users.get(uid).cloned().unwrap_or(("?".into(), None, None));
                let real_s = real.as_deref().unwrap_or("");
                let email_s = email.as_deref().unwrap_or("");
                println!("{id}\t@{disp}\t{real_s}\t{email_s}");
            }
            Ok(())
        }
        Commands::DirectMpMsgs(args) => {
            let token = ensure_token()?;
            let client = http();
            let resp = slack_post(
                &client,
                "conversations.list",
                &token,
                Some(&[("types", "mpim"), ("limit", &args.limit.to_string())]),
            )?;
            let chans = resp
                .get("channels")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            for ch in chans {
                let id = ch.get("id").and_then(|v| v.as_str()).unwrap_or("-");
                let name = ch.get("name").and_then(|v| v.as_str()).unwrap_or("(mpdm)");
                println!("{id}\t#{name}");
            }
            Ok(())
        }
        Commands::FindPerson(args) => {
            let token = ensure_token()?;
            let client = http();
            let users = fetch_users_map(&client, &token)?;
            // Build a user -> DM channel map by listing IMs
            let ims_resp = slack_post(
                &client,
                "conversations.list",
                &token,
                Some(&[("types", "im"), ("limit", "1000")]),
            )?;
            let mut user_to_dm: std::collections::HashMap<String, String> =
                std::collections::HashMap::new();
            if let Some(ims) = ims_resp.get("channels").and_then(|v| v.as_array()) {
                for im in ims {
                    if let (Some(uid), Some(cid)) = (
                        im.get("user").and_then(|v| v.as_str()),
                        im.get("id").and_then(|v| v.as_str()),
                    ) {
                        user_to_dm.insert(uid.to_string(), cid.to_string());
                    }
                }
            }
            let q = args.query.to_lowercase();
            let mut rows: Vec<(String, String, String, String, String)> = Vec::new();
            for (uid, (disp, real, email)) in users.iter() {
                let real_s = real.as_deref().unwrap_or("");
                let email_s = email.as_deref().unwrap_or("");
                let inq = |s: &str| s.to_lowercase().contains(&q);
                if inq(disp) || inq(real_s) || inq(email_s) || inq(uid) {
                    let dm = user_to_dm.get(uid).cloned().unwrap_or_else(|| "-".into());
                    rows.push((
                        uid.clone(),
                        dm,
                        format!("@{}", disp),
                        real_s.to_string(),
                        email_s.to_string(),
                    ));
                }
            }
            rows.truncate(args.limit);
            for (uid, dm, atname, real, email) in rows {
                println!("{uid}\t{dm}\t{atname}\t{real}\t{email}");
            }
            Ok(())
        }
        Commands::Open(args) => {
            let token = ensure_token()?;
            let client = http();
            let users = args
                .users
                .split(',')
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join(",");
            let resp = slack_post(
                &client,
                "conversations.open",
                &token,
                Some(&[("users", users.as_str())]),
            )?;
            let channel_id = resp
                .get("channel")
                .and_then(|c| c.get("id"))
                .and_then(|v| v.as_str())
                .unwrap_or("-");
            println!("opened channel: {channel_id}");
            if let Some(text) = args.text.as_deref() {
                let _ = slack_post(
                    &client,
                    "chat.postMessage",
                    &token,
                    Some(&[("channel", channel_id), ("text", text)]),
                );
            }
            Ok(())
        }
        Commands::Channels(args) => {
            let token = ensure_token()?;
            let client = http();
            let resp = slack_post(
                &client,
                "conversations.list",
                &token,
                Some(&[
                    ("types", args.types.as_str()),
                    ("limit", &args.limit.to_string()),
                ]),
            )?;
            let chans = resp
                .get("channels")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            for ch in chans {
                let id = ch.get("id").and_then(|v| v.as_str()).unwrap_or("-");
                let name = ch
                    .get("name")
                    .and_then(|v| v.as_str())
                    .or_else(|| ch.get("name_normalized").and_then(|v| v.as_str()))
                    .unwrap_or("(dm or unnamed)");
                let ctype = if ch.get("is_im").and_then(|v| v.as_bool()).unwrap_or(false) {
                    "im"
                } else if ch.get("is_mpim").and_then(|v| v.as_bool()).unwrap_or(false) {
                    "mpim"
                } else if ch
                    .get("is_private")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false)
                {
                    "private_channel"
                } else {
                    "public_channel"
                };
                println!("{id}\t#{name}\t({ctype})");
            }
            Ok(())
        }
        Commands::Msgs(args) => {
            let token = ensure_token()?;
            let client = http();
            let resp = slack_post(
                &client,
                "conversations.history",
                &token,
                Some(&[
                    ("channel", args.channel.as_str()),
                    ("limit", &args.limit.to_string()),
                ]),
            )?;
            let msgs = resp
                .get("messages")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            for m in msgs.iter().rev() {
                let ts = m.get("ts").and_then(|v| v.as_str()).unwrap_or("-");
                let user = m
                    .get("user")
                    .and_then(|v| v.as_str())
                    .or_else(|| m.get("bot_id").and_then(|v| v.as_str()))
                    .unwrap_or("unknown");
                let text = m.get("text").and_then(|v| v.as_str()).unwrap_or("");
                println!("{ts} {user}: {text}");
            }
            Ok(())
        }
        Commands::Send(args) => {
            let token = ensure_token()?;
            let client = http();
            let mut form = vec![
                ("channel", args.channel.as_str()),
                ("text", args.text.as_str()),
            ];
            if let Some(ts) = args.thread_ts.as_ref() {
                form.push(("thread_ts", ts.as_str()));
            }
            let resp = slack_post(&client, "chat.postMessage", &token, Some(&form))?;
            let ok = resp.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
            let ts = resp.get("ts").and_then(|v| v.as_str()).unwrap_or("-");
            if ok {
                println!("sent ok, ts={ts}");
                Ok(())
            } else {
                let err = resp
                    .get("error")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown_error");
                Err(anyhow!("Slack error: {err}"))
            }
        }
    }
}

/// --- Init & credential helpers ---
fn init(args: InitArgs) -> Result<()> {
    if args.reset {
        let _ = delete_token();
    }
    if args.force || args.token.is_some() {
        let token = match args.token {
            Some(t) => t,
            None => prompt_for_token()?,
        };
        store_token(&token)?;
        let client = http();
        let info = auth_test(&client, &token)?;
        if info.ok {
            println!("Saved valid token for team {:?}.", info.team);
            return Ok(());
        } else {
            return Err(anyhow!(info.error.unwrap_or_else(|| "invalid_auth".into())));
        }
    }
    match ensure_token() {
        Ok(_) => {
            println!("Token is present and valid.");
            Ok(())
        }
        Err(_) => {
            // Prompt loop until we succeed or user aborts
            for _ in 0..3 {
                let token = prompt_for_token()?;
                store_token(&token)?;
                let client = http();
                match auth_test(&client, &token) {
                    Ok(info) if info.ok => {
                        println!("Saved valid token for team {:?}.", info.team);
                        return Ok(());
                    }
                    Ok(info) => eprintln!("Token didn't validate: {:?}", info.error),
                    Err(e) => eprintln!("Validation call failed: {e}"),
                }
            }
            Err(anyhow!("Could not obtain a working token."))
        }
    }
}

fn ensure_token() -> Result<String> {
    if let Some(tok) = read_token()? {
        let client = http();
        let info = auth_test(&client, &tok)?;
        if info.ok {
            return Ok(tok);
        }
    }
    let token = prompt_for_token()?;
    store_token(&token)?;
    let client = http();
    let info = auth_test(&client, &token)?;
    if info.ok {
        Ok(token)
    } else {
        Err(anyhow!(info.error.unwrap_or_else(|| "invalid_auth".into())))
    }
}

fn read_token() -> Result<Option<String>> {
    let entry = Entry::new(SERVICE, USERNAME)?;
    match entry.get_password() {
        Ok(s) if s.trim().is_empty() => Ok(None),
        Ok(s) => Ok(Some(s)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(anyhow!("keyring read error: {e}")),
    }
}

fn store_token(token: &str) -> Result<()> {
    let entry = Entry::new(SERVICE, USERNAME)?;
    entry
        .set_password(token)
        .map_err(|e| anyhow!("keyring write error: {e}"))
}

fn delete_token() -> Result<()> {
    let entry = Entry::new(SERVICE, USERNAME)?;
    entry
        .set_password("")
        .map_err(|e| anyhow!("keyring delete/overwrite error: {e}"))
}

/// --- Slack HTTP helpers ---
fn http() -> Client {
    Client::builder()
        .user_agent("slackcli/0.1 (+https://example.local)")
        .build()
        .expect("client build")
}

fn auth_test(client: &Client, token: &str) -> Result<AuthTest> {
    let url = format!("{API_BASE}/auth.test");
    let resp = client
        .post(&url)
        .bearer_auth(token)
        .send()
        .context("auth.test http failed")?;
    let st = resp.status();
    let body = resp.text()?;
    if !st.is_success() {
        return Err(anyhow!("HTTP {st} from Slack: {body}"));
    }
    let at: AuthTest = serde_json::from_str(&body)?;
    Ok(at)
}

fn slack_post(
    client: &Client,
    method: &str,
    token: &str,
    form: Option<&[(&str, &str)]>,
) -> Result<Value> {
    let url = format!("{API_BASE}/{method}");
    let resp = client
        .post(&url)
        .bearer_auth(token)
        .form(form.unwrap_or(&[]))
        .send()
        .with_context(|| format!("{method} http failed"))?;
    let st = resp.status();
    let v: Value = resp.json().context("Slack JSON parse failed")?;
    if !st.is_success() {
        return Err(anyhow!("HTTP {st} error from Slack"));
    }
    if !v.get("ok").and_then(|x| x.as_bool()).unwrap_or(false) {
        let err = v
            .get("error")
            .and_then(|x| x.as_str())
            .unwrap_or("unknown_error");
        return Err(anyhow!("Slack error: {err}"));
    }
    Ok(v)
}

use std::collections::HashMap;

type UserInfo = (String, Option<String>, Option<String>);

/// Fetch users.list and return a map from user_id to (display_name, real_name, email)
fn fetch_users_map(client: &Client, token: &str) -> Result<HashMap<String, UserInfo>> {
    let mut map: HashMap<String, UserInfo> = HashMap::new();
    let resp = slack_post(client, "users.list", token, Some(&[("limit", "200")]))?;
    let members = resp
        .get("members")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    for m in members {
        if let Some(uid) = m.get("id").and_then(|v| v.as_str()) {
            let prof = m.get("profile").cloned().unwrap_or(Value::Null);
            let disp = prof
                .get("display_name")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
                .or_else(|| {
                    m.get("name")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string())
                })
                .unwrap_or_else(|| uid.to_string());
            let real = prof
                .get("real_name")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let email = prof
                .get("email")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            map.insert(uid.to_string(), (disp, real, email));
        }
    }
    Ok(map)
}

/// Prompt user for a Slack user token and return it.
fn prompt_for_token() -> Result<String> {
    let token = Password::new()
        .with_prompt("Slack user token (xoxp-Ã¢â‚¬Â¦)")
        .interact()
        .context("failed to read token from prompt")?;
    let token = token.trim().to_string();
    if token.is_empty() {
        Err(anyhow!("Token must not be empty"))
    } else {
        Ok(token)
    }
}
