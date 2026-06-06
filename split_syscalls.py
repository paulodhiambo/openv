import re
import os

with open("src/trap.rs", "r") as f:
    lines = f.readlines()

out_dir = "src/syscall"
os.makedirs(out_dir, exist_ok=True)

# We will just write a message indicating this approach is too risky without a real parser,
# actually I can just do it using manual tools.
