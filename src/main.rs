mod app;
mod cache;
mod i18n;
mod mutate;
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
use store::Session;

#[derive(Default)]
struct Cli {
    dsh_home: Option<PathBuf>,
    data_dir: Option<PathBuf>,
    sessions_roots: Vec<PathBuf>,
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

    let paths = Paths::discover(cli.dsh_home.clone(), cli.data_dir.clone(), cli.sessions_roots.clone());
    let language_choice = init_language(&paths, cli.lang.as_deref())?;

    if cli.help {
        print_help();
        return Ok(());
    }

    if cli.list {
        let result = store::load(&paths);
        if cli.json {
            let rows: Vec<serde_json::Value> = result
                .sessions
                .iter()
                .map(|session| {
                    serde_json::json!({
                        "id": session.id,
                        "title": session.title,
                        "cwd": session.cwd,
                        "createdAt": session.created_at,
                        "updatedAt": session.mtime_ms,
                        "size": session.size,
                        "frames": session.frames,
                        "parent": session.parent,
                        "compressed": session.compressed,
                        "log": session.log_path.display().to_string(),
                    })
                })
                .collect();
            println!("{}", serde_json::to_string_pretty(&rows)?);
        } else {
            for session in &result.sessions {
                println!("{}\t{}\t{}", session.id, session.cwd, session.title);
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

    run_tui(paths, language_choice)
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

fn run_tui(paths: Paths, language_choice: Option<i18n::Language>) -> Result<()> {
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        ratatui::restore();
        original_hook(info);
    }));

    let mut terminal = ratatui::init();
    let mut app = App::new(paths, language_choice);

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
