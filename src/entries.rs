//! External settings entries: XDG desktop files in the `Settings` category.
//!
//! This is the registry for the "More" page. Any tool that ships a desktop
//! entry with `Categories=Settings;` is listed; `X-Dials-Section=<name>`
//! overrides the section it lands in. Entries are launched, never embedded.
//!
//! Only the keys needed to list and launch are parsed. Localised keys
//! (`Name[en_GB]`) are ignored in favour of the plain key.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Desktop-entry id of this application; never listed.
pub const SELF_ID: &str = "dials";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// File stem (`pavucontrol` for `pavucontrol.desktop`).
    pub id: String,
    pub name: String,
    pub comment: String,
    pub icon: String,
    pub section: String,
    /// `Exec=` with field codes stripped, split into argv.
    pub argv: Vec<String>,
}

/// Ordered sections, user-facing names. Unknown `X-Dials-Section` values are
/// shown verbatim after these.
const SECTIONS: &[&str] = &["Appearance", "Hardware", "Network", "System", "Other"];

/// Scan every `applications/` dir, user first. Later (system) files with an
/// id already seen are shadowed, matching XDG precedence.
pub fn scan(dirs: &[PathBuf], current_desktop: &[String]) -> Vec<Entry> {
    let mut seen: BTreeMap<String, Option<Entry>> = BTreeMap::new();
    for dir in dirs {
        let Ok(rd) = std::fs::read_dir(dir.join("applications")) else {
            continue;
        };
        let mut files: Vec<PathBuf> = rd.flatten().map(|e| e.path()).collect();
        files.sort();
        for path in files {
            if path.extension().and_then(|e| e.to_str()) != Some("desktop") {
                continue;
            }
            let Some(id) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            if seen.contains_key(id) {
                continue;
            }
            let parsed = std::fs::read_to_string(&path)
                .ok()
                .and_then(|text| parse(id, &text, current_desktop));
            seen.insert(id.to_string(), parsed);
        }
    }
    let mut entries: Vec<Entry> = seen.into_values().flatten().collect();
    entries.sort_by(|a, b| {
        section_rank(&a.section)
            .cmp(&section_rank(&b.section))
            .then_with(|| a.section.cmp(&b.section))
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    entries
}

fn section_rank(s: &str) -> usize {
    SECTIONS
        .iter()
        .position(|k| k.eq_ignore_ascii_case(s))
        .unwrap_or(SECTIONS.len())
}

/// `XDG_DATA_HOME/applications` then each `XDG_DATA_DIRS` entry.
pub fn data_dirs(data_home: &Path, xdg_data_dirs: Option<&str>) -> Vec<PathBuf> {
    let mut dirs = vec![data_home.to_path_buf()];
    let system = xdg_data_dirs
        .filter(|s| !s.is_empty())
        .unwrap_or("/usr/local/share:/usr/share");
    for d in system.split(':').filter(|d| d.starts_with('/')) {
        let p = PathBuf::from(d);
        if !dirs.contains(&p) {
            dirs.push(p);
        }
    }
    dirs
}

/// `$XDG_CURRENT_DESKTOP` split on `:`.
pub fn current_desktop(value: Option<&str>) -> Vec<String> {
    value
        .unwrap_or("")
        .split(':')
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// Parse one desktop file. `None` when it is not a listable settings entry.
pub fn parse(id: &str, text: &str, current_desktop: &[String]) -> Option<Entry> {
    if id == SELF_ID {
        return None;
    }
    let mut keys: BTreeMap<&str, &str> = BTreeMap::new();
    let mut in_entry = false;
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') {
            in_entry = line == "[Desktop Entry]";
            continue;
        }
        if !in_entry {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        let k = k.trim();
        // Skip localised keys; plain key wins.
        if k.contains('[') {
            continue;
        }
        keys.entry(k).or_insert(v.trim());
    }

    if keys.get("Type").copied().unwrap_or("Application") != "Application" {
        return None;
    }
    if is_true(keys.get("NoDisplay")) || is_true(keys.get("Hidden")) {
        return None;
    }
    let categories: Vec<&str> = list(keys.get("Categories"));
    let section_override = keys
        .get("X-Dials-Section")
        .copied()
        .filter(|s| !s.is_empty());
    if section_override.is_none() && !categories.contains(&"Settings") {
        return None;
    }
    if let Some(only) = keys.get("OnlyShowIn") {
        let only = list(Some(only));
        if !only.iter().any(|d| current_desktop.iter().any(|c| c == d)) {
            return None;
        }
    }
    if let Some(not) = keys.get("NotShowIn") {
        let not = list(Some(not));
        if not.iter().any(|d| current_desktop.iter().any(|c| c == d)) {
            return None;
        }
    }
    if let Some(try_exec) = keys.get("TryExec").copied().filter(|s| !s.is_empty())
        && !on_path(try_exec)
    {
        return None;
    }
    let exec = keys.get("Exec").copied().unwrap_or("");
    let mut argv = exec_argv(exec);
    if argv.is_empty() {
        return None;
    }
    if is_true(keys.get("Terminal")) {
        // Terminal entries need a host. xdg-terminal-exec is the freedesktop
        // convention; $TERMINAL is the common fallback. Neither → not listed.
        let mut host = terminal_host()?;
        host.append(&mut argv);
        argv = host;
    }
    let name = keys.get("Name").copied().unwrap_or(id).to_string();
    let section = section_override
        .map(str::to_string)
        .unwrap_or_else(|| section_from_categories(&categories).to_string());
    Some(Entry {
        id: id.to_string(),
        name,
        comment: keys.get("Comment").copied().unwrap_or("").to_string(),
        icon: keys.get("Icon").copied().unwrap_or("").to_string(),
        section,
        argv,
    })
}

fn is_true(v: Option<&&str>) -> bool {
    v.map(|s| s.eq_ignore_ascii_case("true")).unwrap_or(false)
}

fn list<'a>(v: Option<&&'a str>) -> Vec<&'a str> {
    v.map(|s| {
        s.split(';')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .collect()
    })
    .unwrap_or_default()
}

fn section_from_categories(categories: &[&str]) -> &'static str {
    for c in categories {
        match *c {
            "DesktopSettings" | "Appearance" => return "Appearance",
            "HardwareSettings" => return "Hardware",
            "Network" | "X-GNOME-NetworkSettings" => return "Network",
            "System" | "Security" | "PackageManager" => return "System",
            _ => {}
        }
    }
    if categories
        .iter()
        .any(|c| matches!(*c, "Audio" | "AudioVideo" | "Mixer"))
    {
        return "Hardware";
    }
    "Other"
}

/// Command prefix that runs a terminal program, or `None` when no host exists.
/// Order: the freedesktop `xdg-terminal-exec` (not in every distro's repos),
/// the user's `$TERMINAL`, then common Wayland terminals with their
/// "run this command" form. Extend the list, do not guess flags.
fn terminal_host() -> Option<Vec<String>> {
    if on_path("xdg-terminal-exec") {
        return Some(vec!["xdg-terminal-exec".into()]);
    }
    if let Some(term) = std::env::var("TERMINAL").ok().filter(|t| !t.is_empty()) {
        let argv = exec_argv(&term);
        if let Some(exe) = argv.first()
            && on_path(exe)
        {
            return Some(argv);
        }
    }
    KNOWN_TERMINALS
        .iter()
        .find(|(exe, _)| on_path(exe))
        .map(|(exe, args)| {
            std::iter::once(exe.to_string())
                .chain(args.iter().map(|a| a.to_string()))
                .collect()
        })
}

/// Terminal binary and the arguments that make it run a trailing command.
const KNOWN_TERMINALS: &[(&str, &[&str])] = &[
    ("kitty", &[]),
    ("foot", &[]),
    ("alacritty", &["-e"]),
    ("wezterm", &["start", "--"]),
    ("ghostty", &["-e"]),
];

/// Absolute path, or found in `$PATH`.
fn on_path(exe: &str) -> bool {
    if exe.contains('/') {
        return Path::new(exe).is_file();
    }
    std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).any(|d| d.join(exe).is_file()))
        .unwrap_or(false)
}

/// Split `Exec=` per the desktop-entry spec: shell-like quoting, `%` field
/// codes dropped (we never launch with a file or URL), `%%` → `%`.
pub fn exec_argv(exec: &str) -> Vec<String> {
    let mut argv = Vec::new();
    let mut cur = String::new();
    let mut in_arg = false;
    let mut quoted = false;
    let mut chars = exec.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                quoted = !quoted;
                in_arg = true;
            }
            '\\' if quoted => {
                if let Some(n) = chars.next() {
                    cur.push(n);
                }
            }
            '%' => {
                if let Some('%') = chars.next() {
                    cur.push('%');
                }
            }
            c if c.is_whitespace() && !quoted => {
                if in_arg {
                    argv.push(std::mem::take(&mut cur));
                    in_arg = false;
                }
            }
            c => {
                cur.push(c);
                in_arg = true;
            }
        }
    }
    if in_arg {
        argv.push(cur);
    }
    argv.retain(|a| !a.is_empty());
    argv
}

/// Launch detached in its own process group so closing Dials does not take
/// the tool with it. Errors are returned as text for the status line.
pub fn launch(entry: &Entry) -> Result<(), String> {
    use std::os::unix::process::CommandExt;
    let (exe, args) = entry.argv.split_first().ok_or("empty Exec")?;
    std::process::Command::new(exe)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .process_group(0)
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("{}: {e}", entry.name))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(id: &str, text: &str) -> Option<Entry> {
        parse(id, text, &["Hyprland".to_string()])
    }

    #[test]
    fn settings_category_is_listed() {
        let e = p(
            "pavucontrol",
            "[Desktop Entry]\nType=Application\nName=Volume Control\nExec=pavucontrol %U\nCategories=AudioVideo;Audio;Mixer;Settings;\n",
        )
        .unwrap();
        assert_eq!(e.name, "Volume Control");
        assert_eq!(e.argv, vec!["pavucontrol"]);
        assert_eq!(e.section, "Hardware");
    }

    #[test]
    fn non_settings_is_skipped() {
        assert!(p("firefox", "[Desktop Entry]\nType=Application\nName=Firefox\nExec=firefox\nCategories=Network;WebBrowser;\n").is_none());
    }

    #[test]
    fn section_key_overrides_and_admits() {
        let e = p(
            "lmtt-config",
            "[Desktop Entry]\nType=Application\nName=Themes\nExec=kitty lmtt-config\nCategories=Utility;\nX-Dials-Section=Appearance\n",
        )
        .unwrap();
        assert_eq!(e.section, "Appearance");
    }

    #[test]
    fn self_nodisplay_hidden_are_skipped() {
        let body = "[Desktop Entry]\nType=Application\nName=x\nExec=x\nCategories=Settings;\n";
        assert!(p("dials", body).is_none());
        assert!(p("a", &format!("{body}NoDisplay=true\n")).is_none());
        assert!(p("b", &format!("{body}Hidden=true\n")).is_none());
    }

    #[test]
    fn only_show_in_and_not_show_in() {
        let body = "[Desktop Entry]\nType=Application\nName=x\nExec=x\nCategories=Settings;\n";
        assert!(p("a", &format!("{body}OnlyShowIn=GNOME;\n")).is_none());
        assert!(p("b", &format!("{body}OnlyShowIn=GNOME;Hyprland;\n")).is_some());
        assert!(p("c", &format!("{body}NotShowIn=Hyprland;\n")).is_none());
    }

    #[test]
    fn terminal_entries_get_a_host_or_are_hidden() {
        let body = "[Desktop Entry]\nType=Application\nName=x\nExec=lmtt-config\nTerminal=true\nCategories=Settings;\n";
        // `true` exists on every PATH; use it as the terminal host.
        // SAFETY: tests in this module run single-threaded for this var.
        unsafe { std::env::set_var("TERMINAL", "true -e") };
        let e = p("a", body);
        unsafe { std::env::remove_var("TERMINAL") };
        if on_path("xdg-terminal-exec") {
            assert_eq!(e.unwrap().argv[0], "xdg-terminal-exec");
        } else {
            assert_eq!(e.unwrap().argv, vec!["true", "-e", "lmtt-config"]);
        }
    }

    #[test]
    fn known_terminal_fallback_when_no_terminal_env() {
        // SAFETY: tests in this module run single-threaded for this var.
        unsafe { std::env::remove_var("TERMINAL") };
        let host = terminal_host();
        let any_known = KNOWN_TERMINALS.iter().any(|(exe, _)| on_path(exe));
        if on_path("xdg-terminal-exec") {
            assert_eq!(host.unwrap()[0], "xdg-terminal-exec");
        } else if any_known {
            let host = host.unwrap();
            assert!(
                KNOWN_TERMINALS.iter().any(|(exe, _)| *exe == host[0]),
                "{host:?}"
            );
        } else {
            assert!(host.is_none());
        }
    }

    #[test]
    fn missing_try_exec_is_skipped() {
        let body = "[Desktop Entry]\nType=Application\nName=x\nExec=x\nTryExec=/nonexistent/definitely-not-here\nCategories=Settings;\n";
        assert!(p("a", body).is_none());
    }

    #[test]
    fn localised_keys_do_not_override() {
        let e = p(
            "a",
            "[Desktop Entry]\nType=Application\nName[de]=Einstellungen\nName=Settings\nExec=x\nCategories=Settings;\n",
        )
        .unwrap();
        assert_eq!(e.name, "Settings");
    }

    #[test]
    fn only_desktop_entry_group_is_read() {
        let e = p(
            "a",
            "[Desktop Entry]\nType=Application\nName=Main\nExec=main\nCategories=Settings;\n[Desktop Action x]\nName=Other\nExec=other\n",
        )
        .unwrap();
        assert_eq!(e.name, "Main");
        assert_eq!(e.argv, vec!["main"]);
    }

    #[test]
    fn exec_field_codes_and_quotes() {
        assert_eq!(exec_argv("foo %U"), vec!["foo"]);
        assert_eq!(exec_argv("foo --a %f --b"), vec!["foo", "--a", "--b"]);
        assert_eq!(
            exec_argv("\"/opt/my app/bin\" --x=\"a b\""),
            vec!["/opt/my app/bin", "--x=a b"]
        );
        assert_eq!(
            exec_argv("env FOO=100%% bar"),
            vec!["env", "FOO=100%", "bar"]
        );
        assert_eq!(exec_argv("  "), Vec::<String>::new());
    }

    #[test]
    fn data_dirs_user_first_defaults() {
        let dirs = data_dirs(Path::new("/home/u/.local/share"), None);
        assert_eq!(
            dirs,
            vec![
                PathBuf::from("/home/u/.local/share"),
                PathBuf::from("/usr/local/share"),
                PathBuf::from("/usr/share")
            ]
        );
        let dirs = data_dirs(Path::new("/h"), Some("/a:relative:/h:/b"));
        assert_eq!(
            dirs,
            vec![
                PathBuf::from("/h"),
                PathBuf::from("/a"),
                PathBuf::from("/b")
            ]
        );
    }

    #[test]
    fn scan_user_shadows_system_and_sorts() {
        let tmp = std::env::temp_dir().join(format!("dials-entries-{}", std::process::id()));
        let user = tmp.join("user/applications");
        let sys = tmp.join("sys/applications");
        std::fs::create_dir_all(&user).unwrap();
        std::fs::create_dir_all(&sys).unwrap();
        let mk = |dir: &Path, id: &str, name: &str, extra: &str| {
            std::fs::write(
                dir.join(format!("{id}.desktop")),
                format!("[Desktop Entry]\nType=Application\nName={name}\nExec={id}\nCategories=Settings;\n{extra}"),
            )
            .unwrap()
        };
        mk(&sys, "zeta", "Zeta", "");
        mk(&sys, "shadowed", "System copy", "");
        mk(&user, "shadowed", "User copy", "NoDisplay=true\n");
        mk(&sys, "alpha", "Alpha", "X-Dials-Section=Appearance\n");
        let got = scan(&[tmp.join("user"), tmp.join("sys")], &[]);
        std::fs::remove_dir_all(&tmp).ok();
        let names: Vec<&str> = got.iter().map(|e| e.name.as_str()).collect();
        // User NoDisplay hides the system copy; Appearance sorts before Other.
        assert_eq!(names, vec!["Alpha", "Zeta"]);
    }
}
