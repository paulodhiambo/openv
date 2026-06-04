import re

with open('src/trap.rs', 'r') as f:
    content = f.read()

# The main pattern
pattern1 = re.compile(
    r"let proc = crate::posix::process::PROCESS_TABLE\s*\n\s*\.lock\(\)\s*\n\s*\.get\(&crate::posix::process::current_pid\(\)\)\s*\n\s*\.cloned\(\)\s*\n\s*\.unwrap\(\);"
)

# A single-line variant if any
pattern2 = re.compile(
    r"let proc = crate::posix::process::PROCESS_TABLE\.lock\(\)\.get\(&crate::posix::process::current_pid\(\)\)\.cloned\(\)\.unwrap\(\);"
)

replacement = "let proc = get_current_proc_or_esrch!(tf);"

content = pattern1.sub(replacement, content)
content = pattern2.sub(replacement, content)

with open('src/trap.rs', 'w') as f:
    f.write(content)
