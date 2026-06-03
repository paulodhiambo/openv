# Contributing to OpenV

Thank you for your interest in contributing to OpenV! As an operating system kernel written in Rust, this project demands high standards of safety, correctness, and performance.

By participating in this project, you agree to abide by our standards and conventions.

## Project Philosophy

*   **Safety First:** We prioritize memory safety and correctness above all. `no_std` environments are unforgiving; avoid `unsafe` unless strictly necessary and extensively documented.
*   **Performance:** Kernel code is critical. Avoid unnecessary allocations and focus on efficient data structures.
*   **POSIX-Compliant (Aim):** We strive to provide a standard, expected interface for user-space applications.
*   **Simplicity:** Prefer clear, maintainable, and modular code over clever hacks.

## Development Workflow

### Getting Started
1.  **Clone the repository.**
2.  **Ensure the toolchain is installed:** We use a specific Rust nightly toolchain. See `rust-toolchain.toml` for the required version.
3.  **Build:** Use the provided scripts in the `scripts/` directory.

### Pull Request (PR) Process

1.  **Create an Issue:** If you are planning a significant change, please open an issue first to discuss the design.
2.  **Branching:** Create a descriptive branch (e.g., `feat/vfs-abstraction`, `fix/page-table-leak`).
3.  **Commits:** Keep commits atomic and well-documented. Follow conventional commit messages.
4.  **Testing:** All changes MUST be tested.
5.  **Review:** Submit your PR. Maintainers will review the code for compliance with our standards and design goals.

## Coding Standards

### Rust Conventions
*   Follow [Rust RFCs](https://github.com/rust-lang/rfcs) and idiomatic patterns.
*   Run `cargo fmt` before submitting.
*   Run `cargo clippy` and address all warnings.

### `no_std` & Safety
*   **Avoid `unsafe`:** If you must use `unsafe`, it must be accompanied by a `// SAFETY: ...` comment explaining why it is safe and what invariants are being maintained.
*   **Panic Handling:** Keep `panic!` to an absolute minimum in the kernel. Design APIs that handle errors gracefully (e.g., returning `Result`).

### Kernel Specifics
*   **Documentation:** Document all public kernel APIs using `///` doc comments.
*   **Locking:** Follow strict locking orders to avoid deadlocks. Always use scoped locks if possible.

## Testing

OS development requires rigorous testing.
1.  **Unit Tests:** Place tests in the same file or a `tests/` module.
2.  **Integration Tests:** We use QEMU to test functionality. Ensure your changes do not break the booting process or core system calls.
3.  **CI:** All PRs must pass the CI suite (currently enforced via automated build/test runs).

## Reporting Issues

*   Include a clear description of the problem.
*   Provide steps to reproduce (or relevant code snippets).
*   Specify the environment (e.g., QEMU version, RISC-V configuration).

---
*For questions, please reach out via the issue tracker.*
