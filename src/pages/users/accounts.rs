//! Local accounts: `/etc/passwd` is the list (AccountsService's cache omits
//! users who never logged in), AccountsService adds real name, avatar, admin
//! and lock state and does every write. Like `system`, writes can raise a
//! polkit prompt, so only the admin worker calls the bus functions here.

use std::ffi::{CStr, CString};

use zbus::blocking::{Connection, Proxy, proxy::Builder};
use zbus::proxy::{CacheProperties, MethodFlags};
use zbus::zvariant::OwnedObjectPath;

const ACCOUNTS_DEST: &str = "org.freedesktop.Accounts";
const ACCOUNTS_PATH: &str = "/org/freedesktop/Accounts";
const USER_IFACE: &str = "org.freedesktop.Accounts.User";

/// Shells that mean "not a login account" (same set the vigil greeter skips).
const NOLOGIN_SHELLS: [&str; 6] = ["nologin", "false", "true", "sync", "shutdown", "halt"];

#[derive(Clone, Debug, PartialEq)]
pub struct PasswdEntry {
    pub name: String,
    pub uid: u32,
    pub gid: u32,
    pub gecos: String,
    pub home: String,
    pub shell: String,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LoginDefs {
    pub uid_min: u32,
    pub uid_max: u32,
    pub gid_min: u32,
    pub gid_max: u32,
}

impl Default for LoginDefs {
    fn default() -> Self {
        LoginDefs {
            uid_min: 1000,
            uid_max: 60000,
            gid_min: 1000,
            gid_max: 60000,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct GroupEntry {
    pub name: String,
    pub gid: u32,
    pub members: Vec<String>,
}

/// Groups worth offering as checkboxes when they exist. wheel/sudo are left
/// out: "Administrator" is the control for those.
pub const COMMON_GROUPS: [&str; 13] = [
    "audio", "video", "input", "render", "dialout", "uucp", "lp", "scanner", "plugdev", "docker",
    "libvirt", "kvm", "gamemode",
];

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Account {
    pub name: String,
    pub uid: u32,
    pub gid: u32,
    pub real_name: String,
    pub admin: bool,
    /// Empty when there is no avatar file.
    pub icon: String,
    pub locked: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    pub accounts: Vec<Account>,
    /// False when AccountsService isn't installed: the list is passwd-only
    /// and every write is unavailable.
    pub service: bool,
    /// Uids with a logind user object (logged in, or lingering).
    pub logged_in: Vec<u32>,
    pub groups: Vec<GroupEntry>,
    pub login_defs: LoginDefs,
    /// Primary gids of every passwd entry, people or not.
    pub primary_gids: Vec<u32>,
}

pub fn parse_passwd(text: &str) -> Vec<PasswdEntry> {
    text.lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .filter_map(|line| {
            let f: Vec<&str> = line.split(':').collect();
            if f.len() < 7 || f[0].is_empty() {
                return None;
            }
            Some(PasswdEntry {
                name: f[0].to_string(),
                uid: f[2].parse().ok()?,
                gid: f[3].parse().ok()?,
                gecos: f[4].to_string(),
                home: f[5].to_string(),
                shell: f[6].to_string(),
            })
        })
        .collect()
}

pub fn parse_group(text: &str) -> Vec<GroupEntry> {
    text.lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .filter_map(|line| {
            let f: Vec<&str> = line.split(':').collect();
            if f.len() < 4 || f[0].is_empty() {
                return None;
            }
            Some(GroupEntry {
                name: f[0].to_string(),
                gid: f[2].parse().ok()?,
                members: f[3]
                    .split(',')
                    .filter(|m| !m.is_empty())
                    .map(String::from)
                    .collect(),
            })
        })
        .collect()
}

/// The checkboxes for one user: common groups that exist, plus groups an
/// admin made (gid in the user range) that aren't anyone's primary group.
pub fn group_choices<'a>(
    groups: &'a [GroupEntry],
    defs: &LoginDefs,
    primary_gids: &[u32],
) -> Vec<&'a GroupEntry> {
    let mut out: Vec<&GroupEntry> = COMMON_GROUPS
        .iter()
        .filter_map(|name| groups.iter().find(|g| g.name == *name))
        .collect();
    // A common group can sit in the user gid range too (plugdev made by
    // hand on Arch); it's already listed, so don't offer it twice.
    let mut custom: Vec<&GroupEntry> = groups
        .iter()
        .filter(|g| {
            is_custom_group(g, defs, primary_gids) && !COMMON_GROUPS.contains(&g.name.as_str())
        })
        .collect();
    custom.sort_by(|a, b| a.name.cmp(&b.name));
    out.extend(custom);
    out
}

/// A group an administrator made by hand, as opposed to a system group or
/// a user's personal group.
pub fn is_custom_group(g: &GroupEntry, defs: &LoginDefs, primary_gids: &[u32]) -> bool {
    (defs.gid_min..=defs.gid_max).contains(&g.gid) && !primary_gids.contains(&g.gid)
}

/// The groups AccountsService treats as "Administrator" (distros use one).
const ADMIN_GROUPS: [&str; 3] = ["wheel", "sudo", "admin"];

/// Admin by group membership: the fallback when AccountsService (whose
/// AccountType is derived from the same groups) isn't there to ask.
pub fn is_admin_by_groups(name: &str, groups: &[GroupEntry]) -> bool {
    groups
        .iter()
        .any(|g| ADMIN_GROUPS.contains(&g.name.as_str()) && g.members.iter().any(|m| m == name))
}

pub fn can_delete_group(
    g: &GroupEntry,
    defs: &LoginDefs,
    primary_gids: &[u32],
) -> Result<(), String> {
    if primary_gids.contains(&g.gid) {
        return Err(format!("{} is someone's primary group.", g.name));
    }
    if g.gid < defs.gid_min {
        return Err(format!("{} is a system group.", g.name));
    }
    Ok(())
}

pub fn parse_login_defs(text: &str) -> LoginDefs {
    let mut defs = LoginDefs::default();
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('#') {
            continue;
        }
        let mut it = line.split_whitespace();
        let (Some(key), Some(value)) = (it.next(), it.next()) else {
            continue;
        };
        let Ok(value) = value.parse() else {
            continue;
        };
        match key {
            "UID_MIN" => defs.uid_min = value,
            "UID_MAX" => defs.uid_max = value,
            "GID_MIN" => defs.gid_min = value,
            "GID_MAX" => defs.gid_max = value,
            _ => {}
        }
    }
    defs
}

/// A person's account rather than a service account.
pub fn is_human(e: &PasswdEntry, defs: &LoginDefs) -> bool {
    let shell = e.shell.rsplit('/').next().unwrap_or("");
    (defs.uid_min..=defs.uid_max).contains(&e.uid)
        && !shell.is_empty()
        && !NOLOGIN_SHELLS.contains(&shell)
}

pub fn read_login_defs() -> LoginDefs {
    std::fs::read_to_string("/etc/login.defs")
        .map(|t| parse_login_defs(&t))
        .unwrap_or_default()
}

fn human_entries(all: &[PasswdEntry], defs: &LoginDefs) -> Vec<PasswdEntry> {
    let mut list: Vec<PasswdEntry> = all.iter().filter(|e| is_human(e, defs)).cloned().collect();
    list.sort_by_key(|e| e.uid);
    list.dedup_by_key(|e| e.uid);
    list
}

/// The GECOS full-name field (everything before the first comma).
pub fn gecos_name(gecos: &str) -> &str {
    gecos.split(',').next().unwrap_or("")
}

/// shadow-utils' portable rule: lowercase letter or `_` first, then
/// lowercase letters, digits, `_` and `-`, optionally a trailing `$`.
pub fn validate_username(name: &str, taken: &[String]) -> Result<(), String> {
    if name.is_empty() {
        return Err("Enter a username.".into());
    }
    if name.len() > 32 {
        return Err("Usernames are at most 32 characters.".into());
    }
    let body = name.strip_suffix('$').unwrap_or(name);
    let mut chars = body.chars();
    let first_ok = chars
        .next()
        .is_some_and(|c| c.is_ascii_lowercase() || c == '_');
    if !first_ok {
        return Err("A username starts with a lowercase letter or _.".into());
    }
    if !chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-') {
        return Err("Use only lowercase letters, digits, _ and -.".into());
    }
    if taken.iter().any(|t| t == name) {
        return Err(format!("{name} already exists."));
    }
    Ok(())
}

pub fn validate_real_name(name: &str) -> Result<(), String> {
    if name.contains(':') || name.contains(',') || name.chars().any(char::is_control) {
        return Err("A full name can't contain : , or control characters.".into());
    }
    Ok(())
}

pub fn validate_new_password(password: &str, confirm: &str) -> Result<(), String> {
    if password.is_empty() {
        return Err("Enter a password.".into());
    }
    if password != confirm {
        return Err("The passwords don't match.".into());
    }
    Ok(())
}

/// What the viewer may do to `target`. Kept pure so the guards are testable;
/// the UI disables controls from this and the worker re-checks before writing.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Allowed {
    pub delete: bool,
    pub change_admin: bool,
    pub lock: bool,
    pub edit_profile: bool,
    pub set_password: bool,
    /// Self-change through `passwd`, which asks for the current password.
    pub needs_current_password: bool,
    /// Why `delete` is false, for the UI to say.
    pub delete_reason: &'static str,
}

pub fn allowed(
    target: &Account,
    me_uid: u32,
    me_admin: bool,
    admins: usize,
    logged_in: &[u32],
) -> Allowed {
    let is_me = target.uid == me_uid;
    let last_admin = target.admin && admins <= 1;
    let delete_reason = if !me_admin {
        "Only an administrator can delete accounts."
    } else if is_me {
        "You can't delete the account you're using."
    } else if logged_in.contains(&target.uid) {
        "This user is logged in. Log them out first."
    } else if last_admin {
        "This is the last administrator."
    } else {
        ""
    };
    Allowed {
        delete: delete_reason.is_empty(),
        change_admin: me_admin && !last_admin,
        lock: me_admin && !is_me,
        edit_profile: me_admin || is_me,
        set_password: me_admin || is_me,
        needs_current_password: is_me && !me_admin,
        delete_reason,
    }
}

pub fn initials(real_name: &str, user_name: &str) -> String {
    let source = if real_name.trim().is_empty() {
        user_name
    } else {
        real_name
    };
    source
        .split_whitespace()
        .filter_map(|w| w.chars().next())
        .take(2)
        .flat_map(char::to_uppercase)
        .collect()
}

// --- AccountsService -------------------------------------------------------

fn accounts(conn: &Connection) -> zbus::Result<Proxy<'_>> {
    Builder::new(conn)
        .destination(ACCOUNTS_DEST)?
        .path(ACCOUNTS_PATH)?
        .interface(ACCOUNTS_DEST)?
        .cache_properties(CacheProperties::No)
        .build()
}

fn user_proxy(conn: &Connection, path: OwnedObjectPath) -> zbus::Result<Proxy<'_>> {
    Builder::new(conn)
        .destination(ACCOUNTS_DEST)?
        .path(path)?
        .interface(USER_IFACE)?
        .cache_properties(CacheProperties::No)
        .build()
}

fn user_by_uid(conn: &Connection, uid: u32) -> zbus::Result<Proxy<'_>> {
    let path: OwnedObjectPath = accounts(conn)?.call("FindUserById", &(i64::from(uid),))?;
    user_proxy(conn, path)
}

fn write<B>(p: &Proxy<'_>, method: &str, body: &B) -> Result<(), String>
where
    B: serde::Serialize + zbus::zvariant::DynamicType,
{
    p.call_with_flags::<_, _, ()>(method, MethodFlags::AllowInteractiveAuth.into(), body)
        .map(drop)
        .map_err(|e| crate::admin::describe(&e))
}

pub fn read(conn: &Connection) -> Snapshot {
    let defs = read_login_defs();
    let all = std::fs::read_to_string("/etc/passwd")
        .map(|t| parse_passwd(&t))
        .unwrap_or_default();
    let service =
        accounts(conn).and_then(|p| p.call::<_, _, Vec<OwnedObjectPath>>("ListCachedUsers", &()));
    let mut snap = Snapshot {
        service: service.is_ok(),
        logged_in: logged_in(conn),
        groups: std::fs::read_to_string("/etc/group")
            .map(|t| parse_group(&t))
            .unwrap_or_default(),
        login_defs: defs,
        primary_gids: all.iter().map(|e| e.gid).collect(),
        ..Default::default()
    };
    for e in human_entries(&all, &defs) {
        let mut account = Account {
            name: e.name.clone(),
            uid: e.uid,
            gid: e.gid,
            real_name: gecos_name(&e.gecos).to_string(),
            admin: is_admin_by_groups(&e.name, &snap.groups),
            ..Default::default()
        };
        if snap.service
            && let Ok(p) = user_by_uid(conn, e.uid)
        {
            account.real_name = p.get_property("RealName").unwrap_or(account.real_name);
            account.admin = p
                .get_property::<i32>("AccountType")
                .map(|t| t == 1)
                .unwrap_or(account.admin);
            account.locked = p.get_property("Locked").unwrap_or(false);
            let icon: String = p.get_property("IconFile").unwrap_or_default();
            if std::path::Path::new(&icon).is_file() {
                account.icon = icon;
            }
        }
        snap.accounts.push(account);
    }
    snap
}

fn logged_in(conn: &Connection) -> Vec<u32> {
    Builder::<Proxy>::new(conn)
        .destination("org.freedesktop.login1")
        .and_then(|b| b.path("/org/freedesktop/login1"))
        .and_then(|b| b.interface("org.freedesktop.login1.Manager"))
        .and_then(|b| b.cache_properties(CacheProperties::No).build())
        .and_then(|p| p.call::<_, _, Vec<(u32, String, OwnedObjectPath)>>("ListUsers", &()))
        .map(|users| users.into_iter().map(|(uid, _, _)| uid).collect())
        .unwrap_or_default()
}

/// Returns the new account's uid.
pub fn create(conn: &Connection, name: &str, real_name: &str, admin: bool) -> Result<u32, String> {
    let p = accounts(conn).map_err(|e| crate::admin::describe(&e))?;
    let path: OwnedObjectPath = p
        .call_with_flags::<_, _, OwnedObjectPath>(
            "CreateUser",
            MethodFlags::AllowInteractiveAuth.into(),
            &(name, real_name, i32::from(admin)),
        )
        .map_err(|e| crate::admin::describe(&e))?
        .ok_or("AccountsService returned no user")?;
    let user = user_proxy(conn, path).map_err(|e| crate::admin::describe(&e))?;
    user.get_property::<u64>("Uid")
        .map(|uid| uid as u32)
        .map_err(|e| crate::admin::describe(&e))
}

pub fn set_real_name(conn: &Connection, uid: u32, name: &str) -> Result<(), String> {
    validate_real_name(name)?;
    let p = user_by_uid(conn, uid).map_err(|e| crate::admin::describe(&e))?;
    write(&p, "SetRealName", &(name,))
}

pub fn set_admin(conn: &Connection, uid: u32, admin: bool) -> Result<(), String> {
    let p = user_by_uid(conn, uid).map_err(|e| crate::admin::describe(&e))?;
    write(&p, "SetAccountType", &(i32::from(admin),))
}

pub fn set_locked(conn: &Connection, uid: u32, locked: bool) -> Result<(), String> {
    let p = user_by_uid(conn, uid).map_err(|e| crate::admin::describe(&e))?;
    write(&p, "SetLocked", &(locked,))
}

/// AccountsService copies the file into its own icons directory, so the
/// source may live anywhere the daemon (root) can read.
pub fn set_icon(conn: &Connection, uid: u32, file: &str) -> Result<(), String> {
    if !std::path::Path::new(file).is_file() {
        return Err(format!("{file}: no such file"));
    }
    let p = user_by_uid(conn, uid).map_err(|e| crate::admin::describe(&e))?;
    write(&p, "SetIconFile", &(file,))
}

/// Admin path: AccountsService stores a crypt(3) hash, so hash here with the
/// system's default method. The plaintext buffer is wiped by the caller.
pub fn set_password(conn: &Connection, uid: u32, password: &str) -> Result<(), String> {
    let hash = crypt_default(password)?;
    let p = user_by_uid(conn, uid).map_err(|e| crate::admin::describe(&e))?;
    write(&p, "SetPassword", &(hash.as_str(), ""))
}

pub fn delete(conn: &Connection, uid: u32, remove_files: bool) -> Result<(), String> {
    let p = accounts(conn).map_err(|e| crate::admin::describe(&e))?;
    write(&p, "DeleteUser", &(i64::from(uid), remove_files))
}

// --- groups (pkexec) -------------------------------------------------------
// AccountsService manages only admin membership, so other group changes run
// the stock shadow tools under pkexec: no privileged code of our own, at the
// cost of polkit's generic "run a program as root" prompt.

fn tool(name: &str) -> String {
    ["/usr/bin", "/usr/sbin", "/bin", "/sbin"]
        .iter()
        .map(|dir| format!("{dir}/{name}"))
        .find(|p| std::path::Path::new(p).is_file())
        .unwrap_or_else(|| name.to_string())
}

fn pkexec(args: &[&str]) -> Result<(), String> {
    let out = std::process::Command::new("pkexec")
        .args(args)
        .env("LC_ALL", "C")
        .output()
        .map_err(|e| format!("pkexec: {e}"))?;
    match out.status.code() {
        Some(0) => Ok(()),
        // pkexec: 126 = dialog dismissed / not authorized, 127 = auth failed.
        Some(126 | 127) => {
            Err("Not authorized — the password prompt was cancelled or denied.".into())
        }
        _ => {
            let err = String::from_utf8_lossy(&out.stderr);
            let line = err
                .lines()
                .rev()
                .find(|l| !l.trim().is_empty())
                .unwrap_or("failed");
            Err(line.trim().to_string())
        }
    }
}

pub fn set_group_member(user: &str, group: &str, member: bool) -> Result<(), String> {
    let flag = if member { "-a" } else { "-d" };
    pkexec(&[&tool("gpasswd"), flag, user, group])
}

pub fn create_group(name: &str) -> Result<(), String> {
    pkexec(&[&tool("groupadd"), "--", name])
}

pub fn delete_group(name: &str) -> Result<(), String> {
    pkexec(&[&tool("groupdel"), "--", name])
}

// --- libxcrypt -------------------------------------------------------------

#[link(name = "crypt")]
unsafe extern "C" {
    fn crypt_gensalt_ra(
        prefix: *const libc::c_char,
        count: libc::c_ulong,
        rbytes: *const libc::c_char,
        nrbytes: libc::c_int,
    ) -> *mut libc::c_char;
    fn crypt_ra(
        phrase: *const libc::c_char,
        setting: *const libc::c_char,
        data: *mut *mut libc::c_void,
        size: *mut libc::c_int,
    ) -> *mut libc::c_char;
}

/// Hash with libxcrypt's default method (yescrypt on current Fedora and
/// Arch) and a fresh random salt, as `passwd` would.
pub fn crypt_default(password: &str) -> Result<String, String> {
    let phrase = CString::new(password).map_err(|_| "A password can't contain a NUL byte.")?;
    // SAFETY: NULL prefix/rbytes ask libxcrypt for its default method and
    // OS randomness; the returned buffers are malloc'd and freed below.
    unsafe {
        let setting = crypt_gensalt_ra(std::ptr::null(), 0, std::ptr::null(), 0);
        if setting.is_null() {
            return Err("crypt_gensalt failed".into());
        }
        let mut data: *mut libc::c_void = std::ptr::null_mut();
        let mut size: libc::c_int = 0;
        let out = crypt_ra(phrase.as_ptr(), setting, &mut data, &mut size);
        libc::free(setting.cast());
        let hash = (!out.is_null())
            .then(|| CStr::from_ptr(out).to_string_lossy().into_owned())
            .filter(|h| !h.starts_with('*'));
        if !data.is_null() {
            // crypt_data holds the phrase-derived state; clear it before free.
            std::ptr::write_bytes(data.cast::<u8>(), 0, size as usize);
            libc::free(data);
        }
        let mut bytes = phrase.into_bytes();
        wipe(&mut bytes);
        hash.ok_or_else(|| "crypt failed".into())
    }
}

/// Zero a secret before it is freed. Volatile so the store isn't elided.
pub fn wipe(bytes: &mut [u8]) {
    for b in bytes.iter_mut() {
        // SAFETY: `b` is a valid, exclusive reference.
        unsafe { std::ptr::write_volatile(b, 0) };
    }
    std::sync::atomic::compiler_fence(std::sync::atomic::Ordering::SeqCst);
}

pub fn wipe_string(s: String) {
    let mut bytes = s.into_bytes();
    wipe(&mut bytes);
}

#[cfg(test)]
mod tests {
    use super::*;

    const PASSWD: &str = "\
root:x:0:0::/root:/bin/bash
nobody:x:65534:65534:Kernel Overflow User:/:/usr/bin/nologin
mason:x:1000:1000:Mason Rhodes,,,:/home/mason:/bin/zsh
# comment
svc:x:1001:1001::/var/lib/svc:/usr/sbin/nologin
broken line
ada:x:1002:1002::/home/ada:/bin/bash
";

    #[test]
    fn passwd_and_humans() {
        let entries = parse_passwd(PASSWD);
        assert_eq!(entries.len(), 5);
        let defs = LoginDefs::default();
        let humans: Vec<&str> = entries
            .iter()
            .filter(|e| is_human(e, &defs))
            .map(|e| e.name.as_str())
            .collect();
        assert_eq!(humans, ["mason", "ada"]);
        assert_eq!(gecos_name(&entries[2].gecos), "Mason Rhodes");
    }

    const GROUP: &str = "\
wheel:x:998:mason
audio:x:995:
video:x:983:mason,ada
mason:x:1000:
ada:x:1002:
band:x:1500:ada
";

    #[test]
    fn groups_and_choices() {
        let groups = parse_group(GROUP);
        assert_eq!(groups[2].members, ["mason", "ada"]);
        assert!(groups[1].members.is_empty());
        let defs = LoginDefs::default();
        let primary = [1000, 1002];
        let names: Vec<&str> = group_choices(&groups, &defs, &primary)
            .iter()
            .map(|g| g.name.as_str())
            .collect();
        // Common groups in COMMON_GROUPS order, then custom ones; no wheel,
        // no personal groups.
        assert_eq!(names, ["audio", "video", "band"]);
        assert!(is_admin_by_groups("mason", &groups));
        assert!(!is_admin_by_groups("ada", &groups));
        assert!(can_delete_group(&groups[5], &defs, &primary).is_ok());
        assert!(can_delete_group(&groups[3], &defs, &primary).is_err());
        assert!(can_delete_group(&groups[1], &defs, &primary).is_err());

        // A common group with a user-range gid is listed once, in common order.
        let mut groups = groups;
        groups.push(GroupEntry {
            name: "plugdev".into(),
            gid: 1001,
            members: vec!["mason".into()],
        });
        let names: Vec<&str> = group_choices(&groups, &defs, &primary)
            .iter()
            .map(|g| g.name.as_str())
            .collect();
        assert_eq!(names, ["audio", "video", "plugdev", "band"]);
    }

    #[test]
    fn login_defs_overrides_and_defaults() {
        let defs = parse_login_defs("# c\nUID_MIN\t\t 2000\nUID_MAX 3000\nGID_MIN bad\n");
        assert_eq!(
            defs,
            LoginDefs {
                uid_min: 2000,
                uid_max: 3000,
                gid_min: 1000,
                gid_max: 60000,
            }
        );
        assert_eq!(parse_login_defs(""), LoginDefs::default());
    }

    #[test]
    fn username_rules() {
        let taken = vec!["mason".to_string()];
        assert!(validate_username("ada", &taken).is_ok());
        assert!(validate_username("_svc-1", &taken).is_ok());
        assert!(validate_username("box$", &taken).is_ok());
        assert!(validate_username("", &taken).is_err());
        assert!(validate_username("Ada", &taken).is_err());
        assert!(validate_username("1ada", &taken).is_err());
        assert!(validate_username("a.b", &taken).is_err());
        assert!(validate_username("mason", &taken).is_err());
        assert!(validate_username(&"a".repeat(33), &taken).is_err());
    }

    #[test]
    fn real_name_and_password_rules() {
        assert!(validate_real_name("Ada Lovelace").is_ok());
        assert!(validate_real_name("José Núñez").is_ok());
        assert!(validate_real_name("a:b").is_err());
        assert!(validate_real_name("a,b").is_err());
        assert!(validate_new_password("x", "x").is_ok());
        assert!(validate_new_password("", "").is_err());
        assert!(validate_new_password("x", "y").is_err());
    }

    fn acct(uid: u32, admin: bool) -> Account {
        Account {
            name: format!("u{uid}"),
            uid,
            admin,
            ..Default::default()
        }
    }

    #[test]
    fn guards() {
        // Admin viewing another, logged-out standard user: everything.
        let a = allowed(&acct(1002, false), 1000, true, 1, &[1000]);
        assert!(
            a.delete && a.change_admin && a.lock && a.set_password && !a.needs_current_password
        );
        // Self: no delete, no lock.
        let a = allowed(&acct(1000, true), 1000, true, 2, &[1000]);
        assert!(!a.delete && !a.lock && a.change_admin);
        // Logged-in other user: no delete.
        let a = allowed(&acct(1002, false), 1000, true, 1, &[1000, 1002]);
        assert!(!a.delete);
        assert!(a.delete_reason.contains("logged in"));
        // Last admin: can't demote or delete.
        let a = allowed(&acct(1002, true), 1000, true, 1, &[]);
        assert!(!a.change_admin && !a.delete);
        // Standard viewer: only own profile and password, via passwd.
        let a = allowed(&acct(1002, false), 1002, false, 1, &[1002]);
        assert!(a.edit_profile && a.set_password && a.needs_current_password);
        assert!(!a.delete && !a.change_admin && !a.lock);
        let a = allowed(&acct(1000, true), 1002, false, 1, &[]);
        assert!(!a.edit_profile && !a.set_password);
    }

    /// Live: read this machine's accounts. `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn live_read() {
        let conn = Connection::system().expect("system bus");
        let snap = read(&conn);
        eprintln!(
            "service={} logged_in={:?} groups={} custom={:?}",
            snap.service,
            snap.logged_in,
            snap.groups.len(),
            snap.groups
                .iter()
                .filter(|g| is_custom_group(g, &snap.login_defs, &snap.primary_gids))
                .map(|g| &g.name)
                .collect::<Vec<_>>()
        );
        for a in &snap.accounts {
            eprintln!("{a:?}");
        }
        assert!(!snap.accounts.is_empty());
    }

    #[test]
    fn initials_prefer_real_name() {
        assert_eq!(initials("Ada Lovelace", "ada"), "AL");
        assert_eq!(initials("  ", "mason"), "M");
        assert_eq!(initials("Cher", "c"), "C");
        assert_eq!(initials("ana maría lópez", "x"), "AM");
    }

    #[test]
    fn crypt_uses_system_default_and_verifies() {
        let hash = crypt_default("correct horse").expect("hash");
        assert!(hash.starts_with('$'), "{hash}");
        // Re-hashing with the hash as setting must reproduce it.
        let phrase = CString::new("correct horse").unwrap();
        let setting = CString::new(hash.clone()).unwrap();
        let again = unsafe {
            let mut data: *mut libc::c_void = std::ptr::null_mut();
            let mut size = 0;
            let out = crypt_ra(phrase.as_ptr(), setting.as_ptr(), &mut data, &mut size);
            let s = CStr::from_ptr(out).to_string_lossy().into_owned();
            libc::free(data);
            s
        };
        assert_eq!(again, hash);
        assert_ne!(
            crypt_default("correct horse").unwrap(),
            hash,
            "fresh salt each time"
        );
    }
}
