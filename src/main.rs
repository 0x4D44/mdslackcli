use anyhow::{anyhow, Context, Result};
use clap::{Args, Parser, Subcommand};
use dialoguer::Password;
use keyring::Entry;
use reqwest::blocking::Client;
use serde::Deserialize;
use serde_json::Value;

const SERVICE: &str = "slackcli";
const USERNAME: &str = "bot_token";
const API_BASE: &str = "https://slack.com/api";

#[derive(Parser, Debug)]
#[command(name = "slack", version, about = "A tiny Slack CLI in Rust")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Initialize credentials (stores/validates token in Windows Credential Manager)
    Init(InitArgs),
    /// Show who the token is (team, user/bot)
    Whoami,
    /// List channels/DMs the bot can see
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
    /// Channel ID (e.g., C123… or D123…)
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
        // Validate
        let client = http();
        let info = auth_test(&client, &tok)?;
        if info.ok {
            return Ok(tok);
        }
        // token exists but is bad → fall through to prompt
    }
    // Prompt & store
    let token = prompt_for_token()?;
    store_token(&token)?;
    // Validate stored
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
    // keyring v3 on some backends does not expose a delete method; overwrite instead
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

/// Prompt user for a Slack bot token and return it.
fn prompt_for_token() -> Result<String> {
    let token = Password::new()
        .with_prompt("Enter Slack bot token (e.g., xoxb-...)")
        .interact()
        .context("failed to read token from prompt")?;
    let token = token.trim().to_string();
    if token.is_empty() {
        Err(anyhow!("Token must not be empty"))
    } else {
        Ok(token)
    }
}
