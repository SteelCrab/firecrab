use super::settings::{Error, Settings, Snapshot, Store};
use crossterm::{
    cursor,
    event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
    execute, queue,
    style::{self, Color},
    terminal::{self, ClearType},
};
use std::io::{self, IsTerminal, Write};

struct Terminal;
impl Terminal {
    fn enter() -> io::Result<Self> {
        terminal::enable_raw_mode()?;
        let guard = Self;
        execute!(io::stdout(), terminal::EnterAlternateScreen, cursor::Hide)?;
        Ok(guard)
    }
}
impl Drop for Terminal {
    fn drop(&mut self) {
        let _ = execute!(
            io::stdout(),
            style::ResetColor,
            cursor::Show,
            terminal::LeaveAlternateScreen
        );
        let _ = terminal::disable_raw_mode();
    }
}

struct Editor {
    draft: Settings,
    focus: usize,
    advanced: bool,
    input: Option<String>,
    discard: bool,
    message: String,
}
enum Action {
    None,
    Save,
    Quit,
}
impl Editor {
    fn new(settings: Settings) -> Self {
        Self {
            draft: settings,
            focus: 0,
            advanced: false,
            input: None,
            discard: false,
            message: String::new(),
        }
    }
    fn last(&self) -> usize {
        if self.advanced { 7 } else { 6 }
    }
    fn key(&mut self, key: KeyEvent, saved: &Settings) -> Action {
        if key.kind == KeyEventKind::Release {
            return Action::None;
        }
        if self.discard {
            match key.code {
                KeyCode::Char('y' | 'Y') => return Action::Quit,
                KeyCode::Char('n' | 'N') | KeyCode::Esc => self.discard = false,
                _ => {}
            }
            return Action::None;
        }
        if let Some(input) = &mut self.input {
            match key.code {
                KeyCode::Char(c) if c.is_ascii_digit() && input.len() < 7 => input.push(c),
                KeyCode::Backspace => {
                    input.pop();
                }
                KeyCode::Esc => self.input = None,
                KeyCode::Enter => {
                    let key = match self.focus {
                        0 => "cpu",
                        1 => "memory_mib",
                        4 => "idle_minutes",
                        _ => "api_port",
                    };
                    let mut candidate = self.draft.clone();
                    match candidate
                        .set(&format!("{key}={input}"))
                        .and_then(|_| candidate.validate())
                    {
                        Ok(()) => {
                            self.draft = candidate;
                            self.input = None;
                            self.message.clear();
                        }
                        Err(e) => self.message = e.to_string(),
                    }
                }
                _ => {}
            }
            return Action::None;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('s') {
            return Action::Save;
        }
        match key.code {
            KeyCode::Up | KeyCode::BackTab => {
                self.focus = if self.focus == 0 {
                    self.last()
                } else {
                    self.focus - 1
                }
            }
            KeyCode::Down | KeyCode::Tab => self.focus = (self.focus + 1) % (self.last() + 1),
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('c')
                if key.code != KeyCode::Char('c')
                    || key.modifiers.contains(KeyModifiers::CONTROL) =>
            {
                if &self.draft == saved {
                    return Action::Quit;
                }
                self.discard = true;
            }
            KeyCode::Enter | KeyCode::Char(' ') | KeyCode::Left | KeyCode::Right => {
                match self.focus {
                    2 => self.draft.autostart = !self.draft.autostart,
                    3 => self.draft.sleepy = !self.draft.sleepy,
                    5 => self.advanced = !self.advanced,
                    f if f == self.last() => return Action::Save,
                    4 if !self.draft.sleepy => {}
                    0 | 1 | 4 | 6 => {
                        // Empty input replaces the old value. Escape keeps the original.
                        self.input = Some(String::new());
                        self.message = "Enter a value, then Enter. Esc cancels.".into();
                    }
                    _ => {}
                }
            }
            _ => {}
        }
        Action::None
    }
    fn lines(&self, store: &Store, saved: &Settings) -> Vec<(Option<usize>, String)> {
        let check = |yes| if yes { "[x]" } else { "[ ]" };
        let value = |index, text: String| {
            if self.focus == index {
                self.input
                    .as_ref()
                    .map(|input| format!("[{input}_]"))
                    .unwrap_or(text)
            } else {
                text
            }
        };
        let mut lines = vec![
            (
                None,
                format!(
                    "microManager settings{}",
                    if &self.draft == saved { "" } else { " *" }
                ),
            ),
            (None, String::new()),
            (None, "Basic settings".into()),
            (
                Some(0),
                format!("CPU              {}", value(0, self.draft.cpu.to_string())),
            ),
            (
                Some(1),
                format!(
                    "Memory           {} MiB",
                    value(1, self.draft.memory_mib.to_string())
                ),
            ),
            (
                Some(2),
                format!("{} Start VM at login", check(self.draft.autostart)),
            ),
            (None, String::new()),
            (None, "Sleepy".into()),
            (
                Some(3),
                format!("{} Sleep when idle", check(self.draft.sleepy)),
            ),
            (
                Some(4),
                format!(
                    "Idle timeout     {} minutes",
                    value(4, self.draft.idle_minutes.to_string())
                ),
            ),
            (
                None,
                "Wake             Automatically on work requests".into(),
            ),
            (None, String::new()),
            (
                Some(5),
                format!("{} Advanced", if self.advanced { "v" } else { ">" }),
            ),
        ];
        if self.advanced {
            lines.push((
                None,
                format!("Data directory   {} (read only)", store.home.display()),
            ));
            lines.push((
                Some(6),
                format!(
                    "Local API port   {}",
                    value(6, self.draft.api_port.to_string())
                ),
            ));
        }
        lines.extend([
            (None, String::new()),
            (Some(self.last()), "[ Save settings ]".into()),
            (
                None,
                "Tab/Arrows: select  Enter: edit  Space: toggle".into(),
            ),
            (None, "Ctrl+S: save  Esc: exit".into()),
            (
                None,
                "Resources/port: apply with firecrab service start".into(),
            ),
            (
                None,
                if self.discard {
                    "Unsaved changes. Discard? Y / N".into()
                } else {
                    self.message.clone()
                },
            ),
        ]);
        lines
    }
}

pub fn run(store: &Store, mut saved: Snapshot) -> Result<(), Error> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Err(Error::Invalid(
            "Settings TUI needs a terminal; use --json or --set KEY=VALUE".into(),
        ));
    }
    let _terminal = Terminal::enter()?;
    let mut editor = Editor::new(saved.settings.clone());
    loop {
        draw(&editor, store, &saved.settings)?;
        if let Event::Key(key) = event::read()? {
            match editor.key(key, &saved.settings) {
                Action::Quit => return Ok(()),
                Action::Save => match store.save(&saved, editor.draft.clone()) {
                    Ok(snapshot) => {
                        saved = snapshot;
                        editor.message =
                            "Saved. Sleepy policy reloads while the controller runs.".into();
                    }
                    Err(e) => editor.message = e.to_string(),
                },
                Action::None => {}
            }
        }
    }
}
fn draw(editor: &Editor, store: &Store, saved: &Settings) -> io::Result<()> {
    let (width, height) = terminal::size()?;
    let mut stdout = io::stdout();
    queue!(
        stdout,
        cursor::MoveTo(0, 0),
        terminal::Clear(ClearType::All)
    )?;
    let lines = editor.lines(store, saved);
    if width < 52 || height < lines.len() as u16 {
        queue!(stdout, style::Print("Resize terminal to at least 52 x 24"))?;
    } else {
        for (row, (focus, line)) in lines.iter().enumerate() {
            let selected = *focus == Some(editor.focus);
            queue!(
                stdout,
                cursor::MoveTo(0, row as u16),
                style::SetForegroundColor(if selected { Color::Cyan } else { Color::Reset }),
                style::Print(if selected { "> " } else { "  " }),
                style::Print(clip(line, width.saturating_sub(3) as usize)),
                style::ResetColor
            )?;
        }
    }
    stdout.flush()
}
fn clip(line: &str, width: usize) -> String {
    // Paths may contain non-ASCII characters; wide characters count twice.
    let mut used = 0;
    line.chars()
        .filter(|c| !c.is_control())
        .take_while(|c| {
            used += if c.is_ascii() { 1 } else { 2 };
            used <= width
        })
        .collect()
}
#[cfg(test)]
mod tests {
    use super::*;
    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }
    #[test]
    fn keyboard_edit_cancel_save_and_discard_are_consistent() {
        let saved = Settings::default();
        let mut editor = Editor::new(saved.clone());
        editor.key(key(KeyCode::Enter), &saved);
        editor.key(key(KeyCode::Char('8')), &saved);
        editor.key(key(KeyCode::Esc), &saved);
        assert_eq!(editor.draft.cpu, 2);
        editor.key(key(KeyCode::Enter), &saved);
        editor.key(key(KeyCode::Char('4')), &saved);
        editor.key(key(KeyCode::Enter), &saved);
        assert_eq!(editor.draft.cpu, 4);
        assert!(matches!(
            editor.key(
                KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL),
                &saved
            ),
            Action::Save
        ));
        assert!(matches!(
            editor.key(key(KeyCode::Esc), &saved),
            Action::None
        ));
        assert!(editor.discard);
        assert!(matches!(
            editor.key(key(KeyCode::Char('y')), &saved),
            Action::Quit
        ));
    }
    #[test]
    #[ignore = "run inside a PTY via scripts/test-micromanager-settings.py"]
    fn interactive_terminal() {
        let home = std::env::var_os("FIRECRAB_TUI_TEST_HOME").expect("isolated test directory");
        let store = Store::new(std::path::Path::new(&home));
        run(&store, store.load().unwrap()).unwrap();
    }
}
