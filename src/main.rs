mod app;
mod cache;
mod i18n;
mod session;
mod settings;
mod source;
mod sources;
mod ui;
mod zstd_frames;

use std::env;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{anyhow, bail, Result};
use crossterm::event::{self, Event, KeyEventKind};

use app::App;
use session::Session;
use source::{Env, Registry};

#[derive(Default)]
struct Cli {
    dsh_home: Option<PathBuf>,
    data_dir: Option<PathBuf>,
    sessions_roots: Vec<PathBuf>,
    opencode_dirs: Vec<PathBuf>,
    /// `--source-home <id>=<path>` and `--source-dir <id>=<path>` land here, so
    /// a source added later needs no CLI changes.
    source_homes: Vec<(String, PathBuf)>,
    source_dirs: Vec<(String, PathBuf)>,
    source: Option<String>,
    lang: Option<String>,
    list: bool,
    json: bool,
    rename: Option<(String, String)>,
    delete: Option<String>,
    set_cwd: Option<(String, String)>,
    help: bool,
}

fn main() -> Result<()> {
    let cli = parse_args()?;
    let registry = sources::registry();
    let env = build_env(&cli);
    let language_choice = init_language(&env, cli.lang.as_deref())?;

    if cli.help {
        print_help();
        return Ok(());
    }

    // `--source` is whatever the registry knows, including sources added later.
    let source = match cli.source.as_deref() {
        None | Some("all") => None,
        Some(value) if value.eq_ignore_ascii_case("all") => None,
        Some(value) => {
            let ids = registry.ids().collect::<Vec<_>>().join("、");
            let id = registry.get(value).map(|source| source.id()).ok_or_else(|| {
                anyhow!(i18n::tf(
                    "cli.err.invalid_source",
                    &[("value", value), ("sources", &ids)]
                ))
            })?;
            Some(id)
        }
    };

    if cli.list {
        let result = registry.load(&env);
        let sessions: Vec<&Session> = result
            .sessions
            .iter()
            .filter(|session| source.is_none_or(|id| id == session.source))
            .collect();
        if cli.json {
            let rows: Vec<serde_json::Value> = sessions
                .iter()
                .map(|session| session_json(&registry, session))
                .collect();
            println!("{}", serde_json::to_string_pretty(&rows)?);
        } else {
            for session in sessions {
                println!("{}\t{}\t{}\t{}", session.id, session.cwd, session.title, session.source);
            }
        }
        for error in result.errors {
            eprintln!("{}", i18n::tf("cli.warning", &[("error", &error)]));
        }
        return Ok(());
    }

    if let Some((id, title)) = cli.rename {
        let result = registry.load(&env);
        let session = find_session(&result.sessions, &id)?;
        registry.rename(&env, session, &title)?;
        println!(
            "{}",
            i18n::tf("cli.renamed", &[("id", &session.id), ("title", title.trim())],)
        );
        return Ok(());
    }

    if let Some(id) = cli.delete {
        let result = registry.load(&env);
        let session = find_session(&result.sessions, &id)?;
        registry.delete(&env, session)?;
        println!("{}", i18n::tf("cli.deleted", &[("id", &session.id)]));
        return Ok(());
    }

    if let Some((id, cwd)) = cli.set_cwd {
        let result = registry.load(&env);
        let session = find_session(&result.sessions, &id)?;
        let new_cwd = registry.change_workspace(&env, session, &cwd)?;
        println!("{}", i18n::tf("cli.moved", &[("id", &session.id), ("cwd", &new_cwd)]));
        return Ok(());
    }

    run_tui(env, registry, language_choice, source)
}

/// The `--json` shape: the fields every session has, plus whatever the source
/// wants to add (log path, database path, token counts...).
fn session_json(registry: &Registry, session: &Session) -> serde_json::Value {
    let mut value = serde_json::json!({
        "id": session.id,
        "source": session.source,
        "title": session.title,
        "titleSource": session.title_source,
        "cwd": session.cwd,
        "createdAt": session.created_at,
        "updatedAt": session.mtime_ms,
        "parent": session.parent,
        "agent": session.agent_preset,
        "model": session.model,
    });
    if let Some(object) = value.as_object_mut() {
        for (key, field) in registry.json_fields(session) {
            object.insert(key, field);
        }
    }
    value
}

/// Resolves where every source should look, from the command line plus the
/// defaults for each agent.
fn build_env(cli: &Cli) -> Env {
    let home = home_dir();
    let data_dir = cli.data_dir.clone().unwrap_or_else(|| home.join(".dsh-tui"));
    let mut env = Env::new(home, data_dir);

    if let Some(path) = &cli.dsh_home {
        env = env.with_home("dst", path.clone());
    }
    for path in &cli.sessions_roots {
        env = env.with_dir("dst", path.clone());
    }
    for path in &cli.opencode_dirs {
        env = env.with_dir("opencode", path.clone());
    }
    for (source, path) in &cli.source_homes {
        env = env.with_home(source.clone(), path.clone());
    }
    for (source, path) in &cli.source_dirs {
        env = env.with_dir(source.clone(), path.clone());
    }
    env
}

fn home_dir() -> PathBuf {
    let profile = if cfg!(windows) {
        source::env_path("USERPROFILE")
    } else {
        None
    };
    profile
        .or_else(|| source::env_path("HOME"))
        .or_else(|| source::env_path("USERPROFILE"))
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Applies the startup language and returns the user's preference: `None`
/// means "follow the system", `Some` means an explicit saved choice.
fn init_language(env: &Env, explicit: Option<&str>) -> Result<Option<i18n::Language>> {
    // Start from the system so that even an argument error is readable.
    i18n::set(i18n::Language::detect());

    let saved = settings::load_language(&env.data_dir);
    let (language, choice) = match explicit {
        Some(value) if value.eq_ignore_ascii_case("auto") => (i18n::Language::detect(), None),
        Some(value) => {
            let language = i18n::Language::from_code(value)
                .ok_or_else(|| anyhow!(i18n::tf("cli.err.invalid_language", &[("value", value)])))?;
            (language, Some(language))
        }
        None => (saved.unwrap_or_else(i18n::Language::detect), saved),
    };

    i18n::set(language);
    Ok(choice)
}

fn run_tui(
    env: Env,
    registry: Registry,
    language_choice: Option<i18n::Language>,
    source: Option<&'static str>,
) -> Result<()> {
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        ratatui::restore();
        original_hook(info);
    }));

    let mut terminal = ratatui::init();
    let mut app = App::new(env, registry, language_choice, source);

    let outcome = (|| -> Result<()> {
        loop {
            terminal.draw(|frame| ui::draw(frame, &mut app))?;
            if event::poll(Duration::from_millis(200))? {
                if let Event::Key(key) = event::read()? {
                    if key.kind == KeyEventKind::Press {
                        app.handle_key(key);
                    }
                }
            }
            if app.quit {
                break;
            }
        }
        Ok(())
    })();

    ratatui::restore();
    outcome
}

fn find_session<'a>(sessions: &'a [Session], needle: &str) -> Result<&'a Session> {
    if let Some(session) = sessions.iter().find(|session| session.id == needle) {
        return Ok(session);
    }
    let matches: Vec<&Session> = sessions
        .iter()
        .filter(|session| session.id.starts_with(needle))
        .collect();
    match matches.len() {
        1 => Ok(matches[0]),
        0 => bail!("{}", i18n::tf("cli.err.no_session", &[("needle", needle)])),
        count => bail!(
            "{}",
            i18n::tf(
                "cli.err.ambiguous",
                &[("needle", needle), ("count", &count.to_string())],
            )
        ),
    }
}

fn parse_args() -> Result<Cli> {
    let mut cli = Cli::default();
    let mut args = env::args().skip(1);
    while let Some(argument) = args.next() {
        let (key, inline) = match argument.split_once('=') {
            Some((key, value)) => (key.to_string(), Some(value.to_string())),
            None => (argument.clone(), None),
        };
        let mut take = |name: &str| -> Result<String> {
            if let Some(value) = inline.clone() {
                return Ok(value);
            }
            args.next()
                .ok_or_else(|| anyhow!(i18n::tf("cli.err.missing_value", &[("name", name)])))
        };

        match key.as_str() {
            "-h" | "--help" => cli.help = true,
            "--list" => cli.list = true,
            "--json" => cli.json = true,
            "--lang" => cli.lang = Some(take("--lang")?),
            "--dsh-home" => cli.dsh_home = Some(PathBuf::from(take("--dsh-home")?)),
            "--data-dir" => cli.data_dir = Some(PathBuf::from(take("--data-dir")?)),
            "--sessions-root" => {
                cli.sessions_roots.push(PathBuf::from(take("--sessions-root")?));
            }
            "--opencode-dir" => {
                cli.opencode_dirs.push(PathBuf::from(take("--opencode-dir")?));
            }
            "--source-dir" => {
                cli.source_dirs
                    .push(parse_source_path(&take("--source-dir <id=path>")?)?);
            }
            "--source-home" => {
                cli.source_homes
                    .push(parse_source_path(&take("--source-home <id=path>")?)?);
            }
            "--source" => cli.source = Some(take("--source <all|source>")?),
            "--rename" => {
                let id = take("--rename <id>")?;
                let title = take("--rename <title>")?;
                cli.rename = Some((id, title));
            }
            "--delete" => cli.delete = Some(take("--delete <id>")?),
            "--set-cwd" => {
                let id = take("--set-cwd <id>")?;
                let cwd = take("--set-cwd <path>")?;
                cli.set_cwd = Some((id, cwd));
            }
            other => bail!("{}", i18n::tf("cli.err.unknown_argument", &[("arg", other)])),
        }
    }
    Ok(cli)
}

/// `<source>=<path>`, the shape `--source-dir` and `--source-home` accept.
fn parse_source_path(value: &str) -> Result<(String, PathBuf)> {
    let (source, path) = value
        .split_once('=')
        .ok_or_else(|| anyhow!(i18n::tf("cli.err.source_path", &[("value", value)])))?;
    if source.trim().is_empty() || path.trim().is_empty() {
        bail!("{}", i18n::tf("cli.err.source_path", &[("value", value)]));
    }
    Ok((source.trim().to_string(), PathBuf::from(path.trim())))
}

fn print_help() {
    print!("{}", i18n::tf("cli.help", &[("version", env!("CARGO_PKG_VERSION"))]));
}
