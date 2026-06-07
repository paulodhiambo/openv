# 13. Security Model

### Discretionary Access Control (DAC)

openv implements Unix-style **Discretionary Access Control**:

- Every file/directory has an owner `uid`, owner `gid`, and a 9-bit `mode`.
- `check_access(vnode, proc, flags)` compares `proc.euid`/`proc.egid` against the file's ownership to select the correct permission bits.
- **Root (`uid=0`) bypasses all access checks** — this is checked first in `check_access`.

### Credential Propagation

```
Process credentials flow:
  fork():  child inherits uid, gid, euid, egid from parent
  exec():  uid/gid unchanged; euid set to file owner if S_ISUID is set
  setuid(): POSIX semantics (root can set any; non-root restricted)
```

### sudo / Privilege Escalation

```
sudo workflow:
  1. User runs sudo binary (setuid-root)
  2. sudo calls sys_authenticate(username, password) → uid check
  3. sudo calls sys_can_sudo(uid) → checks sudo group membership
  4. If authorised: sys_setuid(0) → euid = 0 (root)
  5. Executes target command as root
```

### Password Hashing

Passwords are stored as **FNV-1a 64-bit hashes**:

```rust
fn fnv1a_64(data: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;  // FNV offset basis
    for &byte in data {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);  // FNV prime
    }
    hash
}
```

> **⚠️ Security Notice:** FNV-1a is a fast, non-cryptographic hash. It is **not suitable for production password storage**. A production system must use a proper password hashing function such as Argon2id, bcrypt, or scrypt with a random salt. The FNV-1a approach is used in openv v1 as a demo-only placeholder.

### Known Gaps in v1

| Gap | Description |
|-----|-------------|
| No ASLR | User processes always load at fixed addresses (`0x100000000`) |
| No stack canaries | Stack overflows are not detected |
| No SMEP/SMAP | RISC-V does not have these x86 mitigations; `PTE_U` on kernel pages is absent |
| Signals | Only Ctrl-C (`exit(130)`) is handled; no full POSIX signal delivery |
| FNV-1a passwords | Not suitable for production (see above) |
| No seccomp | No syscall filtering per-process |

---
[Back to Index](README.md)
