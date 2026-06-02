/// User / group database and authentication for openv.
///
/// Passwords are stored as FNV-1a 64-bit hashes (fast, deterministic,
/// computable at const time — sufficient for a single-machine demo OS).
/// Replace with a proper KDF (bcrypt/argon2) when adding network services.

pub type Uid = u32;
pub type Gid = u32;

// ── Well-known IDs ─────────────────────────────────────────────────────────
pub const ROOT_UID:  Uid = 0;
pub const ROOT_GID:  Gid = 0;
pub const GUEST_UID: Uid = 1000;
pub const GUEST_GID: Gid = 1000;
/// Debian/Ubuntu convention: gid 27 = sudo group
pub const SUDO_GID:  Gid = 27;

// ── FNV-1a 64-bit (const-fn, usable in static initialisers) ───────────────
pub const fn fnv1a(data: &[u8]) -> u64 {
    let mut h: u64 = 14695981039346656037;
    let mut i = 0;
    while i < data.len() {
        h ^= data[i] as u64;
        h = h.wrapping_mul(1099511628211);
        i += 1;
    }
    h
}

pub fn verify_password(stored_hash: u64, plaintext: &[u8]) -> bool {
    fnv1a(plaintext) == stored_hash
}

// ── Static user database (/etc/passwd + /etc/shadow equivalent) ───────────
pub struct UserEntry {
    pub uid:           Uid,
    pub gid:           Gid,
    pub name:          &'static str,
    pub password_hash: u64,
    pub home:          &'static str,
    pub shell:         &'static str,
}

pub struct GroupEntry {
    pub gid:     Gid,
    pub name:    &'static str,
    pub members: &'static [Uid],
}

static USERS: &[UserEntry] = &[
    UserEntry {
        uid:           ROOT_UID,
        gid:           ROOT_GID,
        name:          "root",
        password_hash: fnv1a(b"root"),
        home:          "/root",
        shell:         "/sh",
    },
    UserEntry {
        uid:           GUEST_UID,
        gid:           GUEST_GID,
        name:          "guest",
        password_hash: fnv1a(b"guest"),
        home:          "/home/guest",
        shell:         "/sh",
    },
];

// ── Static group database (/etc/group equivalent) ─────────────────────────
static GROUPS: &[GroupEntry] = &[
    GroupEntry { gid: ROOT_GID,  name: "root",  members: &[ROOT_UID]           },
    GroupEntry { gid: GUEST_GID, name: "guest", members: &[GUEST_UID]          },
    // sudo group: both root and guest can escalate
    GroupEntry { gid: SUDO_GID,  name: "sudo",  members: &[ROOT_UID, GUEST_UID] },
];

// ── Lookup helpers ─────────────────────────────────────────────────────────
pub fn find_by_name(name: &str) -> Option<&'static UserEntry> {
    USERS.iter().find(|u| u.name == name)
}

pub fn find_by_uid(uid: Uid) -> Option<&'static UserEntry> {
    USERS.iter().find(|u| u.uid == uid)
}

pub fn find_group_by_name(name: &str) -> Option<&'static GroupEntry> {
    GROUPS.iter().find(|g| g.name == name)
}

pub fn find_group_by_gid(gid: Gid) -> Option<&'static GroupEntry> {
    GROUPS.iter().find(|g| g.gid == gid)
}

/// Returns every group the uid belongs to (including primary gid).
pub fn groups_of(uid: Uid) -> alloc::vec::Vec<Gid> {
    let mut result = alloc::vec::Vec::new();
    for g in GROUPS {
        if g.members.contains(&uid) {
            result.push(g.gid);
        }
    }
    result
}

// ── Authentication ─────────────────────────────────────────────────────────
/// Verify username + password. Returns the matching UserEntry on success.
pub fn authenticate<'a>(username: &str, password: &[u8]) -> Option<&'static UserEntry> {
    let user = find_by_name(username)?;
    if verify_password(user.password_hash, password) { Some(user) } else { None }
}

/// Verify a UID's password directly (used by sudo: verify current user's creds).
pub fn authenticate_uid(uid: Uid, password: &[u8]) -> bool {
    find_by_uid(uid)
        .map_or(false, |u| verify_password(u.password_hash, password))
}

// ── Privilege checks ───────────────────────────────────────────────────────
pub fn in_group(uid: Uid, gid: Gid) -> bool {
    GROUPS.iter()
        .find(|g| g.gid == gid)
        .map_or(false, |g| g.members.contains(&uid))
}

/// Returns true if uid is allowed to use sudo (root or member of sudo group).
pub fn can_sudo(uid: Uid) -> bool {
    uid == ROOT_UID || in_group(uid, SUDO_GID)
}

/// Returns true if euid == 0.
#[inline]
pub fn is_root(euid: Uid) -> bool {
    euid == ROOT_UID
}
