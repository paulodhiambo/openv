import re

with open("src/syscall/fs.rs", "r") as f:
    content = f.read()

# Remove specific functions using a simple regex or string split.
# It's easier to just find the string start and end.
funcs_to_remove = [
    "pub fn sys_open",
    "pub fn sys_getdents",
    "pub fn sys_create",
    "pub fn sys_mkdir",
    "pub fn sys_unlink",
    "pub fn sys_rename",
    "pub fn sys_chdir",
    "pub fn sys_getcwd",
    "#[repr(C)]\n#[derive(Debug, Clone, Copy)]\npub struct PosixStat",
    "fn fill_posix_stat",
    "pub fn sys_stat",
    "pub fn sys_fstat"
]

for func in funcs_to_remove:
    idx = content.find(func)
    if idx != -1:
        # Find next blank line or pub fn
        end_idx = content.find("\npub fn ", idx + 10)
        if end_idx == -1:
            end_idx = len(content)
        content = content[:idx] + content[end_idx:]

with open("src/syscall/fs.rs", "w") as f:
    f.write(content)

print("Done")
