import re

with open("src/trap.rs") as f:
    code = f.read()

# We won't fully automate it, but we can extract the arms.
# Actually, it might be easier to just manually write the files 
# since we only have ~40 syscalls and they need to be wrapped in functions.
