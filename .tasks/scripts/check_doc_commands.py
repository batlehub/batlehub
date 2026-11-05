"""Every `batlehub-cli …` command the published docs show parses with the CLI as built.

Each command is run as `<bin> <args> --help`: clap refuses an unknown
subcommand or flag before it reaches `--help`, so a spelling RFC 0040 removed
(`admin …`, `version …`) is caught as not parsing. A command spelled `batlehub …` is the
server binary, which has no such subcommand, and is reported as well.

Only code is read — fenced blocks and inline spans that start with the binary —
so prose such as "the batlehub server" is not a command. Synopsis notation is
reduced first: `[optional]` groups go, `<a|b>` becomes `a`, other `<placeholder>`s
become a dummy value, and shell around the command (`| jq`, `> file`, `&& npm ci`)
is cut. A word written `plan|seed|export` is checked once per alternative. An inline mention of a flag without its value is accepted.

Usage: python3 check_doc_commands.py <batlehub-cli binary> <docs dir>
"""
import pathlib
import re
import shlex
import subprocess
import sys

TOP = (
    "profile registry package owner visibility grant namespace publish download why check me "
    "wait verdict audit authz auth setup mise proxy vsx policy exemption upstream user quota "
    "ip-block server cache import retention banner exposure bundle air-gap flag gdpr sbom "
    "notification java tui completion admin version owners verdicts config"
).split()
# The pages that show old spellings on purpose, and the trees that are not published.
SKIP = ("rfc/", "fr/rfc/", "internal/", ".vitepress/")
COMMAND = re.compile(
    r"\b(batlehub(?:-cli)?) ((?:" + "|".join(map(re.escape, TOP)) + r")(?=[\s`]|$)[^`#\n]*)"
)
UUID = "00000000-0000-0000-0000-000000000000"


def code_of(path):
    """(line number, text) of every fenced line and inline code span."""
    fenced = False
    lines = path.read_text().splitlines()
    i = 0
    while i < len(lines):
        start, line = i + 1, lines[i]
        if line.lstrip().startswith("```"):
            fenced = not fenced
        elif fenced:
            # `\` continuations, joined as a shell would.
            while line.rstrip().endswith("\\") and i + 1 < len(lines):
                i += 1
                line = line.rstrip()[:-1] + " " + lines[i].strip()
            yield start, line
        else:
            for span in re.findall(r"`([^`\n]+)`", line):
                if span.startswith("batlehub"):
                    yield start, span
        i += 1


def args_of(raw):
    text = re.sub(r"\$\([^)]*\)", "X", raw).replace("…", "")
    text = re.split(r"\s(?:\||>|>>|>>>|<<<|&&|;)\s", text + " ")[0]
    while re.search(r"\[[^\[\]]*\]", text):
        text = re.sub(r"\[[^\[\]]*\]", "", text)
    text = re.sub(r"<([^<>|]*)\|[^<>]*>", r"\1", text)
    text = re.sub(r"<[^<>]*\b(?:uuid|id)>", UUID, text)
    text = re.sub(r"<[^<>]*>", "X", text)
    try:
        args = shlex.split(text)
    except ValueError:
        return []
    # `plan|seed|export`: each alternative is a command of its own.
    for i, a in enumerate(args):
        if "|" in a:
            return [args[:i] + [alt] + args[i + 1:] for alt in a.split("|") if alt not in ("", "...")]
    return [args]


def main():
    binary, docs = sys.argv[1], pathlib.Path(sys.argv[2])
    problems = 0
    for md in sorted(docs.rglob("*.md")):
        rel = md.relative_to(docs).as_posix()
        if rel.startswith(SKIP):
            continue
        for lineno, code in code_of(md):
            for m in COMMAND.finditer(code):
                name, raw = m.group(1), m.group(2).strip()
                where = f"docs/{rel}:{lineno}"
                if name == "batlehub":
                    print(f"{where}: `batlehub {raw}` runs the server binary; the CLI is batlehub-cli")
                    problems += 1
                for args in args_of(raw):
                    shown = " ".join(args)
                    r = subprocess.run([binary, *args, "--help"], capture_output=True, text=True)
                    if r.returncode != 0:
                        error = (r.stderr.strip().splitlines() or ["?"])[0]
                        if "a value is required for" in error and "but none was supplied" in error:
                            continue
                        print(f"{where}: `batlehub-cli {shown}` does not parse: {error}")
                        problems += 1
    if problems:
        print(f"{problems} documented command(s) do not run as written")
        sys.exit(1)
    print("every documented batlehub-cli command parses with the current CLI")


if __name__ == "__main__":
    main()
