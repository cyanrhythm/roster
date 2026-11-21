mod app;
mod cache;
mod i18n;
mod mutate;
mod opencode;
mod paths;
mod settings;
mod store;
mod ui;
mod zstd_frames;

use std::env;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{anyhow, bail, Result};
use crossterm::event::{self, Event, KeyEventKind};

use app::App;
use paths::Paths;
use store::{Kind, Location, Session};

#[derive(Default)]
struct Cli {
    dsh_home: Option<PathBuf>,
    data_dir: Option<PathBuf>,
    sessions_roots: Vec<PathBuf>,
    opencode_dirs: Vec<PathBuf>,
    source: Option<Kind>,
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

    let paths = Paths::discover(
        cli.dsh_home.clone(),
        cli.data_dir.clone(),
        cli.sessions_roots.clone(),
        cli.opencode_dirs.clone(),
    );
    let language_choice = init_language(&paths, cli.lang.as_deref())?;

    if cli.help {
        print_help();
        return Ok(());
    }

    if cli.list {
        let result = store::load(&paths);
        let sessions: Vec<&Session> = result
            .sessions
            .iter()
            .filter(|session| cli.source.is_none_or(|kind| kind == session.kind()))
            .collect();
        if cli.json {
            let rows: Vec<serde_json::Value> = sessions.iter().map(|session| session_json(session)).collect();
            println!("{}", serde_json::to_string_pretty(&rows)?);
        } else {
            for session in sessions {
                println!(
                    "{}\t{}\t{}\t{}",
                    session.id,
                    session.cwd,
                    session.title,
                    session.kind().as_str()
                );
            }
        }
        for error in result.errors {
            eprintln!("{}", i18n::tf("cli.warning", &[("error", &error)]));
        }
        return Ok(());
    }

    if let Some((id, title)) = cli.rename {
        let result = store::load(&paths);
        let session = find_session(&result.sessions, &id)?;
        mutate::rename(&paths, session, &title)?;
        println!(
            "{}",
            i18n::tf("cli.renamed", &[("id", &session.id), ("title", title.trim())],)
        );
        return Ok(());
    }

    if let Some(id) = cli.delete {
        let result = store::load(&paths);
        let session = find_session(&result.sessions, &id)?;
        let trash = mutate::delete(&paths, session)?;
        println!(
            "{}",
            i18n::tf(
                "cli.trashed",
                &[("id", &session.id), ("path", &trash.display().to_string())],
            )
        );
        return Ok(());
    }

    if let Some((id, cwd)) = cli.set_cwd {
        let result = store::load(&paths);
        let session = find_session(&result.sessions, &id)?;
        let new_cwd = mutate::change_workspace(&paths, session, &cwd)?;
        println!("{}", i18n::tf("cli.moved", &[("id", &session.id), ("cwd", &new_cwd)]));
        return Ok(());
    }

    run_tui(paths, language_choice, cli.source)
}

/// The `--json` shape: one flat object, with the fields that only make sense
/// for one source added on top.
fn session_json(session: &Session) -> serde_json::Value {
    let mut value = serde_json::json!({
        "id": session.id,
        "source": session.kind().as_str(),
        "title": session.title,
        "titleSource": session.title_source,
        "cwd": session.cwd,
        "createdAt": session.created_at,
        "updatedAt": session.mtime_ms,
        "parent": session.parent,
        "agent": session.agent_preset,
        "model": session.model,
    });
    let object = value.as_object_mut().expect("a JSON object");
    match &session.location {
        Location::Dst {
            root,
            dir,
            log_path,
            log_name,
            compressed,
            frames,
            size,
            ..
        } => {
            object.insert("root".into(), serde_json::json!(root.display().to_string()));
            object.insert("dir".into(), serde_json::json!(dir.display().to_string()));
            object.insert("log".into(), serde_json::json!(log_path.display().to_string()));
            object.insert("logName".into(), serde_json::json!(log_name));
            object.insert("compressed".into(), serde_json::json!(compressed));
            object.insert("frames".into(), serde_json::json!(frames));
            object.insert("size".into(), serde_json::json!(size));
        }
        Location::Opencode {
            db,
            project_id,
            worktree,
            messages,
            events,
            cost,
            tokens_in,
            tokens_out,
            tokens_reasoning,
            tokens_cache_read,
            tokens_cache_write,
        } => {
            object.insert("db".into(), serde_json::json!(db.display().to_string()));
            object.insert("projectId".into(), serde_json::json!(project_id));
            object.insert("worktree".into(), serde_json::json!(worktree));
            object.insert("messages".into(), serde_json::json!(messages));
            object.insert("events".into(), serde_json::json!(events));
            object.insert("cost".into(), serde_json::json!(cost));
            object.insert(
                "tokens".into(),
                serde_json::json!({
                    "input": tokens_in,
                    "output": tokens_out,
                    "reasoning": tokens_reasoning,
                    "cacheRead": tokens_cache_read,
                    "cacheWrite": tokens_cache_write,
                }),
            );
        }
    }
    value
}

/// Applies the startup language and returns the user's preference: `None`
/// means "follow the system", `Some` means an explicit saved choice.
fn init_language(paths: &Paths, explicit: Option<&str>) -> Result<Option<i18n::Language>> {
    // Start from the system so that even an argument error is readable.
    i18n::set(i18n::Language::detect());

    let saved = settings::load_language(&paths.data_dir);
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

fn run_tui(paths: Paths, language_choice: Option<i18n::Language>, source: Option<Kind>) -> Result<()> {
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        ratatui::restore();
        original_hook(info);
    }));

    let mut terminal = ratatui::init();
    let mut app = App::new(paths, language_choice, source);

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
            "--source" => {
                let value = take("--source <all|dst|opencode>")?;
                cli.source = if value.eq_ignore_ascii_case("all") {
                    None
                } else {
                    Some(
                        Kind::from_arg(&value)
                            .ok_or_else(|| anyhow!(i18n::tf("cli.err.invalid_source", &[("value", &value)])))?,
                    )
                };
            }
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

fn print_help() {
    print!("{}", i18n::tf("cli.help", &[("version", env!("CARGO_PKG_VERSION"))]));
}
