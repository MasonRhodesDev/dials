//! Where pam_fprintd sits in the PAM stacks, read-only. dials never writes
//! PAM configuration: this only reports which prompts would accept a
//! fingerprint. Follows `include`/`substack` (and Debian's `@include`) the
//! way libpam does, looking in /etc/pam.d before the vendor /usr/lib/pam.d.
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// Stacks worth reporting, in display order: the stack name and what it
/// guards. Only stacks that exist on this system are shown.
pub const STACKS: &[(&str, &str)] = &[
    ("sudo", "sudo"),
    ("polkit-1", "Administrator prompts (polkit)"),
    ("vigil", "vigil login screen"),
    ("vigil-lock", "vigil lock screen"),
    ("hyprlock", "hyprlock"),
    ("greetd", "greetd"),
    ("login", "Console login"),
    ("system-local-login", "Local logins (shared)"),
    ("system-auth", "System auth (shared)"),
];

const MODULE: &str = "pam_fprintd.so";

/// Directories libpam modules are installed in across Arch, Fedora and Debian.
const MODULE_DIRS: &[&str] = &[
    "usr/lib/security",
    "usr/lib64/security",
    "lib/security",
    "lib64/security",
    "usr/lib/x86_64-linux-gnu/security",
    "usr/lib/aarch64-linux-gnu/security",
];

/// PAM configuration directories, in libpam's lookup order.
const CONFIG_DIRS: &[&str] = &["etc/pam.d", "usr/lib/pam.d", "usr/share/pam.d"];

/// Include depth past which a chain is treated as a loop.
const MAX_DEPTH: usize = 8;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Wiring {
    /// pam_fprintd is in this stack's auth section with this control.
    Direct(String),
    /// Reached through an included stack (the first hop named).
    Via {
        stack: String,
        control: String,
    },
    Absent,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stack {
    pub name: String,
    pub label: String,
    pub wiring: Wiring,
}

/// Fedora's authselect selection: the profile and whether the
/// `with-fingerprint` feature is on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Authselect {
    pub profile: String,
    pub fingerprint: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Status {
    /// The installed pam_fprintd module, if any.
    pub module: Option<PathBuf>,
    pub stacks: Vec<Stack>,
    pub authselect: Option<Authselect>,
}

/// Read the PAM status under `root` (`/` outside tests).
pub fn read(root: &Path) -> Status {
    let module = MODULE_DIRS
        .iter()
        .map(|d| root.join(d).join(MODULE))
        .find(|p| p.exists());
    let stacks = STACKS
        .iter()
        .filter(|(name, _)| stack_text(root, name).is_some())
        .map(|(name, label)| Stack {
            name: name.to_string(),
            label: label.to_string(),
            wiring: wiring(root, name),
        })
        .collect();
    let authselect = std::fs::read_to_string(root.join("etc/authselect/authselect.conf"))
        .ok()
        .and_then(|t| parse_authselect(&t));
    Status {
        module,
        stacks,
        authselect,
    }
}

fn stack_text(root: &Path, name: &str) -> Option<String> {
    if name.is_empty() || name.contains('/') {
        return None;
    }
    CONFIG_DIRS
        .iter()
        .find_map(|d| std::fs::read_to_string(root.join(d).join(name)).ok())
}

/// How the auth section of stack `name` reaches pam_fprintd.
pub fn wiring(root: &Path, name: &str) -> Wiring {
    let mut seen = HashSet::new();
    match find(root, name, 0, &mut seen) {
        Some((None, control)) => Wiring::Direct(control),
        Some((Some(stack), control)) => Wiring::Via { stack, control },
        None => Wiring::Absent,
    }
}

/// The first pam_fprintd auth line reachable from `name`: the included
/// stack it came through (None when it is in `name` itself) and its control.
fn find(
    root: &Path,
    name: &str,
    depth: usize,
    seen: &mut HashSet<String>,
) -> Option<(Option<String>, String)> {
    if depth > MAX_DEPTH || !seen.insert(name.to_string()) {
        return None;
    }
    let text = stack_text(root, name)?;
    for line in auth_lines(&text) {
        match line {
            Line::Module { control, module } if module_name(&module) == MODULE => {
                return Some((None, control));
            }
            Line::Module { .. } => {}
            Line::Include(next) => {
                if let Some((_, control)) = find(root, &next, depth + 1, seen) {
                    return Some((Some(next), control));
                }
            }
        }
    }
    None
}

#[derive(Debug, PartialEq, Eq)]
enum Line {
    Module { control: String, module: String },
    Include(String),
}

/// The auth-relevant lines of one PAM file, in order.
fn auth_lines(text: &str) -> Vec<Line> {
    let mut out = Vec::new();
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut words = line.split_whitespace();
        let Some(first) = words.next() else {
            continue;
        };
        if first == "@include" {
            if let Some(next) = words.next() {
                out.push(Line::Include(next.to_string()));
            }
            continue;
        }
        // A leading '-' only silences a missing module.
        if !first.trim_start_matches('-').eq_ignore_ascii_case("auth") {
            continue;
        }
        let Some(mut control) = words.next().map(String::from) else {
            continue;
        };
        if control.starts_with('[') {
            while !control.ends_with(']') {
                let Some(w) = words.next() else {
                    break;
                };
                control.push(' ');
                control.push_str(w);
            }
        }
        let Some(module) = words.next() else {
            continue;
        };
        if control == "include" || control == "substack" {
            out.push(Line::Include(module.to_string()));
        } else {
            out.push(Line::Module {
                control,
                module: module.to_string(),
            });
        }
    }
    out
}

fn module_name(module: &str) -> &str {
    module.rsplit('/').next().unwrap_or(module)
}

/// authselect.conf: the first non-comment line is the profile, every later
/// one a feature.
fn parse_authselect(text: &str) -> Option<Authselect> {
    let mut lines = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'));
    let profile = lines.next()?.to_string();
    let fingerprint = lines.any(|l| l == "with-fingerprint");
    Some(Authselect {
        profile,
        fingerprint,
    })
}

/// One line for a stack: the state badge and a detail.
pub fn describe(w: &Wiring) -> (&'static str, String) {
    match w {
        Wiring::Direct(control) => ("On", control_note(control)),
        Wiring::Via { stack, control } => {
            ("On", format!("{} (via {stack})", control_note(control)))
        }
        Wiring::Absent => ("Off", "Password only.".into()),
    }
}

fn control_note(control: &str) -> String {
    match control {
        "sufficient" => "A fingerprint is enough; the password still works.".into(),
        "required" | "requisite" => "A fingerprint is required in addition to the password.".into(),
        other => format!("Control: {other}."),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch root with the given files; removed on drop.
    struct Root(PathBuf);

    impl Root {
        fn new(tag: &str, files: &[(&str, &str)]) -> Self {
            let dir = std::env::temp_dir().join(format!("dials-pam-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            for (path, text) in files {
                let p = dir.join(path);
                std::fs::create_dir_all(p.parent().unwrap()).unwrap();
                std::fs::write(p, text).unwrap();
            }
            Root(dir)
        }
    }

    impl Drop for Root {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Arch: fprintd ships the module; the admin adds it to sudo by hand,
    /// polkit-1 is only in the vendor directory.
    #[test]
    fn arch_layout() {
        let root = Root::new(
            "arch",
            &[
                ("usr/lib/security/pam_fprintd.so", ""),
                (
                    "etc/pam.d/sudo",
                    "#%PAM-1.0\nauth\t\tsufficient\tpam_fprintd.so\nauth\t\tinclude\t\tsystem-auth\naccount\t\tinclude\t\tsystem-auth\n",
                ),
                (
                    "etc/pam.d/system-auth",
                    "#%PAM-1.0\nauth       required                    pam_faillock.so      preauth\n-auth      [success=2 default=ignore]  pam_systemd_home.so\nauth       [success=1 default=bad]     pam_unix.so          try_first_pass nullok\naccount    required                    pam_unix.so\n",
                ),
                (
                    "etc/pam.d/system-local-login",
                    "auth      include   system-login\n",
                ),
                (
                    "etc/pam.d/system-login",
                    "auth required pam_shells.so\nauth include system-auth\n",
                ),
                ("usr/lib/pam.d/polkit-1", "auth include system-auth\n"),
                ("etc/pam.d/vigil-lock", "auth include system-local-login\n"),
            ],
        );
        let st = read(&root.0);
        assert_eq!(
            st.module,
            Some(root.0.join("usr/lib/security/pam_fprintd.so"))
        );
        assert_eq!(st.authselect, None);
        let get = |n: &str| {
            st.stacks
                .iter()
                .find(|s| s.name == n)
                .map(|s| s.wiring.clone())
        };
        assert_eq!(get("sudo"), Some(Wiring::Direct("sufficient".into())));
        assert_eq!(get("polkit-1"), Some(Wiring::Absent), "vendor dir is read");
        assert_eq!(get("vigil-lock"), Some(Wiring::Absent));
        assert_eq!(get("hyprlock"), None, "missing stacks are not listed");
        let names: Vec<_> = st.stacks.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "sudo",
                "polkit-1",
                "vigil-lock",
                "system-local-login",
                "system-auth"
            ],
            "display order follows STACKS"
        );
    }

    /// Fedora: authselect generates system-auth with pam_fprintd behind
    /// `with-fingerprint`; every stack reaches it through an include.
    #[test]
    fn fedora_layout() {
        let root = Root::new(
            "fedora",
            &[
                ("usr/lib64/security/pam_fprintd.so", ""),
                (
                    "etc/authselect/authselect.conf",
                    "local\nwith-fingerprint\nwith-silent-lastlog\n",
                ),
                (
                    "etc/pam.d/system-auth",
                    "# Generated by authselect\nauth        required                                     pam_env.so\nauth        [success=done default=ignore]                pam_fprintd.so\nauth        sufficient                                   pam_unix.so nullok\n",
                ),
                (
                    "etc/pam.d/sudo",
                    "#%PAM-1.0\nauth       include      system-auth\n",
                ),
                ("etc/pam.d/login", "auth       substack     system-auth\n"),
            ],
        );
        let st = read(&root.0);
        assert_eq!(
            st.module,
            Some(root.0.join("usr/lib64/security/pam_fprintd.so"))
        );
        assert_eq!(
            st.authselect,
            Some(Authselect {
                profile: "local".into(),
                fingerprint: true
            })
        );
        let via = Wiring::Via {
            stack: "system-auth".into(),
            control: "[success=done default=ignore]".into(),
        };
        let get = |n: &str| {
            st.stacks
                .iter()
                .find(|s| s.name == n)
                .map(|s| s.wiring.clone())
        };
        assert_eq!(get("sudo"), Some(via.clone()));
        assert_eq!(get("login"), Some(via));
    }

    #[test]
    fn no_module_and_include_loops_terminate() {
        let root = Root::new(
            "loop",
            &[
                ("etc/pam.d/sudo", "auth include system-auth\n"),
                (
                    "etc/pam.d/system-auth",
                    "auth include sudo\n@include ../../etc/passwd\n",
                ),
            ],
        );
        let st = read(&root.0);
        assert_eq!(st.module, None);
        assert!(st.stacks.iter().all(|s| s.wiring == Wiring::Absent));
    }

    #[test]
    fn debian_include_and_paths() {
        let lines = auth_lines(
            "@include common-auth\nauth [success=1 default=ignore] /lib/security/pam_fprintd.so max-tries=1\nsession include x\n",
        );
        assert_eq!(
            lines,
            vec![
                Line::Include("common-auth".into()),
                Line::Module {
                    control: "[success=1 default=ignore]".into(),
                    module: "/lib/security/pam_fprintd.so".into()
                },
            ]
        );
        assert_eq!(module_name("/lib/security/pam_fprintd.so"), MODULE);
    }

    #[test]
    fn descriptions() {
        assert_eq!(describe(&Wiring::Absent).0, "Off");
        let (badge, detail) = describe(&Wiring::Via {
            stack: "system-auth".into(),
            control: "sufficient".into(),
        });
        assert_eq!(badge, "On");
        assert!(detail.ends_with("(via system-auth)"));
    }
}
