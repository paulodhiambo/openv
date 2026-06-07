//! # User / Group Database and Authentication
//!
//! This module provides a simple user and group database for OpenV.
//! Passwords are stored as FNV-1a 64-bit hashes (fast, deterministic,
//! computable at `const` time — sufficient for a single-machine demo
//! OS). Replace with a proper KDF (bcrypt/argon2) when adding network
//! services.
//!
//! ## Overview
//!
//! The module provides:
//!
//! - **User IDs** ([`Uid`]) and **Group IDs** ([`Gid`])
//!  - **Well-known IDs**: [`ROOT_UID`], [`ROOT_GID`], [`GUEST_UID`], [`GUEST_GID`], [`SUDO_GID`]
//!  - **FNV-1a hash function**: [`fnv1a`]
//!  - **User database**: [`UserEntry`] and the [`USERS`] static array
//!  - **Group database**: [`GroupEntry`] and the [`GROUPS`] static array
//!  - **Lookup functions**: [`find_by_name`], [`find_by_uid`], etc.
//!  - **Authentication**: [`authenticate`], [`authenticate_uid`]
//!  - **Privilege checks**: [`in_group`], [`can_sudo`], [`is_root`]
//!
//! ## Security Note
//!
//! FNV-1a is a fast, non-cryptographic hash function. It is suitable
//! for demo purposes but should be replaced with a proper password
//! hashing function (bcrypt, argon2) for any real-world use.

/// User ID type.
pub type Uid = u32;
/// Group ID type.
pub type Gid = u32;

// ── Well-known IDs ─────────────────────────────────────────────────────────

/// Root user ID.
pub const ROOT_UID: Uid = 0;
/// Root group ID.
pub const ROOT_GID: Gid = 0;
/// Guest user ID (UID 1000, the default for the first non-root user on most Linux systems).
pub const GUEST_UID: Uid = 1000;
/// Guest group ID (GID 1000, matching the guest user).
pub const GUEST_GID: Gid = 1000;
/// Debian/Ubuntu convention: GID 27 = sudo group.
pub const SUDO_GID: Gid = 27;

// ── FNV-1a 64-bit (const-fn, usable in static initialisers) ───────────────

/// Computes the FNV-1a 64-bit hash of a byte slice.
///
/// This is a `const` function, so it can be used in static initializers.
///
/// # Arguments
///
/// * `data` - The data to hash.
///
/// # Returns
///
/// The 64-bit FNV-1a hash.
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

/// Verifies a password against a stored FNV-1a hash.
///
/// # Arguments
///
/// * `stored_hash` - The stored password hash.
/// * `plaintext` - The plaintext password to verify.
///
/// # Returns
///
/// `true` if the password matches, `false` otherwise.
pub fn verify_password(stored_hash: u64, plaintext: &[u8]) -> bool {
    fnv1a(plaintext) == stored_hash
}

// ── Static user database (/etc/passwd + /etc/shadow equivalent) ───────────

/// A user entry in the user database.
///
/// # Fields
///
/// * `uid` - User ID.
/// * `gid` - Primary group ID.
/// * `name` - Username.
/// * `password_hash` - FNV-1a hash of the password.
/// * `home` - Home directory path.
/// * `shell` - Login shell path.
pub struct UserEntry {
    /// User ID.
    pub uid: Uid,
    /// Primary group ID.
    pub gid: Gid,
    /// Username.
    pub name: &'static str,
    /// FNV-1a hash of the password.
    pub password_hash: u64,
    /// Home directory path.
    pub home: &'static str,
    /// Login shell path.
    pub shell: &'static str,
}

/// A group entry in the group database.
///
/// # Fields
///
/// * `gid` - Group ID.
/// * `name` - Group name.
/// * `members` - List of user IDs that are members of this group.
pub struct GroupEntry {
    /// Group ID.
    pub gid: Gid,
    /// Group name.
    pub name: &'static str,
    /// List of user IDs that are members of this group.
    pub members: &'static [Uid],
}

/// Static user database.
///
/// Contains the root and guest users. The root user has password
/// "root" and the guest user has password "guest".
static USERS: &[UserEntry] = &[
    UserEntry {
        uid: ROOT_UID,
        gid: ROOT_GID,
        name: "root",
        password_hash: fnv1a(b"root"),
        home: "/root",
        shell: "/sh",
    },
    UserEntry {
        uid: GUEST_UID,
        gid: GUEST_GID,
        name: "guest",
        password_hash: fnv1a(b"guest"),
        home: "/home/guest",
        shell: "/sh",
    },
];

// ── Static group database (/etc/group equivalent) ─────────────────────────

/// Static group database.
///
/// Contains the root, guest, and sudo groups. The sudo group includes
/// both root and guest users.
static GROUPS: &[GroupEntry] = &[
    GroupEntry {
        gid: ROOT_GID,
        name: "root",
        members: &[ROOT_UID],
    },
    GroupEntry {
        gid: GUEST_GID,
        name: "guest",
        members: &[GUEST_UID],
    },
    // sudo group: both root and guest can escalate
    GroupEntry {
        gid: SUDO_GID,
        name: "sudo",
        members: &[ROOT_UID, GUEST_UID],
    },
];

// ── Lookup helpers ─────────────────────────────────────────────────────────

/// Looks up a user by name.
///
/// # Arguments
///
/// * `name` - The username to look up.
///
/// # Returns
///
/// `Some(&UserEntry)` if the user exists, `None` otherwise.
pub fn find_by_name(name: &str) -> Option<&'static UserEntry> {
    USERS.iter().find(|u| u.name == name)
}

/// Looks up a user by UID.
///
/// # Arguments
///
/// * `uid` - The UID to look up.
///
/// # Returns
///
/// `Some(&UserEntry)` if the user exists, `None` otherwise.
pub fn find_by_uid(uid: Uid) -> Option<&'static UserEntry> {
    USERS.iter().find(|u| u.uid == uid)
}

/// Looks up a group by name.
///
/// # Arguments
///
/// * `name` - The group name to look up.
///
/// # Returns
///
/// `Some(&GroupEntry)` if the group exists, `None` otherwise.
pub fn find_group_by_name(name: &str) -> Option<&'static GroupEntry> {
    GROUPS.iter().find(|g| g.name == name)
}

/// Looks up a group by GID.
///
/// # Arguments
///
/// * `gid` - The GID to look up.
///
/// # Returns
///
/// `Some(&GroupEntry)` if the group exists, `None` otherwise.
pub fn find_group_by_gid(gid: Gid) -> Option<&'static GroupEntry> {
    GROUPS.iter().find(|g| g.gid == gid)
}

/// Returns every group the UID belongs to (including primary GID).
///
/// # Arguments
///
/// * `uid` - The UID to look up.
///
/// # Returns
///
/// A `Vec<Gid>` of all group IDs the user belongs to.
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

/// Verifies a username and password. Returns the matching [`UserEntry`] on success.
///
/// # Arguments
///
/// * `username` - The username.
/// * `password` - The plaintext password.
///
/// # Returns
///
/// `Some(&UserEntry)` if authentication succeeds, `None` otherwise.
pub fn authenticate(username: &str, password: &[u8]) -> Option<&'static UserEntry> {
    let user = find_by_name(username)?;
    if verify_password(user.password_hash, password) {
        Some(user)
    } else {
        None
    }
}

/// Verifies a UID's password directly (used by `sudo` to verify the
/// current user's credentials).
///
/// # Arguments
///
/// * `uid` - The UID to authenticate.
/// * `password` - The plaintext password.
///
/// # Returns
///
/// `true` if the password matches, `false` otherwise.
pub fn authenticate_uid(uid: Uid, password: &[u8]) -> bool {
    find_by_uid(uid).is_some_and(|u| verify_password(u.password_hash, password))
}

// ── Privilege checks ───────────────────────────────────────────────────────

/// Checks if a UID is a member of a group.
///
/// # Arguments
///
/// * `uid` - The UID to check.
/// * `gid` - The GID to check.
///
/// # Returns
///
/// `true` if the UID is a member of the group, `false` otherwise.
pub fn in_group(uid: Uid, gid: Gid) -> bool {
    GROUPS
        .iter()
        .find(|g| g.gid == gid)
        .is_some_and(|g| g.members.contains(&uid))
}

/// Returns `true` if the UID is allowed to use `sudo` (root or member of the `sudo` group).
///
/// # Arguments
///
/// * `uid` - The UID to check.
///
/// # Returns
///
/// `true` if the UID can use `sudo`, `false` otherwise.
pub fn can_sudo(uid: Uid) -> bool {
    uid == ROOT_UID || in_group(uid, SUDO_GID)
}

/// Returns `true` if the effective UID is 0 (root).
///
/// # Arguments
///
/// * `euid` - The effective UID to check.
///
/// # Returns
///
/// `true` if the EUID is 0, `false` otherwise.
#[inline]
pub fn is_root(euid: Uid) -> bool {
    euid == ROOT_UID
}
