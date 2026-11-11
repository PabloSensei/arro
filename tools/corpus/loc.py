"""Count Java/Kotlin code lines: non-blank lines outside of comments. Paths are argv."""
import pathlib
import re
import sys

TOKEN = re.compile(r'"(?:\\.|[^"\\\n])*"|/\*.*?\*/|//[^\n]*', re.S)


def strip(text):
    return TOKEN.sub(lambda m: m.group(0) if m.group(0).startswith('"') else "", text)


for root in sys.argv[1:]:
    root = pathlib.Path(root)
    totals = {}
    for ext in ("java", "kt"):
        files = code = test = 0
        for p in root.rglob(f"*.{ext}"):
            parts = p.relative_to(root).parts
            if any(x in ("target", "build", ".git", ".gradle") for x in parts):
                continue
            n = sum(1 for l in strip(p.read_text("utf-8", "replace")).splitlines() if l.strip())
            files += 1
            code += n
            if "test" in parts or any(x.endswith("-test") or x.endswith("-tests") for x in parts):
                test += n
        totals[ext] = (files, code, test)
    print(root.name, " ".join(f"{e}: files={f} code={c} of_which_test={t}" for e, (f, c, t) in totals.items()))
