# 15. CI/CD

### GitHub Actions Workflow

The CI pipeline runs on every push and pull request with the following structure:

```
┌─────────────────────────────────────────────────────┐
│                      Push / PR                       │
└──────────────────────┬──────────────────────────────┘
                       │
          ┌────────────┴─────────────┐
          ▼                          ▼
    ┌──────────┐               ┌──────────┐
    │  lint    │               │  lint    │
    │ (kernel) │               │(userspace│
    └────┬─────┘               └────┬─────┘
         │  cargo check              │  cargo check
         │  cargo clippy             │  cargo clippy
         │    -D warnings            │    -D warnings
         └────────────┬─────────────┘
                      │  both must pass
                      ▼
               ┌──────────────┐
               │    build     │
               └──────┬───────┘
                      │
            ┌─────────┴──────────┐
            │                    │
            ▼                    ▼
      Debug kernel          Release kernel
      + initrd TAR          + binary size report
```

### Artifacts

| Artifact | Retention | Description |
|----------|-----------|-------------|
| `kernel-debug` | 90 days | Debug build with full debug info |
| `kernel-release` | 90 days | Release build (optimized) |
| `initrd.tar` | 90 days | User space archive (root filesystem) |

### Release Bundle

On push to `main` or `master`, a versioned release bundle is created:

```
openv-<branch>-<short-sha>.tar.gz
├── kernel          (release binary)
├── initrd.tar      (user filesystem)
└── README.md       (build info)
```

### Binary Size Report

After the release build, a **GitHub Step Summary** is posted with:
- Kernel ELF size (bytes)
- Stripped kernel size
- Section sizes (`text`, `rodata`, `data`, `bss`)
- initrd TAR size

This gives developers a quick view of code size regression between commits.

---
[Back to Index](README.md)
